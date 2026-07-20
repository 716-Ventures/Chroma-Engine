use anyhow::{bail, Result};

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

pub fn media_fragment(
    sequence_number: u32,
    tracks: &[Fmp4FragmentTrack],
    mdat_payload: &[u8],
) -> Result<Vec<u8>> {
    if tracks.is_empty() {
        bail!("fMP4 media fragment requires at least one track");
    }
    let mut out = Vec::new();
    let mut data_offset_patches = Vec::new();
    write_box(&mut out, *b"moof", |out| {
        write_mfhd(out, sequence_number);
        for track in tracks {
            write_traf(out, track, &mut data_offset_patches);
        }
    });
    let mdat_start = out.len();
    write_box(&mut out, *b"mdat", |out| {
        out.extend_from_slice(mdat_payload)
    });
    let data_offset = i32::try_from(mdat_start + 8).unwrap_or(i32::MAX);
    for patch in data_offset_patches {
        out[patch..patch + 4].copy_from_slice(&data_offset.to_be_bytes());
    }
    Ok(out)
}

pub fn samples_from_packets(packets: &[PacketRef]) -> Vec<Fmp4Sample> {
    packets
        .iter()
        .map(|packet| {
            let duration = packet.duration.as_millis().max(1).min(u64::from(u32::MAX)) as u32;
            let pts90 = to_90khz(packet.pts.units, packet.pts.scale.units_per_second);
            let dts90 = to_90khz(packet.dts.units, packet.dts.scale.units_per_second);
            let composition_time_offset = ((pts90 as i128 - dts90 as i128) / 90)
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
                write_empty_stbl(out);
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
        be_u32(out, 0);
        be_u32(out, 0);
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

fn write_empty_stbl(out: &mut Vec<u8>) {
    write_box(out, *b"stbl", |out| {
        write_box(out, *b"stsd", |out| {
            be_u32(out, 0);
            be_u32(out, 0);
        });
        write_full_box(out, *b"stts", 0, 0, |out| be_u32(out, 0));
        write_full_box(out, *b"stsc", 0, 0, |out| be_u32(out, 0));
        write_full_box(out, *b"stsz", 0, 0, |out| {
            be_u32(out, 0);
            be_u32(out, 0);
        });
        write_full_box(out, *b"stco", 0, 0, |out| be_u32(out, 0));
    });
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

fn to_90khz(units: u64, units_per_second: u32) -> u64 {
    units.saturating_mul(90_000) / u64::from(units_per_second.max(1))
}

#[cfg(test)]
mod tests {
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
            },
            Fmp4Track {
                id: 2,
                kind: Fmp4TrackKind::Audio,
                timescale: 48_000,
                default_sample_duration: 1_024,
                default_sample_size: 0,
                default_sample_flags: 0x0200_0000,
            },
        ])
        .expect("init segment");

        assert_eq!(&init[4..8], b"ftyp");
        assert!(contains_box(&init, b"moov"));
        assert!(init.windows(4).any(|w| w == b"mvex"));
        assert_eq!(top_level_boxes(&init), vec![*b"ftyp", *b"moov"]);
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
            }],
            b"test",
        )
        .expect("media fragment");

        assert_eq!(top_level_boxes(&fragment), vec![*b"moof", *b"mdat"]);
        let mdat_offset = find_top_level_box(&fragment, b"mdat").expect("mdat") + 8;
        assert!(fragment
            .windows(4)
            .any(|w| w == &(mdat_offset as i32).to_be_bytes()));
        assert!(fragment.ends_with(b"test"));
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

    fn contains_box(bytes: &[u8], name: &[u8; 4]) -> bool {
        bytes.windows(4).any(|w| w == name)
    }
}
