//! Bounded packet-copy bridge for AVC/HEVC + AAC live MPEG-TS segments.
use crate::fmp4::{Fmp4FragmentTrack, Fmp4Sample, Fmp4SampleEntry, Fmp4Track, Fmp4TrackKind};
use crate::transcode::{build_avc_decoder_config, build_hevc_decoder_config};
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;

const WRAP: u64 = 1 << 33;

struct Pes {
    pts: u64,
    dts: u64,
    bytes: Vec<u8>,
}

fn timestamp(bytes: &[u8]) -> Result<u64> {
    ensure!(
        bytes.len() >= 5 && bytes[0] & 1 == 1 && bytes[2] & 1 == 1 && bytes[4] & 1 == 1,
        "invalid PES timestamp"
    );
    Ok((u64::from((bytes[0] >> 1) & 7) << 30)
        | (u64::from(bytes[1]) << 22)
        | (u64::from(bytes[2] >> 1) << 15)
        | (u64::from(bytes[3]) << 7)
        | u64::from(bytes[4] >> 1))
}

fn near_clock(value: u64, anchor: u64) -> u64 {
    let epoch = anchor / WRAP * WRAP;
    let candidate = epoch + value;
    if candidate.saturating_add(WRAP / 2) < anchor {
        candidate + WRAP
    } else if candidate > anchor.saturating_add(WRAP / 2) && candidate >= WRAP {
        candidate - WRAP
    } else {
        candidate
    }
}

fn pes(bytes: &[u8], clock: u64) -> Result<Pes> {
    ensure!(
        bytes.len() >= 14 && bytes.starts_with(&[0, 0, 1]),
        "truncated PES"
    );
    let length = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
    let end = if length == 0 { bytes.len() } else { 6 + length };
    let start = 9 + usize::from(bytes[8]);
    ensure!(start <= end && end <= bytes.len(), "incomplete PES payload");
    ensure!(bytes[7] & 0x80 != 0, "PES lacks presentation timestamp");
    let pts = timestamp(&bytes[9..])?;
    let dts = if bytes[7] & 0x40 != 0 {
        ensure!(bytes[8] >= 10, "truncated PES decode timestamp");
        timestamp(&bytes[14..])?
    } else {
        pts
    };
    let dts = near_clock(dts, clock);
    let pts = near_clock(pts, dts);
    Ok(Pes {
        pts,
        dts,
        bytes: bytes[start..end].to_vec(),
    })
}

fn demux(bytes: &[u8], clock: u64) -> Result<(Vec<Pes>, Vec<Pes>)> {
    ensure!(
        !bytes.is_empty() && bytes.len().is_multiple_of(188),
        "invalid TS packet alignment"
    );
    let mut pending: BTreeMap<u16, (u8, u8, Vec<u8>)> = BTreeMap::new();
    let mut video = Vec::new();
    let mut audio = Vec::new();
    for packet in bytes.as_chunks::<188>().0 {
        ensure!(
            packet[0] == 0x47 && packet[1] & 0x80 == 0,
            "corrupt TS packet"
        );
        if packet[3] & 0x10 == 0 {
            continue;
        }
        ensure!(packet[3] & 0xc0 == 0, "encrypted TS is unsupported");
        let pid = (u16::from(packet[1] & 31) << 8) | u16::from(packet[2]);
        let offset = if packet[3] & 0x20 != 0 {
            5 + usize::from(packet[4])
        } else {
            4
        };
        ensure!(offset <= 188, "invalid TS adaptation field");
        let payload = &packet[offset..];
        let cc = packet[3] & 15;
        if packet[1] & 0x40 != 0 {
            if let Some((kind, _, old)) = pending.remove(&pid) {
                let parsed = pes(&old, clock)?;
                if kind == 0xe0 {
                    video.push(parsed)
                } else {
                    audio.push(parsed)
                }
            }
            if payload.len() >= 6 && payload.starts_with(&[0, 0, 1]) {
                let kind = payload[3] & 0xe0;
                if kind == 0xe0 || kind == 0xc0 {
                    pending.insert(pid, (kind, cc, payload.to_vec()));
                }
            }
        } else if let Some((_, previous, buffer)) = pending.get_mut(&pid) {
            if cc == *previous {
                continue;
            }
            ensure!(cc == (*previous + 1) & 15, "missing TS packet");
            *previous = cc;
            ensure!(
                buffer.len() + payload.len() <= 16 * 1024 * 1024,
                "oversized PES"
            );
            buffer.extend_from_slice(payload);
        }
    }
    for (_, (kind, _, bytes)) in pending {
        let parsed = pes(&bytes, clock)?;
        if kind == 0xe0 {
            video.push(parsed)
        } else {
            audio.push(parsed)
        }
    }
    ensure!(
        !video.is_empty() && !audio.is_empty(),
        "live segment lacks video or AAC audio"
    );
    ensure!(
        video.len() <= 4096 && audio.len() <= 4096,
        "too many live PES packets"
    );
    Ok((video, audio))
}

