//! Bounded positional views for MP4s whose unrelated track tables exceed the
//! source metadata ceiling. Packet tables are copied only for explicitly selected
//! tracks; their original absolute file offsets and timing data remain unchanged.
use super::{append, read};
use crate::{ResourcePolicy, WorkControl, container::ParseBudget};
use anyhow::{Result, bail};
use std::fs::File;

struct BoxRef {
    kind: [u8; 4],
    offset: u64,
    payload: u64,
    end: u64,
}

struct Reader<'a> {
    file: &'a File,
    policy: &'a ResourcePolicy,
    control: &'a WorkControl,
    budget: ParseBudget,
    selected: &'a [&'a str],
    tracks: [usize; 4],
}

pub(super) fn append_moov(
    file: &File,
    start: u64,
    end: u64,
    output: &mut Vec<u8>,
    policy: &ResourcePolicy,
    control: &WorkControl,
    selected: &[&str],
) -> Result<()> {
    let mut prior_tracks = [0; 4];
    for track in crate::container::mp4::parse_basic_metadata(output).tracks {
        use crate::container::mp4::Mp4TrackKind;
        let index = match track.kind {
            Mp4TrackKind::Video => 0,
            Mp4TrackKind::Audio => 1,
            Mp4TrackKind::Subtitle => 2,
            Mp4TrackKind::Unknown => 3,
        };
        prior_tracks[index] += 1;
    }
    let mut reader = Reader {
        file,
        policy,
        control,
        budget: ParseBudget::new(policy.parse_limits()),
        selected,
        tracks: prior_tracks,
    };
    reader.container(*b"moov", start, end, output, 0)
}

