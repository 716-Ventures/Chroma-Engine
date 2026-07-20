use anyhow::{Result, bail};

use crate::packet::PacketRef;

const MOVIE_TIMESCALE: u32 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fmp4TrackKind {
    Video,
    Audio,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fmp4Track {
    pub id: u32,
    pub kind: Fmp4TrackKind,
    pub timescale: u32,
    pub default_sample_duration: u32,
    pub default_sample_size: u32,
    pub default_sample_flags: u32,
    pub sample_entry: Fmp4SampleEntry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fmp4SampleEntry {
    Avc {
        codec_config: Vec<u8>,
        width: u16,
        height: u16,
    },
    Hevc {
        codec_config: Vec<u8>,
        width: u16,
        height: u16,
    },
    Aac {
        decoder_config: Vec<u8>,
        channel_count: u16,
        sample_rate: u32,
    },
    Ac3 {
        dac3: [u8; 3],
        channel_count: u16,
        sample_rate: u32,
    },
    Eac3 {
        dec3: Vec<u8>,
        channel_count: u16,
        sample_rate: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fmp4Sample {
    pub duration: u32,
    pub size: u32,
    pub flags: u32,
    pub composition_time_offset: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fmp4FragmentTrack {
    pub track_id: u32,
    pub base_decode_time: u64,
    pub samples: Vec<Fmp4Sample>,
    pub payload: Vec<u8>,
}

pub fn init_segment(tracks: &[Fmp4Track]) -> Result<Vec<u8>> {
    if tracks.is_empty() {
        bail!("fMP4 init segment requires at least one track");
    }
    let mut out = Vec::new();
    write_box(&mut out, *b"ftyp", |out| {
        out.extend_from_slice(b"iso6");
        be_u32(out, 1);
        out.extend_from_slice(b"iso6");
        out.extend_from_slice(b"mp41");
        out.extend_from_slice(b"cmfc");
    });
    write_box(&mut out, *b"moov", |out| {
        write_mvhd(out);
        for track in tracks {
            write_trak(out, track);
        }
        write_mvex(out, tracks);
    });
    Ok(out)
}

pub fn media_fragment(sequence_number: u32, tracks: &[Fmp4FragmentTrack]) -> Result<Vec<u8>> {
    if tracks.is_empty() {
        bail!("fMP4 media fragment requires at least one track");
    }
    for track in tracks {
        let sample_bytes: u64 = track
            .samples
            .iter()
            .map(|sample| u64::from(sample.size))
            .sum();
        if sample_bytes != track.payload.len() as u64 {
            bail!(
                "fMP4 track {} sample bytes ({sample_bytes}) do not match payload bytes ({})",
                track.track_id,
                track.payload.len()
            );
        }
    }

    let mut out = Vec::new();
    let mut data_offset_patches = Vec::new();
    write_box(&mut out, *b"moof", |out| {
        write_mfhd(out, sequence_number);
        for track in tracks {
            write_traf(out, track, &mut data_offset_patches);
        }
    });
    let mdat_payload_start = out.len() + 8;
    let mut track_offsets = Vec::with_capacity(tracks.len());
    let mut cursor = mdat_payload_start;
    for track in tracks {
        track_offsets.push(i32::try_from(cursor).unwrap_or(i32::MAX));
        cursor = cursor.saturating_add(track.payload.len());
    }
    write_box(&mut out, *b"mdat", |out| {
        for track in tracks {
            out.extend_from_slice(&track.payload);
        }
    });
    for (patch, data_offset) in data_offset_patches.into_iter().zip(track_offsets) {
        out[patch..patch + 4].copy_from_slice(&data_offset.to_be_bytes());
    }
    Ok(out)
}

pub fn samples_from_packets_with_timescale(
    packets: &[PacketRef],
    timescale: u32,
) -> Vec<Fmp4Sample> {
    packets
        .iter()
        .map(|packet| {
            let duration = rescale_time(
                packet.duration.units,
                packet.duration.scale.units_per_second,
                timescale,
            )
            .max(1)
            .min(u64::from(u32::MAX)) as u32;
            let pts = rescale_time(
                packet.pts.units,
                packet.pts.scale.units_per_second,
                timescale,
            );
            let dts = rescale_time(
                packet.dts.units,
                packet.dts.scale.units_per_second,
                timescale,
            );
            let composition_time_offset = (pts as i128 - dts as i128)
                .clamp(i128::from(i32::MIN), i128::from(i32::MAX))
                as i32;
            Fmp4Sample {
                duration,
                size: packet.size,
                flags: if packet.keyframe {
                    0x0200_0000
                } else {
                    0x0101_0000
                },
                composition_time_offset,
            }
        })
        .collect()
}

pub fn decode_time_for_timescale(packet: &PacketRef, timescale: u32) -> u64 {
    rescale_time(
        packet.dts.units,
        packet.dts.scale.units_per_second,
        timescale,
    )
}

fn write_mvhd(out: &mut Vec<u8>) {
    write_full_box(out, *b"mvhd", 0, 0, |out| {
        be_u32(out, 0);
        be_u32(out, 0);
        be_u32(out, MOVIE_TIMESCALE);
        be_u32(out, 0);
        be_u32(out, 0x0001_0000);
        be_u16(out, 0x0100);
        be_u16(out, 0);
        be_u32(out, 0);
        be_u32(out, 0);
        write_identity_matrix(out);
        for _ in 0..6 {
            be_u32(out, 0);
        }
        be_u32(out, 1);
    });
}

fn write_trak(out: &mut Vec<u8>, track: &Fmp4Track) {
    write_box(out, *b"trak", |out| {
        write_tkhd(out, track);
        write_box(out, *b"mdia", |out| {
            write_mdhd(out, track.timescale);
            write_hdlr(out, track.kind);
            write_box(out, *b"minf", |out| {
                match track.kind {
                    Fmp4TrackKind::Video => write_vmhd(out),
                    Fmp4TrackKind::Audio => write_smhd(out),
                }
                write_dinf(out);
                write_empty_stbl(out, track);
            });
        });
    });
}

fn write_tkhd(out: &mut Vec<u8>, track: &Fmp4Track) {
    write_full_box(out, *b"tkhd", 0, 0x000007, |out| {
        be_u32(out, 0);
        be_u32(out, 0);
        be_u32(out, track.id);
        be_u32(out, 0);
        be_u32(out, 0);
        be_u32(out, 0);
        be_u32(out, 0);
        be_u16(out, 0);
        be_u16(out, 0);
        be_u16(
            out,
            if track.kind == Fmp4TrackKind::Audio {
                0x0100
            } else {
                0
            },
        );
        be_u16(out, 0);
        write_identity_matrix(out);
        match &track.sample_entry {
            Fmp4SampleEntry::Avc { width, height, .. }
            | Fmp4SampleEntry::Hevc { width, height, .. } => {
                be_u32(out, u32::from(*width) << 16);
                be_u32(out, u32::from(*height) << 16);
            }
            Fmp4SampleEntry::Aac { .. }
            | Fmp4SampleEntry::Ac3 { .. }
            | Fmp4SampleEntry::Eac3 { .. } => {
                be_u32(out, 0);
                be_u32(out, 0);
            }
        }
    });
}

fn write_mdhd(out: &mut Vec<u8>, timescale: u32) {
    write_full_box(out, *b"mdhd", 0, 0, |out| {
        be_u32(out, 0);
        be_u32(out, 0);
        be_u32(out, timescale);
        be_u32(out, 0);
        be_u16(out, 0x55c4);
        be_u16(out, 0);
    });
}

fn write_hdlr(out: &mut Vec<u8>, kind: Fmp4TrackKind) {
    write_full_box(out, *b"hdlr", 0, 0, |out| {
        be_u32(out, 0);
        match kind {
            Fmp4TrackKind::Video => out.extend_from_slice(b"vide"),
            Fmp4TrackKind::Audio => out.extend_from_slice(b"soun"),
        }
        be_u32(out, 0);
        be_u32(out, 0);
        be_u32(out, 0);
        out.push(0);
    });
}

fn write_vmhd(out: &mut Vec<u8>) {
    write_full_box(out, *b"vmhd", 0, 1, |out| {
        be_u16(out, 0);
        be_u16(out, 0);
        be_u16(out, 0);
        be_u16(out, 0);
    });
}

fn write_smhd(out: &mut Vec<u8>) {
    write_full_box(out, *b"smhd", 0, 0, |out| {
        be_u16(out, 0);
        be_u16(out, 0);
    });
}

fn write_dinf(out: &mut Vec<u8>) {
    write_box(out, *b"dinf", |out| {
        write_full_box(out, *b"dref", 0, 0, |out| {
            be_u32(out, 1);
            write_full_box(out, *b"url ", 0, 1, |_| {});
        });
    });
}

fn write_empty_stbl(out: &mut Vec<u8>, track: &Fmp4Track) {
    write_box(out, *b"stbl", |out| {
        write_stsd(out, track);
        write_full_box(out, *b"stts", 0, 0, |out| be_u32(out, 0));
        write_full_box(out, *b"stsc", 0, 0, |out| be_u32(out, 0));
        write_full_box(out, *b"stsz", 0, 0, |out| {
            be_u32(out, 0);
            be_u32(out, 0);
        });
        write_full_box(out, *b"stco", 0, 0, |out| be_u32(out, 0));
    });
}

fn write_stsd(out: &mut Vec<u8>, track: &Fmp4Track) {
    write_full_box(out, *b"stsd", 0, 0, |out| {
        be_u32(out, 1);
        match &track.sample_entry {
            Fmp4SampleEntry::Avc {
                codec_config,
                width,
                height,
            } => write_video_sample_entry(out, *b"avc1", *width, *height, *b"avcC", codec_config),
            Fmp4SampleEntry::Hevc {
                codec_config,
                width,
                height,
            } => write_video_sample_entry(out, *b"hvc1", *width, *height, *b"hvcC", codec_config),
            Fmp4SampleEntry::Aac {
                decoder_config,
                channel_count,
                sample_rate,
            } => write_audio_sample_entry(out, *b"mp4a", *channel_count, *sample_rate, |out| {
                write_esds(out, decoder_config)
            }),
            Fmp4SampleEntry::Ac3 {
                dac3,
                channel_count,
                sample_rate,
            } => write_audio_sample_entry(out, *b"ac-3", *channel_count, *sample_rate, |out| {
                write_box(out, *b"dac3", |out| out.extend_from_slice(dac3));
            }),
            Fmp4SampleEntry::Eac3 {
                dec3,
                channel_count,
                sample_rate,
            } => write_audio_sample_entry(out, *b"ec-3", *channel_count, *sample_rate, |out| {
                write_box(out, *b"dec3", |out| out.extend_from_slice(dec3));
            }),
        }
    });
}

fn write_video_sample_entry(
    out: &mut Vec<u8>,
    coding_name: [u8; 4],
    width: u16,
    height: u16,
    config_name: [u8; 4],
    codec_config: &[u8],
) {
    write_box(out, coding_name, |out| {
        out.extend_from_slice(&[0; 6]);
        be_u16(out, 1);
        be_u16(out, 0);
        be_u16(out, 0);
        be_u32(out, 0);
        be_u32(out, 0);
        be_u32(out, 0);
        be_u16(out, width);
        be_u16(out, height);
        be_u32(out, 0x0048_0000);
        be_u32(out, 0x0048_0000);
        be_u32(out, 0);
        be_u16(out, 1);
        out.extend_from_slice(&[0; 32]);
        be_u16(out, 0x0018);
        be_u16(out, 0xffff);
        write_box(out, config_name, |out| out.extend_from_slice(codec_config));
    });
}

fn write_audio_sample_entry<F>(
    out: &mut Vec<u8>,
    coding_name: [u8; 4],
    channel_count: u16,
    sample_rate: u32,
    write_extension: F,
) where
    F: FnOnce(&mut Vec<u8>),
{
    write_box(out, coding_name, |out| {
        out.extend_from_slice(&[0; 6]);
        be_u16(out, 1);
        be_u16(out, 0);
        be_u16(out, 0);
        be_u32(out, 0);
        be_u16(out, channel_count);
        be_u16(out, 16);
        be_u16(out, 0);
        be_u16(out, 0);
        be_u32(out, sample_rate << 16);
        write_extension(out);
    });
}

fn write_esds(out: &mut Vec<u8>, decoder_config: &[u8]) {
    write_full_box(out, *b"esds", 0, 0, |out| {
        write_descriptor(out, 0x03, |out| {
            be_u16(out, 0);
            out.push(0);
            write_descriptor(out, 0x04, |out| {
                out.push(0x40);
                out.push(0x15);
                out.extend_from_slice(&[0, 0, 0]);
                be_u32(out, 0);
                be_u32(out, 0);
                write_descriptor(out, 0x05, |out| out.extend_from_slice(decoder_config));
            });
            write_descriptor(out, 0x06, |out| out.push(2));
        });
    });
}

fn write_descriptor<F>(out: &mut Vec<u8>, tag: u8, write_payload: F)
where
    F: FnOnce(&mut Vec<u8>),
{
    out.push(tag);
    let len_pos = out.len();
    out.extend_from_slice(&[0; 4]);
    let payload_start = out.len();
    write_payload(out);
    let len = out.len() - payload_start;
    write_descriptor_len(&mut out[len_pos..len_pos + 4], len);
}

fn write_descriptor_len(slot: &mut [u8], mut len: usize) {
    let mut bytes = [0u8; 4];
    for i in (0..4).rev() {
        bytes[i] = (len & 0x7f) as u8;
        if i != 3 {
            bytes[i] |= 0x80;
        }
        len >>= 7;
    }
    slot.copy_from_slice(&bytes);
}

fn write_mvex(out: &mut Vec<u8>, tracks: &[Fmp4Track]) {
    write_box(out, *b"mvex", |out| {
        for track in tracks {
            write_full_box(out, *b"trex", 0, 0, |out| {
                be_u32(out, track.id);
                be_u32(out, 1);
                be_u32(out, track.default_sample_duration);
                be_u32(out, track.default_sample_size);
                be_u32(out, track.default_sample_flags);
            });
        }
    });
}

fn write_mfhd(out: &mut Vec<u8>, sequence_number: u32) {
    write_full_box(out, *b"mfhd", 0, 0, |out| be_u32(out, sequence_number));
}

fn write_traf(out: &mut Vec<u8>, track: &Fmp4FragmentTrack, data_offset_patches: &mut Vec<usize>) {
    write_box(out, *b"traf", |out| {
        write_full_box(out, *b"tfhd", 0, 0x020000, |out| {
            be_u32(out, track.track_id)
        });
        write_full_box(out, *b"tfdt", 1, 0, |out| {
            be_u64(out, track.base_decode_time)
        });
        write_full_box(out, *b"trun", 1, 0x000f01, |out| {
            be_u32(out, track.samples.len() as u32);
            data_offset_patches.push(out.len());
            be_i32(out, 0);
            for sample in &track.samples {
                be_u32(out, sample.duration);
                be_u32(out, sample.size);
                be_u32(out, sample.flags);
                be_i32(out, sample.composition_time_offset);
            }
        });
    });
}

fn write_identity_matrix(out: &mut Vec<u8>) {
    be_u32(out, 0x0001_0000);
    be_u32(out, 0);
    be_u32(out, 0);
    be_u32(out, 0);
    be_u32(out, 0x0001_0000);
    be_u32(out, 0);
    be_u32(out, 0);
    be_u32(out, 0);
    be_u32(out, 0x4000_0000);
}

fn write_box<F>(out: &mut Vec<u8>, name: [u8; 4], write_payload: F)
where
    F: FnOnce(&mut Vec<u8>),
{
    let start = out.len();
    be_u32(out, 0);
    out.extend_from_slice(&name);
    write_payload(out);
    let size = u32::try_from(out.len() - start).unwrap_or(u32::MAX);
    out[start..start + 4].copy_from_slice(&size.to_be_bytes());
}

fn write_full_box<F>(out: &mut Vec<u8>, name: [u8; 4], version: u8, flags: u32, write_payload: F)
where
    F: FnOnce(&mut Vec<u8>),
{
    write_box(out, name, |out| {
        out.push(version);
        out.push(((flags >> 16) & 0xff) as u8);
        out.push(((flags >> 8) & 0xff) as u8);
        out.push((flags & 0xff) as u8);
        write_payload(out);
    });
}

fn be_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn be_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn be_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn be_i32(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn rescale_time(units: u64, from_units_per_second: u32, to_units_per_second: u32) -> u64 {
    units.saturating_mul(u64::from(to_units_per_second)) / u64::from(from_units_per_second.max(1))
}

#[cfg(test)]
mod tests {
    use crate::packet::{PacketRef, TimeDelta, TimePoint};

    use super::*;

    #[test]
    fn init_segment_contains_ftyp_moov_and_mvex() {
        let init = init_segment(&[
            Fmp4Track {
                id: 1,
                kind: Fmp4TrackKind::Video,
                timescale: 90_000,
                default_sample_duration: 3_753,
                default_sample_size: 0,
                default_sample_flags: 0x0101_0000,
                sample_entry: Fmp4SampleEntry::Avc {
                    codec_config: vec![1, 100, 0, 31, 0xff, 0xe1, 0, 0],
                    width: 1_920,
                    height: 1_080,
                },
            },
            Fmp4Track {
                id: 2,
                kind: Fmp4TrackKind::Audio,
                timescale: 48_000,
                default_sample_duration: 1_024,
                default_sample_size: 0,
                default_sample_flags: 0x0200_0000,
                sample_entry: Fmp4SampleEntry::Aac {
                    decoder_config: vec![0x11, 0x90],
                    channel_count: 6,
                    sample_rate: 48_000,
                },
            },
        ])
        .expect("init segment");

        assert_eq!(&init[4..8], b"ftyp");
        assert!(contains_box(&init, b"moov"));
        assert!(init.windows(4).any(|w| w == b"mvex"));
        assert!(init.windows(4).any(|w| w == b"avc1"));
        assert!(init.windows(4).any(|w| w == b"avcC"));
        assert!(init.windows(4).any(|w| w == b"mp4a"));
        assert!(init.windows(4).any(|w| w == b"esds"));
        assert_eq!(top_level_boxes(&init), vec![*b"ftyp", *b"moov"]);
    }

    #[test]
    fn init_segment_writes_hevc_sample_description() {
        let init = init_segment(&[Fmp4Track {
            id: 1,
            kind: Fmp4TrackKind::Video,
            timescale: 90_000,
            default_sample_duration: 3_753,
            default_sample_size: 0,
            default_sample_flags: 0x0101_0000,
            sample_entry: Fmp4SampleEntry::Hevc {
                codec_config: vec![
                    1, 1, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf3, 0,
                ],
                width: 3_840,
                height: 2_160,
            },
        }])
        .expect("init segment");

        assert!(init.windows(4).any(|w| w == b"hvc1"));
        assert!(init.windows(4).any(|w| w == b"hvcC"));
        assert!(init.windows(23).any(|w| {
            w == [
                1, 1, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf3, 0,
            ]
        }));
    }

    #[test]
    fn init_segment_writes_ac3_sample_description() {
        let init = init_segment(&[Fmp4Track {
            id: 2,
            kind: Fmp4TrackKind::Audio,
            timescale: 48_000,
            default_sample_duration: 1_536,
            default_sample_size: 0,
            default_sample_flags: 0x0200_0000,
            sample_entry: Fmp4SampleEntry::Ac3 {
                dac3: [0x50, 0x51, 0],
                channel_count: 2,
                sample_rate: 48_000,
            },
        }])
        .expect("init segment");

        assert!(init.windows(4).any(|w| w == b"ac-3"));
        assert!(init.windows(4).any(|w| w == b"dac3"));
        assert!(init.windows(3).any(|w| w == [0x50, 0x51, 0]));
    }

    #[test]
    fn init_segment_writes_eac3_sample_description() {
        let init = init_segment(&[Fmp4Track {
            id: 2,
            kind: Fmp4TrackKind::Audio,
            timescale: 48_000,
            default_sample_duration: 1_536,
            default_sample_size: 0,
            default_sample_flags: 0x0200_0000,
            sample_entry: Fmp4SampleEntry::Eac3 {
                dec3: vec![0x00, 0x10, 0x20, 0x0f, 0x00],
                channel_count: 6,
                sample_rate: 48_000,
            },
        }])
        .expect("init segment");

        assert!(init.windows(4).any(|w| w == b"ec-3"));
        assert!(init.windows(4).any(|w| w == b"dec3"));
        assert!(init.windows(5).any(|w| w == [0x00, 0x10, 0x20, 0x0f, 0x00]));
    }

    #[test]
    fn media_fragment_patches_data_offset_to_mdat_payload() {
        let fragment = media_fragment(
            7,
            &[Fmp4FragmentTrack {
                track_id: 1,
                base_decode_time: 90_000,
                samples: vec![Fmp4Sample {
                    duration: 3_753,
                    size: 4,
                    flags: 0x0200_0000,
                    composition_time_offset: 0,
                }],
                payload: b"test".to_vec(),
            }],
        )
        .expect("media fragment");

        assert_eq!(top_level_boxes(&fragment), vec![*b"moof", *b"mdat"]);
        let mdat_offset = find_top_level_box(&fragment, b"mdat").expect("mdat") + 8;
        assert!(
            fragment
                .windows(4)
                .any(|w| w == (mdat_offset as i32).to_be_bytes())
        );
        assert!(fragment.ends_with(b"test"));
    }

    #[test]
    fn media_fragment_patches_each_track_to_its_payload() {
        let fragment = media_fragment(
            3,
            &[
                Fmp4FragmentTrack {
                    track_id: 1,
                    base_decode_time: 0,
                    samples: vec![Fmp4Sample {
                        duration: 1_000,
                        size: 5,
                        flags: 0x0200_0000,
                        composition_time_offset: 0,
                    }],
                    payload: b"video".to_vec(),
                },
                Fmp4FragmentTrack {
                    track_id: 2,
                    base_decode_time: 0,
                    samples: vec![Fmp4Sample {
                        duration: 1_024,
                        size: 5,
                        flags: 0x0200_0000,
                        composition_time_offset: 0,
                    }],
                    payload: b"audio".to_vec(),
                },
            ],
        )
        .expect("media fragment");

        let mdat_payload = find_top_level_box(&fragment, b"mdat").expect("mdat") + 8;
        let audio_payload = mdat_payload + 5;
        assert!(
            fragment
                .windows(4)
                .any(|w| w == (mdat_payload as i32).to_be_bytes())
        );
        assert!(
            fragment
                .windows(4)
                .any(|w| w == (audio_payload as i32).to_be_bytes())
        );
        assert!(fragment.ends_with(b"videoaudio"));
    }

    #[test]
    fn media_fragment_inspection_validates_timing_without_external_tools() {
        let packets = vec![
            PacketRef {
                source_offset: 0,
                size: 3,
                pts: TimePoint::millis(1_000),
                dts: TimePoint::millis(900),
                duration: TimeDelta::millis(40),
                keyframe: true,
            },
            PacketRef {
                source_offset: 3,
                size: 2,
                pts: TimePoint::millis(1_040),
                dts: TimePoint::millis(940),
                duration: TimeDelta::millis(40),
                keyframe: false,
            },
        ];
        let samples = samples_from_packets_with_timescale(&packets, 90_000);
        let base_decode_time = decode_time_for_timescale(&packets[0], 90_000);
        let fragment = media_fragment(
            11,
            &[Fmp4FragmentTrack {
                track_id: 1,
                base_decode_time,
                samples,
                payload: vec![0xaa, 0xbb, 0xcc, 0xdd, 0xee],
            }],
        )
        .expect("media fragment");

        let moof = top_level_box_payload(&fragment, b"moof").expect("moof");
        let traf = child_box_payload(moof, b"traf").expect("traf");
        let tfdt = child_box_payload(traf, b"tfdt").expect("tfdt");
        let trun = child_box_payload(traf, b"trun").expect("trun");
        let mdat_offset = find_top_level_box(&fragment, b"mdat").expect("mdat");

        assert_eq!(tfdt_base_decode_time(tfdt), Some(81_000));
        let inspected = inspect_trun(trun).expect("trun inspection");
        assert_eq!(inspected.data_offset, (mdat_offset + 8) as i32);
        assert_eq!(
            inspected.samples,
            vec![
                InspectedSample {
                    duration: 3_600,
                    size: 3,
                    flags: 0x0200_0000,
                    composition_time_offset: 9_000,
                },
                InspectedSample {
                    duration: 3_600,
                    size: 2,
                    flags: 0x0101_0000,
                    composition_time_offset: 9_000,
                },
            ]
        );
        assert_eq!(
            inspected
                .samples
                .iter()
                .map(|sample| sample.size)
                .sum::<u32>(),
            5
        );
        assert_eq!(
            &fragment[mdat_offset + 8..],
            &[0xaa, 0xbb, 0xcc, 0xdd, 0xee]
        );
    }

    #[test]
    fn media_fragment_rejects_payload_size_mismatch() {
        let err = media_fragment(
            1,
            &[Fmp4FragmentTrack {
                track_id: 1,
                base_decode_time: 0,
                samples: vec![Fmp4Sample {
                    duration: 1,
                    size: 4,
                    flags: 0,
                    composition_time_offset: 0,
                }],
                payload: b"too-long".to_vec(),
            }],
        )
        .expect_err("reject inconsistent fragment");

        assert!(err.to_string().contains("sample bytes"));
    }

    fn top_level_boxes(bytes: &[u8]) -> Vec<[u8; 4]> {
        let mut out = Vec::new();
        let mut offset = 0;
        while offset + 8 <= bytes.len() {
            let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            out.push(bytes[offset + 4..offset + 8].try_into().unwrap());
            offset += size;
        }
        out
    }

    fn find_top_level_box(bytes: &[u8], name: &[u8; 4]) -> Option<usize> {
        let mut offset = 0;
        while offset + 8 <= bytes.len() {
            let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().ok()?) as usize;
            if &bytes[offset + 4..offset + 8] == name {
                return Some(offset);
            }
            offset += size;
        }
        None
    }

    fn top_level_box_payload<'a>(bytes: &'a [u8], name: &[u8; 4]) -> Option<&'a [u8]> {
        let offset = find_top_level_box(bytes, name)?;
        box_payload_at(bytes, offset)
    }

    fn child_box_payload<'a>(bytes: &'a [u8], name: &[u8; 4]) -> Option<&'a [u8]> {
        let mut offset = 0;
        while offset + 8 <= bytes.len() {
            let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().ok()?) as usize;
            if size < 8 || offset + size > bytes.len() {
                return None;
            }
            if &bytes[offset + 4..offset + 8] == name {
                return Some(&bytes[offset + 8..offset + size]);
            }
            offset += size;
        }
        None
    }

    fn box_payload_at(bytes: &[u8], offset: usize) -> Option<&[u8]> {
        let size = u32::from_be_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?) as usize;
        if size < 8 || offset + size > bytes.len() {
            return None;
        }
        Some(&bytes[offset + 8..offset + size])
    }

    fn tfdt_base_decode_time(payload: &[u8]) -> Option<u64> {
        if payload.len() < 12 || payload[0] != 1 {
            return None;
        }
        Some(u64::from_be_bytes(payload[4..12].try_into().ok()?))
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct InspectedTrun {
        data_offset: i32,
        samples: Vec<InspectedSample>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct InspectedSample {
        duration: u32,
        size: u32,
        flags: u32,
        composition_time_offset: i32,
    }

    fn inspect_trun(payload: &[u8]) -> Option<InspectedTrun> {
        if payload.len() < 12 || payload[0] != 1 {
            return None;
        }
        let flags = u32::from_be_bytes([0, payload[1], payload[2], payload[3]]);
        if flags != 0x000f01 {
            return None;
        }
        let sample_count = u32::from_be_bytes(payload[4..8].try_into().ok()?) as usize;
        let data_offset = i32::from_be_bytes(payload[8..12].try_into().ok()?);
        let mut offset = 12_usize;
        let mut samples = Vec::with_capacity(sample_count);
        for _ in 0..sample_count {
            let end = offset.checked_add(16)?;
            if end > payload.len() {
                return None;
            }
            samples.push(InspectedSample {
                duration: u32::from_be_bytes(payload[offset..offset + 4].try_into().ok()?),
                size: u32::from_be_bytes(payload[offset + 4..offset + 8].try_into().ok()?),
                flags: u32::from_be_bytes(payload[offset + 8..offset + 12].try_into().ok()?),
                composition_time_offset: i32::from_be_bytes(
                    payload[offset + 12..offset + 16].try_into().ok()?,
                ),
            });
            offset = end;
        }
        (offset == payload.len()).then_some(InspectedTrun {
            data_offset,
            samples,
        })
    }

    fn contains_box(bytes: &[u8], name: &[u8; 4]) -> bool {
        bytes.windows(4).any(|w| w == name)
    }
}
