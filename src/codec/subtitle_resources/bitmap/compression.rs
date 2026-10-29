//! Matroska ContentEncodings shared by text and bitmap subtitle extraction.
//! A compact metadata view retains Tracks, including these encoding headers.
use crate::source::{
    MediaSource,
    probe_reader::{Element, element_from_header},
};
use anyhow::{Context, Result, bail};
use flate2::{Decompress, FlushDecompress, Status};
use std::collections::{BTreeMap, BTreeSet};

const MAX_COMPRESSED: usize = 8 * 1024 * 1024;
const MAX_EXPANDED: usize = 64 * 1024 * 1024;
const MAX_SETTINGS: usize = 64 * 1024;
#[derive(Clone, Debug)]
enum Algorithm {
    Zlib,
    Header(Vec<u8>),
}
#[derive(Clone, Debug)]
struct Encoding {
    order: u64,
    scope: u64,
    algorithm: Algorithm,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct TrackCompression {
    encodings: Vec<Encoding>,
    error: Option<String>,
}
impl TrackCompression {
    /// Restore a frame (scope 1) or CodecPrivate (scope 2), reversing the
    /// declared encoding order. Every intermediate and final buffer is bounded.
    pub(crate) fn decode(&self, bytes: &[u8], scope: u64, limit: usize) -> Result<Vec<u8>> {
        if let Some(error) = &self.error {
            bail!("{error}");
        }
        let limit = limit.min(MAX_EXPANDED);
        if bytes.len() > MAX_COMPRESSED || limit == 0 {
            bail!("Compressed subtitle input exceeds limit");
        }
        let mut output = bytes.to_vec();
        if output.len() > limit {
            bail!("Subtitle data exceeds expanded size limit");
        }
        for encoding in self
            .encodings
            .iter()
            .rev()
            .filter(|encoding| encoding.scope & scope != 0)
        {
            output = match &encoding.algorithm {
                Algorithm::Header(header) => {
                    let size = header
                        .len()
                        .checked_add(output.len())
                        .context("Subtitle header size overflow")?;
                    if size > limit {
                        bail!("Restored subtitle header exceeds size limit");
                    }
                    let mut restored = Vec::new();
                    restored.try_reserve_exact(size)?;
                    restored.extend(header);
                    restored.extend(&output);
                    restored
                }
                Algorithm::Zlib => inflate(&output, limit)?,
            };
        }
        Ok(output)
    }
}

fn inflate(input: &[u8], limit: usize) -> Result<Vec<u8>> {
    let mut decoder = Decompress::new(true);
    let mut output = Vec::new();
    let mut scratch = [0u8; 8192];
    loop {
        let before_in = decoder.total_in();
        let before_out = decoder.total_out();
        let status = decoder.decompress(
            &input[usize::try_from(before_in)?..],
            &mut scratch,
            FlushDecompress::Finish,
        )?;
        let written = usize::try_from(decoder.total_out() - before_out)?;
        let next = output
            .len()
            .checked_add(written)
            .context("Expanded subtitle size overflow")?;
        if next > limit {
            bail!("Expanded subtitle exceeds size limit");
        }
        if next > output.capacity() {
            let capacity = output.capacity().saturating_mul(2).max(next).min(limit);
            output.try_reserve_exact(capacity - output.len())?;
        }
        output.extend_from_slice(&scratch[..written]);
        if status == Status::StreamEnd {
            if decoder.total_in() != input.len() as u64 {
                bail!("Trailing data in compressed subtitle");
            }
            return Ok(output);
        }
        if decoder.total_in() == before_in && decoder.total_out() == before_out {
            bail!("Truncated compressed subtitle stream");
        }
    }
}

pub(crate) fn parse(source: &MediaSource) -> Result<BTreeMap<u64, TrackCompression>> {
    source.check_work()?;
    let mut work = 0usize;
    parse_bytes(source.as_ref(), &mut || {
        source.check_work()?;
        work += 1;
        if work > 32768 {
            bail!("Subtitle encoding metadata work limit exceeded");
        }
        Ok(())
    })
}
fn parse_bytes(
    bytes: &[u8],
    check: &mut impl FnMut() -> Result<()>,
) -> Result<BTreeMap<u64, TrackCompression>> {
    let segment = children(bytes, 0, bytes.len(), check)?
        .into_iter()
        .find(|element| element.id == 0x1853_8067)
        .context("Missing subtitle metadata segment")?;
    let tracks = children(bytes, segment.payload as usize, segment.end as usize, check)?
        .into_iter()
        .find(|element| element.id == 0x1654_ae6b);
    let mut result = BTreeMap::new();
    let Some(tracks) = tracks else {
        return Ok(result);
    };
    for track in children(bytes, tracks.payload as usize, tracks.end as usize, check)?
        .into_iter()
        .filter(|element| element.id == 0xae)
    {
        let fields = children(bytes, track.payload as usize, track.end as usize, check)?;
        let number = fields
            .iter()
            .find(|field| field.id == 0xd7)
            .map(|field| uint(bytes, field))
            .transpose()?;
        let Some(number) = number else { continue };
        if let Some(encodings) = fields.iter().find(|field| field.id == 0x6d80) {
            let parsed = if fields.iter().filter(|field| field.id == 0x6d80).count() != 1 {
                Err(anyhow::anyhow!("Repeated subtitle ContentEncodings"))
            } else {
                parse_encodings(bytes, encodings, check)
            };
            result.insert(
                number,
                match parsed {
                    Ok(encodings) => TrackCompression {
                        encodings,
                        error: None,
                    },
                    Err(error) => TrackCompression {
                        encodings: Vec::new(),
                        error: Some(error.to_string()),
                    },
                },
            );
        }
    }
    Ok(result)
}
fn parse_encodings(
    bytes: &[u8],
    element: &Element,
    check: &mut impl FnMut() -> Result<()>,
) -> Result<Vec<Encoding>> {
    let mut result = Vec::new();
    let mut orders = BTreeSet::new();
    for encoding in children(bytes, element.payload as usize, element.end as usize, check)?
        .into_iter()
        .filter(|element| element.id == 0x6240)
    {
        if result.len() >= 4 {
            bail!("Subtitle content encoding layer limit exceeded");
        }
        let fields = children(
            bytes,
            encoding.payload as usize,
            encoding.end as usize,
            check,
        )?;
        for id in [0x5031, 0x5032, 0x5033, 0x5034] {
            if fields.iter().filter(|field| field.id == id).count() > 1 {
                bail!("Repeated subtitle content encoding field");
            }
        }
        let value = |id, default| -> Result<u64> {
            fields
                .iter()
                .find(|field| field.id == id)
                .map(|field| uint(bytes, field))
                .transpose()
                .map(|value| value.unwrap_or(default))
        };
        let order = value(0x5031, 0)?;
        let scope = value(0x5032, 1)?;
        let kind = value(0x5033, 0)?;
        if !orders.insert(order) {
            bail!("Repeated subtitle content encoding order");
        }
        if kind != 0 || scope == 0 || scope & !3 != 0 {
            bail!("Unsupported subtitle encryption or encoding scope");
        }
        let mut algorithm = 0;
        let mut settings = Vec::new();
        if let Some(compression) = fields.iter().find(|field| field.id == 0x5034) {
            let children = children(
                bytes,
                compression.payload as usize,
                compression.end as usize,
                check,
            )?;
            for id in [0x4254, 0x4255] {
                if children.iter().filter(|field| field.id == id).count() > 1 {
                    bail!("Repeated subtitle compression setting");
                }
            }
            for field in children {
                if field.id == 0x4254 {
                    algorithm = uint(bytes, &field)?;
                }
                if field.id == 0x4255 {
                    let raw = payload(bytes, &field)?;
                    if raw.len() > MAX_SETTINGS {
                        bail!("Subtitle compression settings exceed limit");
                    }
                    settings = raw.to_vec();
                }
            }
        }
        let algorithm = match algorithm {
            0 if settings.is_empty() => Algorithm::Zlib,
            3 => Algorithm::Header(settings),
            _ => bail!("Unsupported subtitle content compression algorithm"),
        };
        result.push(Encoding {
            order,
            scope,
            algorithm,
        });
    }
    result.sort_by_key(|encoding| encoding.order);
    Ok(result)
}
fn children(
    bytes: &[u8],
    start: usize,
    end: usize,
    check: &mut impl FnMut() -> Result<()>,
) -> Result<Vec<Element>> {
    if start > end || end > bytes.len() {
        bail!("Subtitle encoding metadata outside source");
    }
    let mut elements = Vec::new();
    let mut offset = start;
    while offset < end {
        check()?;
        if elements.len() >= 4096 {
            bail!("Subtitle encoding metadata element limit exceeded");
        }
        let header = &bytes[offset..end.min(offset.saturating_add(12))];
        let element = element_from_header(header, offset as u64, end as u64)?;
        if element.end <= offset as u64 {
            bail!("Subtitle encoding metadata made no progress");
        }
        offset = usize::try_from(element.end)?;
        elements.push(element);
    }
    Ok(elements)
}
fn payload<'a>(bytes: &'a [u8], element: &Element) -> Result<&'a [u8]> {
    bytes
        .get(usize::try_from(element.payload)?..usize::try_from(element.end)?)
        .context("Subtitle encoding payload outside source")
}
fn uint(bytes: &[u8], element: &Element) -> Result<u64> {
    let raw = payload(bytes, element)?;
    if raw.is_empty() || raw.len() > 8 {
        bail!("Invalid subtitle encoding integer");
    }
    Ok(raw
        .iter()
        .fold(0, |value, byte| value << 8 | u64::from(*byte)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn zlib(bytes: &[u8]) -> Vec<u8> {
        let mut writer =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        writer.write_all(bytes).unwrap();
        writer.finish().unwrap()
    }
    fn element(id: u32, body: &[u8]) -> Vec<u8> {
        let id = id.to_be_bytes();
        let first = id.iter().position(|v| *v != 0).unwrap();
        let mut bytes = id[first..].to_vec();
        let n = (1..=8)
            .find(|n| (body.len() as u64) < ((1u64 << (7 * n)) - 1))
            .unwrap();
        let size = body.len() as u64 | (1u64 << (7 * n));
        bytes.extend_from_slice(&size.to_be_bytes()[8 - n..]);
        bytes.extend(body);
        bytes
    }
    fn entry(order: u8, scope: u8, algorithm: u8, settings: &[u8]) -> Vec<u8> {
        let mut compression = element(0x4254, &[algorithm]);
        compression.extend(element(0x4255, settings));
        let mut data = element(0x5031, &[order]);
        data.extend(element(0x5032, &[scope]));
        data.extend(element(0x5034, &compression));
        element(0x6240, &data)
    }
    fn metadata(encodings: &[u8]) -> Vec<u8> {
        let mut track = element(0xd7, &[4]);
        track.extend(element(0x6d80, encodings));
        let mut bytes = element(0x1a45_dfa3, &[]);
        bytes.extend(element(
            0x1853_8067,
            &element(0x1654_ae6b, &element(0xae, &track)),
        ));
        bytes
    }
    #[test]
    fn zlib_and_header_stripping_restore_frames_in_reverse_order_and_honor_scope() {
        let mut settings = entry(0, 1, 3, b"header");
        settings.extend(entry(1, 1, 0, b""));
        let map = parse_bytes(&metadata(&settings), &mut || Ok(())).unwrap();
        let decoder = &map[&4];
        assert_eq!(
            decoder.decode(&zlib(b"body"), 1, 64).unwrap(),
            b"headerbody"
        );
        assert_eq!(decoder.decode(b"private", 2, 64).unwrap(), b"private");
        let map = parse_bytes(&metadata(&entry(0, 3, 0, b"")), &mut || Ok(())).unwrap();
        assert_eq!(
            map[&4].decode(&zlib(b"private"), 2, 64).unwrap(),
            b"private"
        );
    }
    #[test]
    fn encoding_errors_are_isolated_to_track_and_decompression_bombs_trailing_data_are_rejected() {
        for entry in [entry(0, 1, 1, b""), entry(0, 4, 0, b""), {
            let mut x = entry(0, 1, 0, b"");
            x.extend(entry(0, 1, 0, b""));
            x
        }] {
            let map = parse_bytes(&metadata(&entry), &mut || Ok(())).unwrap();
            assert!(map[&4].decode(b"data", 1, 32).is_err());
        }
        let map = parse_bytes(&metadata(&entry(0, 1, 0, b"")), &mut || Ok(())).unwrap();
        assert!(map[&4].decode(&zlib(&vec![0; 4096]), 1, 32).is_err());
        let mut trailing = zlib(b"data");
        trailing.push(0);
        assert!(map[&4].decode(&trailing, 1, 32).is_err());
        assert!(map[&4].decode(&[0, 1, 2], 1, 32).is_err());
        assert!(map[&4].decode(&[], 1, 32).is_err());
        let complete = zlib(b"checksum");
        for end in 0..complete.len() {
            assert!(map[&4].decode(&complete[..end], 1, 64).is_err());
        }
    }
    #[test]
    fn compression_metadata_bounds_and_cancellation_are_preserved() {
        let mut bytes = metadata(&entry(0, 1, 3, b"a"));
        bytes.pop();
        assert!(parse_bytes(&bytes, &mut || Ok(())).is_err());
        let mut visits = 0;
        assert!(
            parse_bytes(&metadata(&entry(0, 1, 0, b"")), &mut || {
                visits += 1;
                if visits > 2 {
                    bail!("cancelled");
                }
                Ok(())
            })
            .is_err()
        );
        let map = parse_bytes(&metadata(&entry(0, 1, 3, b"long header")), &mut || Ok(())).unwrap();
        assert!(map[&4].decode(b"body", 1, 4).is_err());
    }
}