impl Reader<'_> {
    fn header(&mut self, offset: u64, end: u64, depth: usize) -> Result<BoxRef> {
        self.control.check()?;
        if end.saturating_sub(offset) < 8 {
            bail!("truncated MP4 box header");
        }
        let header = read(self.file, offset, (end - offset).min(16) as usize)?;
        let size32 = u32::from_be_bytes(header[..4].try_into()?);
        let (header_len, size) = match size32 {
            0 => (8, end - offset),
            1 if header.len() >= 16 => (16, u64::from_be_bytes(header[8..16].try_into()?)),
            1 => bail!("truncated extended MP4 box header"),
            n => (8, u64::from(n)),
        };
        if size < header_len || size > end - offset {
            bail!("invalid MP4 box size");
        }
        let kind = header[4..8].try_into()?;
        self.budget.visit(depth, kind == *b"trak", 0)?;
        Ok(BoxRef {
            kind,
            offset,
            payload: offset + header_len,
            end: offset + size,
        })
    }

    fn child(
        &mut self,
        start: u64,
        end: u64,
        kind: [u8; 4],
        depth: usize,
    ) -> Result<Option<BoxRef>> {
        let mut offset = start;
        while offset < end {
            let current = self.header(offset, end, depth)?;
            offset = current.end;
            if current.kind == kind {
                return Ok(Some(current));
            }
        }
        Ok(None)
    }

    fn track_id(&mut self, track: &BoxRef, depth: usize) -> Result<String> {
        let mut index = 3;
        if let Some(mdia) = self.child(track.payload, track.end, *b"mdia", depth + 1)?
            && let Some(hdlr) = self.child(mdia.payload, mdia.end, *b"hdlr", depth + 2)?
            && hdlr.end - hdlr.payload >= 12
        {
            let bytes = read(self.file, hdlr.payload + 8, 4)?;
            index = match bytes.as_slice() {
                b"vide" => 0,
                b"soun" => 1,
                b"text" | b"sbtl" | b"subt" | b"clcp" => 2,
                _ => 3,
            };
        }
        let id = format!("{}{}", ['v', 'a', 's', 'x'][index], self.tracks[index]);
        self.tracks[index] += 1;
        Ok(id)
    }

    fn generated(&self, bytes: &[u8], output: &mut Vec<u8>) -> Result<()> {
        let size = output
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| anyhow::anyhow!("metadata size overflow"))?;
        self.policy
            .check("container metadata", size, self.policy.metadata_bytes)?;
        if size > super::MAX_METADATA {
            bail!("metadata memory budget exceeded");
        }
        if output.capacity() < size {
            let capacity = size
                .checked_next_power_of_two()
                .ok_or_else(|| anyhow::anyhow!("metadata capacity overflow"))?
                .min(self.policy.metadata_bytes.min(super::MAX_METADATA));
            output.try_reserve_exact(capacity - output.len())?;
        }
        output.extend_from_slice(bytes);
        Ok(())
    }

    fn container(
        &mut self,
        kind: [u8; 4],
        start: u64,
        end: u64,
        output: &mut Vec<u8>,
        depth: usize,
    ) -> Result<()> {
        let size_offset = output.len();
        self.generated(&[0, 0, 0, 0], output)?;
        self.generated(&kind, output)?;
        let mut offset = start;
        while offset < end {
            let current = self.header(offset, end, depth + 1)?;
            offset = current.end;
            let keep_track = if current.kind == *b"trak" {
                let id = self.track_id(&current, depth + 1)?;
                self.selected.contains(&id.as_str())
            } else {
                false
            };
            if keep_track {
                append(
                    self.file,
                    current.offset,
                    current.end - current.offset,
                    output,
                    self.policy,
                )?;
            } else if matches!(
                &current.kind,
                b"trak" | b"mdia" | b"minf" | b"stbl" | b"udta" | b"edts"
            ) {
                self.container(
                    current.kind,
                    current.payload,
                    current.end,
                    output,
                    depth + 1,
                )?;
            } else if matches!(
                &current.kind,
                b"mvhd" | b"tkhd" | b"mdhd" | b"hdlr" | b"stsd" | b"chpl" | b"elst"
            ) {
                append(
                    self.file,
                    current.offset,
                    current.end - current.offset,
                    output,
                    self.policy,
                )?;
            }
        }
        let size = u32::try_from(output.len() - size_offset)?;
        output[size_offset..size_offset + 4].copy_from_slice(&size.to_be_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(payload);
        bytes
    }

    fn large_multitrack_mp4() -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&atom(b"ftyp", b"isom\0\0\0\0")).unwrap();
        file.write_all(&[0; 8]).unwrap();
        let movie_start = 16;
        for index in 0..17 {
            let kind = if index == 0 { b"vide" } else { b"soun" };
            let codec = if index == 0 { b"avc1" } else { b"mp4a" };
            let mut mdhd = vec![0; 24];
            mdhd[12..16].copy_from_slice(&1000_u32.to_be_bytes());
            mdhd[16..20].copy_from_slice(&7_200_000_u32.to_be_bytes());
            let mut hdlr = vec![0; 12];
            hdlr[8..12].copy_from_slice(kind);
            let mut entry = vec![0; if index == 0 { 78 } else { 28 }];
            if index == 0 {
                entry[24..26].copy_from_slice(&1920_u16.to_be_bytes());
                entry[26..28].copy_from_slice(&1080_u16.to_be_bytes());
            } else {
                entry[16..18].copy_from_slice(&2_u16.to_be_bytes());
                entry[24..28].copy_from_slice(&(48000_u32 << 16).to_be_bytes());
            }
            let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
            stsd.extend(atom(codec, &entry));
            let timing = atom(b"stsd", &stsd);
            let offsets_size = 6 * 1024 * 1024;
            let stbl_size = 8 + timing.len() + offsets_size;
            let minf_size = 8 + stbl_size;
            let mdia_size = 8 + atom(b"mdhd", &mdhd).len() + atom(b"hdlr", &hdlr).len() + minf_size;
            file.write_all(&((8 + mdia_size) as u32).to_be_bytes())
                .unwrap();
            file.write_all(b"trak").unwrap();
            file.write_all(&(mdia_size as u32).to_be_bytes()).unwrap();
            file.write_all(b"mdia").unwrap();
            file.write_all(&atom(b"mdhd", &mdhd)).unwrap();
            file.write_all(&atom(b"hdlr", &hdlr)).unwrap();
            file.write_all(&(minf_size as u32).to_be_bytes()).unwrap();
            file.write_all(b"minf").unwrap();
            file.write_all(&(stbl_size as u32).to_be_bytes()).unwrap();
            file.write_all(b"stbl").unwrap();
            file.write_all(&timing).unwrap();
            file.write_all(&(offsets_size as u32).to_be_bytes())
                .unwrap();
            file.write_all(b"co64").unwrap();
            file.write_all(&[0; 4]).unwrap();
            file.write_all(&(((offsets_size - 16) / 8) as u32).to_be_bytes())
                .unwrap();
            file.seek(SeekFrom::Current((offsets_size - 16) as i64))
                .unwrap();
        }
        let end = file.stream_position().unwrap();
        file.as_file().set_len(end).unwrap();
        file.seek(SeekFrom::Start(movie_start)).unwrap();
        file.write_all(&((end - movie_start) as u32).to_be_bytes())
            .unwrap();
        file.write_all(b"moov").unwrap();
        file
    }

    #[test]
    fn seventeen_track_movie_keeps_bounded_headers_and_loads_only_selected_tables() {
        let file = large_multitrack_mp4();
        let policy = ResourcePolicy::default();
        let (_, headers, compact) =
            super::super::read_metadata_view(file.path(), &policy, &WorkControl::default(), &[])
                .unwrap();
        assert!(compact);
        assert!(headers.len() < 16 * 1024);
        assert!(file.as_file().metadata().unwrap().len() > 100_000_000);
        let metadata = crate::container::mp4::parse_basic_metadata(&headers);
        assert_eq!(metadata.tracks.len(), 17);
        assert_eq!(metadata.tracks[0].width, Some(1920));
        assert_eq!(metadata.tracks[0].codec, "h264");
        assert_eq!(
            metadata
                .tracks
                .iter()
                .filter(|t| t.kind == crate::container::mp4::Mp4TrackKind::Audio)
                .count(),
            16
        );
        let (_, selected, _) = super::super::read_metadata_view(
            file.path(),
            &policy,
            &WorkControl::default(),
            &["a14"],
        )
        .unwrap();
        assert!(selected.len() > 6 * 1024 * 1024 && selected.len() < 7 * 1024 * 1024);
        assert_eq!(
            crate::container::mp4::parse_basic_metadata(&selected)
                .tracks
                .len(),
            17
        );
        assert_eq!(
            selected
                .windows(4)
                .filter(|bytes| *bytes == b"co64")
                .count(),
            1
        );
        let restricted = ResourcePolicy {
            metadata_bytes: 1024 * 1024,
            ..policy
        };
        let error = super::super::read_metadata_view(
            file.path(),
            &restricted,
            &WorkControl::default(),
            &["a14"],
        )
        .unwrap_err();
        assert!(error.to_string().contains("container metadata"));
    }

    #[test]
    fn compact_metadata_preserves_depth_and_track_limits() {
        let mut nested = atom(b"mvhd", &[0; 20]);
        for _ in 0..40 {
            nested = atom(b"stbl", &nested);
        }
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), &nested).unwrap();
        let policy = ResourcePolicy::default();
        let mut output = Vec::new();
        let error = append_moov(
            file.as_file(),
            0,
            nested.len() as u64,
            &mut output,
            &policy,
            &WorkControl::default(),
            &[],
        )
        .unwrap_err();
        assert!(error.to_string().contains("nesting depth"));
        let tracks = atom(b"trak", &[]).repeat(257);
        std::fs::write(file.path(), &tracks).unwrap();
        let error = append_moov(
            file.as_file(),
            0,
            tracks.len() as u64,
            &mut Vec::new(),
            &policy,
            &WorkControl::default(),
            &[],
        )
        .unwrap_err();
        assert!(error.to_string().contains("container tracks"));
    }

    #[test]
    fn compact_metadata_does_not_accept_invalid_nested_box_sizes() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&atom(b"ftyp", b"isom\0\0\0\0")).unwrap();
        file.write_all(&1000_u32.to_be_bytes()).unwrap();
        file.write_all(b"moov").unwrap();
        file.write_all(&2000_u32.to_be_bytes()).unwrap();
        file.write_all(b"trak").unwrap();
        file.as_file().set_len(1016).unwrap();
        let policy = ResourcePolicy {
            metadata_bytes: 512,
            ..Default::default()
        };
        let error =
            super::super::read_metadata_view(file.path(), &policy, &WorkControl::default(), &[])
                .unwrap_err();
        assert!(error.to_string().contains("invalid MP4 box size"));
    }
}