fn nalus(bytes: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut index = 0;
    while index + 3 <= bytes.len() {
        if bytes[index..].starts_with(&[0, 0, 1]) {
            starts.push((index, index + 3));
            index += 3;
        } else if bytes[index..].starts_with(&[0, 0, 0, 1]) {
            starts.push((index, index + 4));
            index += 4;
        } else {
            index += 1
        }
    }
    starts
        .iter()
        .enumerate()
        .filter_map(|(i, (_, start))| {
            let mut end = starts.get(i + 1).map(|p| p.0).unwrap_or(bytes.len());
            while end > *start && bytes[end - 1] == 0 {
                end -= 1;
            }
            (end > *start).then_some(&bytes[*start..end])
        })
        .collect()
}

fn video_track(
    packets: Vec<Pes>,
    width: u16,
    height: u16,
    hevc: bool,
) -> Result<(Fmp4Track, Fmp4FragmentTrack)> {
    let mut sets = Vec::new();
    let mut fragment = Fmp4FragmentTrack {
        track_id: 1,
        base_decode_time: packets[0].dts,
        samples: Vec::new(),
        payload: Vec::new(),
    };
    let default_duration = packets
        .windows(2)
        .filter_map(|p| p[1].dts.checked_sub(p[0].dts))
        .find(|d| *d > 0)
        .context("video has no decode duration")?;
    let mut first_keyframe = false;
    for (index, packet) in packets.iter().enumerate() {
        let units = nalus(&packet.bytes);
        let mut sample = Vec::new();
        let mut keyframe = false;
        let mut has_vcl = false;
        for unit in units {
            let kind = if hevc {
                (unit[0] >> 1) & 63
            } else {
                unit[0] & 31
            };
            let config = if hevc {
                matches!(kind, 32..=34)
            } else {
                matches!(kind, 7 | 8)
            };
            if config && !sets.iter().any(|s: &Vec<u8>| s.as_slice() == unit) {
                sets.push(unit.to_vec());
            }
            keyframe |= if hevc {
                matches!(kind, 16..=21)
            } else {
                kind == 5
            };
            has_vcl |= if hevc {
                kind <= 31
            } else {
                matches!(kind, 1..=5)
            };
            sample.extend_from_slice(&u32::try_from(unit.len())?.to_be_bytes());
            sample.extend_from_slice(unit);
        }
        ensure!(has_vcl, "video PES does not contain an access unit");
        if index == 0 {
            first_keyframe = keyframe;
        }
        let duration = packets
            .get(index + 1)
            .map(|next| next.dts.checked_sub(packet.dts))
            .unwrap_or(Some(default_duration))
            .context("nonmonotonic live video")?;
        ensure!(duration > 0 && duration <= 90000, "invalid video duration");
        fragment.samples.push(Fmp4Sample {
            duration: u32::try_from(duration)?,
            size: u32::try_from(sample.len())?,
            flags: if keyframe { 0x0200_0000 } else { 0x0101_0000 },
            composition_time_offset: i32::try_from(
                i128::from(packet.pts) - i128::from(packet.dts),
            )?,
        });
        fragment.payload.extend(sample);
    }
    ensure!(
        first_keyframe,
        "live segment does not start with a random-access frame"
    );
    let config = if hevc {
        build_hevc_decoder_config(&sets, 4)
    } else {
        build_avc_decoder_config(&sets, 4)
    }
    .context("missing video parameter sets")?;
    let entry = if hevc {
        Fmp4SampleEntry::Hevc {
            codec_config: config,
            width,
            height,
        }
    } else {
        Fmp4SampleEntry::Avc {
            codec_config: config,
            width,
            height,
        }
    };
    Ok((
        Fmp4Track {
            id: 1,
            kind: Fmp4TrackKind::Video,
            timescale: 90000,
            default_sample_duration: 0,
            default_sample_size: 0,
            default_sample_flags: 0x0101_0000,
            sample_entry: entry,
        },
        fragment,
    ))
}

fn audio_track(packets: Vec<Pes>) -> Result<(Fmp4Track, Fmp4FragmentTrack)> {
    let rates = [
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
    ];
    let mut config = None;
    let mut samples = Vec::new();
    let mut payload = Vec::new();
    let mut base = None;
    let mut sample_rate = 0;
    let mut channel_count = 0;
    let mut expected90 = None;
    for packet in packets {
        if let Some(expected) = expected90 {
            ensure!(
                packet.pts.abs_diff(expected) <= 9000,
                "discontinuous AAC timing within segment"
            );
        }
        let mut bytes = packet.bytes.as_slice();
        let mut count = 0;
        while !bytes.is_empty() {
            ensure!(
                bytes.len() >= 7 && bytes[0] == 0xff && bytes[1] & 0xf6 == 0xf0,
                "invalid ADTS header"
            );
            let profile = (bytes[2] >> 6) + 1;
            let rate_index = (bytes[2] >> 2) & 15;
            let channels = ((bytes[2] & 1) << 2) | (bytes[3] >> 6);
            ensure!(
                profile == 2 && channels > 0 && bytes[6] & 3 == 0,
                "unsupported AAC format"
            );
            let rate = *rates
                .get(usize::from(rate_index))
                .context("invalid AAC sample rate")?;
            let asc = vec![
                (profile << 3) | (rate_index >> 1),
                (rate_index << 7) | (channels << 3),
            ];
            if let Some(old) = &config {
                ensure!(*old == asc, "AAC format changed within segment");
            }
            config = Some(asc);
            sample_rate = rate;
            channel_count = u16::from(channels);
            base.get_or_insert(u64::try_from(
                u128::from(packet.pts) * u128::from(rate) / 90000,
            )?);
            let size = (usize::from(bytes[3] & 3) << 11)
                | (usize::from(bytes[4]) << 3)
                | usize::from(bytes[5] >> 5);
            let header = if bytes[1] & 1 != 0 { 7 } else { 9 };
            ensure!(
                size >= header && size <= bytes.len(),
                "truncated ADTS frame"
            );
            payload.extend_from_slice(&bytes[header..size]);
            samples.push(Fmp4Sample {
                duration: 1024,
                size: u32::try_from(size - header)?,
                flags: 0x0200_0000,
                composition_time_offset: 0,
            });
            ensure!(samples.len() <= 32768, "too many AAC frames");
            count += 1;
            bytes = &bytes[size..];
        }
        expected90 = Some(packet.pts + count * 1024 * 90000 / u64::from(sample_rate));
    }
    let config = config.context("no AAC samples")?;
    Ok((
        Fmp4Track {
            id: 2,
            kind: Fmp4TrackKind::Audio,
            timescale: sample_rate,
            default_sample_duration: 1024,
            default_sample_size: 0,
            default_sample_flags: 0x0200_0000,
            sample_entry: Fmp4SampleEntry::Aac {
                decoder_config: config,
                channel_count,
                sample_rate,
            },
        },
        Fmp4FragmentTrack {
            track_id: 2,
            base_decode_time: base.context("no AAC clock")?,
            samples,
            payload,
        },
    ))
}

pub(crate) fn remux(
    bytes: &[u8],
    width: u16,
    height: u16,
    hevc: bool,
    clock_epoch: u64,
) -> Result<(Vec<u8>, Vec<u8>)> {
    ensure!(
        bytes.len() <= 32 * 1024 * 1024
            && width > 0
            && height > 0
            && width <= 8192
            && height <= 8192,
        "invalid live input bounds"
    );
    let clock = clock_epoch
        .checked_mul(90000)
        .context("live clock overflow")?;
    ensure!(
        clock <= u64::MAX - WRAP,
        "live clock exceeds supported range"
    );
    let (video, audio) = demux(bytes, clock)?;
    let (video, video_fragment) = video_track(video, width, height, hevc)?;
    let (audio, audio_fragment) = audio_track(audio)?;
    let audio_timescale = audio.timescale;
    let init = crate::fmp4::init_segment(&[video, audio])?;
    let media = crate::fmp4::interleaved_media_fragment(
        1,
        &[video_fragment, audio_fragment],
        &[90000, audio_timescale],
    )?;
    Ok((init, media))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clock_unwrap_preserves_wrap_boundary() {
        let before = near_clock(WRAP - 90000, WRAP - 90000);
        let after = near_clock(90000, WRAP + 90000);
        assert_eq!(after - before, 180000);
    }
    #[test]
    fn rejects_truncated_or_encrypted_input() {
        assert!(remux(&[0; 187], 1920, 1080, false, 0).is_err());
        let mut packet = [0xff; 188];
        packet[0] = 0x47;
        packet[1] = 0x40;
        packet[3] = 0xd0;
        assert!(demux(&packet, 0).is_err());
    }
    #[test]
    fn annex_b_extracts_mixed_start_codes() {
        assert_eq!(
            nalus(&[0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0]),
            vec![&[0x67, 1, 2][..], &[0x68, 3][..]]
        );
    }
}
