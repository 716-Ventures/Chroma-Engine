use anyhow::{Result, bail};

use crate::packet::{ChunkSample, PacketRef};
use crate::transcode::{EncodedAudioFrame, EncodedVideoFrame};

mod validation;

#[cfg(any(test, feature = "fuzzing"))]
pub(crate) use validation::validate_media_fragment;

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
    Mp3 {
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
    Flac {
        stream_info: Vec<u8>,
        channel_count: u16,
        sample_rate: u32,
    },
    Alac {
        codec_config: Vec<u8>,
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
    init_segment_with_edits(tracks, &[])
}

/// Track ID, media-time priming in track samples, and valid movie duration in ms.
pub(crate) fn init_segment_with_edits(
    tracks: &[Fmp4Track],
    edits: &[(u32, u32, u64)],
) -> Result<Vec<u8>> {
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
            write_trak(
                out,
                track,
                edits
                    .iter()
                    .find(|edit| edit.0 == track.id)
                    .map(|edit| (edit.1, edit.2)),
            );
        }
        write_mvex(out, tracks);
    });
    Ok(out)
}

fn fragment_header(sequence_number: u32, tracks: &[Fmp4FragmentTrack]) -> Result<Vec<u8>> {
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
        track_offsets.push(i32::try_from(cursor)?);
        cursor = cursor
            .checked_add(track.payload.len())
            .ok_or_else(|| anyhow::anyhow!("fragment size overflow"))?;
    }
    let payload_len = cursor
        .checked_sub(mdat_payload_start)
        .ok_or_else(|| anyhow::anyhow!("invalid fragment layout"))?;
    be_u32(
        &mut out,
        u32::try_from(
            payload_len
                .checked_add(8)
                .ok_or_else(|| anyhow::anyhow!("mdat overflow"))?,
        )?,
    );
    out.extend_from_slice(b"mdat");
    for (patch, data_offset) in data_offset_patches.into_iter().zip(track_offsets) {
        out[patch..patch + 4].copy_from_slice(&data_offset.to_be_bytes());
    }
    validation::validate_fragment_header(&out, payload_len)?;
    Ok(out)
}

#[derive(Debug)]
pub(crate) struct OwnedMediaFragment {
    header: Vec<u8>,
    tracks: Vec<Fmp4FragmentTrack>,
    len: usize,
}
impl OwnedMediaFragment {
    pub(crate) fn new(sequence: u32, tracks: Vec<Fmp4FragmentTrack>, limit: usize) -> Result<Self> {
        let payload = tracks.iter().try_fold(0usize, |sum, track| {
            sum.checked_add(track.payload.len())
                .ok_or_else(|| anyhow::anyhow!("fragment overflow"))
        })?;
        let overhead = tracks.iter().try_fold(256usize, |sum, track| {
            sum.checked_add(track.samples.len().saturating_mul(16).saturating_add(128))
                .ok_or_else(|| anyhow::anyhow!("fragment metadata overflow"))
        })?;
        if payload.saturating_add(overhead) > limit {
            return Err(crate::ResourceError::Exceeded {
                resource: "output fragment",
                requested: payload.saturating_add(overhead) as u64,
                limit: limit as u64,
            }
            .into());
        }
        let header = fragment_header(sequence, &tracks)?;
        let len = header
            .len()
            .checked_add(payload)
            .ok_or_else(|| anyhow::anyhow!("fragment overflow"))?;
        Ok(Self {
            header,
            tracks,
            len,
        })
    }
    pub(crate) fn len(&self) -> usize {
        self.len
    }
    pub(crate) fn publish(&self, output: &std::path::Path) -> std::io::Result<()> {
        let mut parts = Vec::with_capacity(self.tracks.len() + 1);
        parts.push(self.header.as_slice());
        parts.extend(self.tracks.iter().map(|track| track.payload.as_slice()));
        crate::output::publish_parts(output, &parts)
    }
}

pub fn media_fragment(sequence_number: u32, tracks: &[Fmp4FragmentTrack]) -> Result<Vec<u8>> {
    let mut output = fragment_header(sequence_number, tracks)?;
    let payload_len = tracks.iter().try_fold(0usize, |sum, track| {
        sum.checked_add(track.payload.len())
            .ok_or_else(|| anyhow::anyhow!("fragment overflow"))
    })?;
    output.try_reserve_exact(payload_len)?;
    for track in tracks {
        output.extend_from_slice(&track.payload);
    }
    Ok(output)
}

pub fn samples_from_packets_with_timescale(
    packets: &[PacketRef],
    timescale: u32,
) -> Vec<Fmp4Sample> {
    let mut samples = packets
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
        .collect::<Vec<_>>();

    if let Some(min_offset) = samples
        .iter()
        .map(|sample| sample.composition_time_offset)
        .min()
        && min_offset < 0
    {
        for sample in &mut samples {
            sample.composition_time_offset =
                sample.composition_time_offset.saturating_sub(min_offset);
        }
    }

    samples
}

/// Builds one fMP4 fragment track from contiguous encoded audio frames.
#[allow(
    dead_code,
    reason = "native audio encoder backends will use this mux adapter once implemented"
)]
pub fn fragment_track_from_encoded_audio_frames(
    track_id: u32,
    frames: &[EncodedAudioFrame],
) -> Result<Fmp4FragmentTrack> {
    let first = frames
        .first()
        .ok_or_else(|| anyhow::anyhow!("fMP4 audio fragment requires at least one frame"))?;
    let timescale = first.timing.pts.scale;
    let mut expected_sample = first.timing.start_sample;
    let mut payload = Vec::new();
    let mut samples = Vec::with_capacity(frames.len());

    for frame in frames {
        if frame.timing.pts.scale != timescale || frame.timing.duration.scale != timescale {
            bail!("encoded audio frame time scales must match inside one fMP4 fragment");
        }
        if frame.timing.start_sample != expected_sample || frame.timing.pts.units != expected_sample
        {
            bail!("encoded audio frames must be contiguous inside one fMP4 fragment");
        }
        let duration = frame.timing.duration.units.max(1).min(u64::from(u32::MAX)) as u32;
        let size = frame.payload.len().min(u32::MAX as usize) as u32;
        payload.extend_from_slice(&frame.payload);
        samples.push(Fmp4Sample {
            duration,
            size,
            flags: 0x0200_0000,
            composition_time_offset: 0,
        });
        expected_sample = expected_sample.saturating_add(u64::from(frame.timing.sample_count));
    }

    Ok(Fmp4FragmentTrack {
        track_id,
        base_decode_time: first.timing.start_sample,
        samples,
        payload,
    })
}

/// Builds one fMP4 fragment track from contiguous encoded video frames.
#[allow(
    dead_code,
    reason = "native video transcode backends use this mux adapter from the CLI segment path"
)]
pub fn fragment_track_from_encoded_video_frames(
    track_id: u32,
    frames: &[EncodedVideoFrame],
) -> Result<Fmp4FragmentTrack> {
    let first = frames
        .first()
        .ok_or_else(|| anyhow::anyhow!("fMP4 video fragment requires at least one frame"))?;
    let timescale = first.dts.scale;
    let mut payload = Vec::new();
    let mut samples = Vec::with_capacity(frames.len());

    for frame in frames {
        if frame.pts.scale != timescale
            || frame.dts.scale != timescale
            || frame.duration.scale != timescale
        {
            bail!("encoded video frame time scales must match inside one fMP4 fragment");
        }
        let duration = frame.duration.units.max(1).min(u64::from(u32::MAX)) as u32;
        let size = frame.payload.len().min(u32::MAX as usize) as u32;
        let composition_time_offset = (i128::from(frame.pts.units) - i128::from(frame.dts.units))
            .clamp(i128::from(i32::MIN), i128::from(i32::MAX))
            as i32;
        payload.extend_from_slice(&frame.payload);
        samples.push(Fmp4Sample {
            duration,
            size,
            flags: if frame.keyframe {
                0x0200_0000
            } else {
                0x0101_0000
            },
            composition_time_offset,
        });
    }

    if let Some(min_offset) = samples
        .iter()
        .map(|sample| sample.composition_time_offset)
        .min()
        && min_offset < 0
    {
        for sample in &mut samples {
            sample.composition_time_offset =
                sample.composition_time_offset.saturating_sub(min_offset);
        }
    }

    Ok(Fmp4FragmentTrack {
        track_id,
        base_decode_time: first.dts.units,
        samples,
        payload,
    })
}

/// Builds one fMP4 fragment track from extracted packet-copy chunk samples.
pub fn fragment_track_from_chunk_samples(
    track_id: u32,
    samples: &[ChunkSample],
    payload: Vec<u8>,
    timescale: u32,
) -> Result<Fmp4FragmentTrack> {
    let first = samples
        .first()
        .ok_or_else(|| anyhow::anyhow!("fMP4 packet-copy fragment requires at least one sample"))?;
    let mut out_samples = Vec::with_capacity(samples.len());
    let mut expected_offset = 0_u64;
    for sample in samples {
        if sample.payload_offset != expected_offset {
            bail!("packet-copy samples must be contiguous inside one fMP4 fragment");
        }
        let duration = rescale_time(
            sample.duration.units,
            sample.duration.scale.units_per_second,
            timescale,
        )
        .max(1)
        .min(u64::from(u32::MAX)) as u32;
        let pts = rescale_time(
            sample.pts.units,
            sample.pts.scale.units_per_second,
            timescale,
        );
        let dts = rescale_time(
            sample.dts.units,
            sample.dts.scale.units_per_second,
            timescale,
        );
        out_samples.push(Fmp4Sample {
            duration,
            size: sample.byte_count,
            flags: if sample.keyframe {
                0x0200_0000
            } else {
                0x0101_0000
            },
            composition_time_offset: (pts as i128 - dts as i128)
                .clamp(i128::from(i32::MIN), i128::from(i32::MAX))
                as i32,
        });
        expected_offset = expected_offset.saturating_add(u64::from(sample.byte_count));
    }
    if expected_offset != payload.len() as u64 {
        bail!("packet-copy sample bytes do not match fMP4 fragment payload bytes");
    }
    if let Some(min_offset) = out_samples
        .iter()
        .map(|sample| sample.composition_time_offset)
        .min()
        && min_offset < 0
    {
        for sample in &mut out_samples {
            sample.composition_time_offset =
                sample.composition_time_offset.saturating_sub(min_offset);
        }
    }

    Ok(Fmp4FragmentTrack {
        track_id,
        base_decode_time: rescale_time(
            first.dts.units,
            first.dts.scale.units_per_second,
            timescale,
        ),
        samples: out_samples,
        payload,
    })
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

fn write_trak(out: &mut Vec<u8>, track: &Fmp4Track, edit: Option<(u32, u64)>) {
    write_box(out, *b"trak", |out| {
        write_tkhd(out, track);
        if let Some((priming, duration_ms)) = edit {
            write_box(out, *b"edts", |out| {
                write_full_box(out, *b"elst", 1, 0, |out| {
                    be_u32(out, 1);
                    be_u64(out, duration_ms);
                    be_u64(out, u64::from(priming));
                    be_u16(out, 1);
                    be_u16(out, 0);
                });
            });
        }
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
            | Fmp4SampleEntry::Mp3 { .. }
            | Fmp4SampleEntry::Ac3 { .. }
            | Fmp4SampleEntry::Eac3 { .. }
            | Fmp4SampleEntry::Flac { .. }
            | Fmp4SampleEntry::Alac { .. } => {
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
            } => write_hevc_sample_entry(out, *width, *height, codec_config),
            Fmp4SampleEntry::Aac {
                decoder_config,
                channel_count,
                sample_rate,
            } => write_audio_sample_entry(out, *b"mp4a", *channel_count, *sample_rate, |out| {
                write_esds(out, 0x40, decoder_config)
            }),
            Fmp4SampleEntry::Mp3 {
                channel_count,
                sample_rate,
            } => write_audio_sample_entry(out, *b"mp4a", *channel_count, *sample_rate, |out| {
                write_esds(out, 0x6b, &[])
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
            Fmp4SampleEntry::Flac {
                stream_info,
                channel_count,
                sample_rate,
            } => write_audio_sample_entry(out, *b"fLaC", *channel_count, *sample_rate, |out| {
                write_dfla(out, stream_info);
            }),
            Fmp4SampleEntry::Alac {
                codec_config,
                channel_count,
                sample_rate,
            } => write_audio_sample_entry(out, *b"alac", *channel_count, *sample_rate, |out| {
                write_box(out, *b"alac", |out| out.extend_from_slice(codec_config));
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

fn write_hevc_sample_entry(out: &mut Vec<u8>, width: u16, height: u16, codec_config: &[u8]) {
    write_box(out, *b"hev1", |out| {
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
        write_box(out, *b"hvcC", |out| out.extend_from_slice(codec_config));
        if hevc_config_is_ten_bit_or_higher(codec_config) {
            write_nclx_colr(out, 9, 16, 9, false);
        }
        write_pasp(out, 1, 1);
    });
}

fn hevc_config_is_ten_bit_or_higher(codec_config: &[u8]) -> bool {
    codec_config.get(17).is_some_and(|byte| (byte & 0x07) >= 2)
}

fn write_nclx_colr(
    out: &mut Vec<u8>,
    primaries: u16,
    transfer_characteristics: u16,
    matrix_coefficients: u16,
    full_range: bool,
) {
    write_box(out, *b"colr", |out| {
        out.extend_from_slice(b"nclx");
        be_u16(out, primaries);
        be_u16(out, transfer_characteristics);
        be_u16(out, matrix_coefficients);
        out.push(if full_range { 0x80 } else { 0x00 });
    });
}

fn write_pasp(out: &mut Vec<u8>, h_spacing: u32, v_spacing: u32) {
    write_box(out, *b"pasp", |out| {
        be_u32(out, h_spacing.max(1));
        be_u32(out, v_spacing.max(1));
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

fn write_esds(out: &mut Vec<u8>, object_type: u8, decoder_config: &[u8]) {
    write_full_box(out, *b"esds", 0, 0, |out| {
        write_descriptor(out, 0x03, |out| {
            be_u16(out, 0);
            out.push(0);
            write_descriptor(out, 0x04, |out| {
                out.push(object_type);
                out.push(0x15);
                out.extend_from_slice(&[0, 0, 0]);
                be_u32(out, 0);
                be_u32(out, 0);
                if !decoder_config.is_empty() {
                    write_descriptor(out, 0x05, |out| out.extend_from_slice(decoder_config));
                }
            });
            write_descriptor(out, 0x06, |out| out.push(2));
        });
    });
}

fn write_dfla(out: &mut Vec<u8>, stream_info: &[u8]) {
    write_full_box(out, *b"dfLa", 0, 0, |out| {
        out.push(0x80);
        be_u24(out, stream_info.len() as u32);
        out.extend_from_slice(stream_info);
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

fn be_u24(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&[
        ((v >> 16) & 0xff) as u8,
        ((v >> 8) & 0xff) as u8,
        (v & 0xff) as u8,
    ]);
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
    use crate::packet::{PacketRef, TimeDelta, TimePoint, TimeScale};

    use super::*;

    #[test]
    fn streamed_fragment_matches_buffered_and_never_replaces_conflict() {
        let tracks = vec![Fmp4FragmentTrack {
            track_id: 1,
            base_decode_time: 0,
            samples: vec![Fmp4Sample {
                duration: 1000,
                size: 3,
                flags: 0x0200_0000,
                composition_time_offset: 0,
            }],
            payload: b"abc".to_vec(),
        }];
        let expected = media_fragment(1, &tracks).unwrap();
        let fragment = OwnedMediaFragment::new(1, tracks.clone(), 4096).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("segment.m4s");
        fragment.publish(&path).unwrap();
        fragment.publish(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        assert_eq!(fragment.len(), expected.len());
        assert!(OwnedMediaFragment::new(1, tracks, 2).is_err());
        std::fs::write(&path, b"existing").unwrap();
        assert_eq!(
            fragment.publish(&path).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"existing");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn aac_edit_list_separates_priming_from_coded_sample_duration() {
        let track = Fmp4Track {
            id: 2,
            kind: Fmp4TrackKind::Audio,
            timescale: 48_000,
            default_sample_duration: 1024,
            default_sample_size: 0,
            default_sample_flags: 0x0200_0000,
            sample_entry: Fmp4SampleEntry::Aac {
                decoder_config: vec![0x11, 0x90],
                channel_count: 2,
                sample_rate: 48_000,
            },
        };
        let init = init_segment_with_edits(&[track], &[(2, 1024, 12_345)]).unwrap();
        let position = init.windows(4).position(|bytes| bytes == b"elst").unwrap() + 4;
        assert_eq!(init[position], 1);
        assert_eq!(
            u32::from_be_bytes(init[position + 4..position + 8].try_into().unwrap()),
            1
        );
        assert_eq!(
            u64::from_be_bytes(init[position + 8..position + 16].try_into().unwrap()),
            12_345
        );
        assert_eq!(
            u64::from_be_bytes(init[position + 16..position + 24].try_into().unwrap()),
            1024
        );
    }

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
                    1, 1, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xfa, 0, 0, 0, 0xf3, 0,
                ],
                width: 3_840,
                height: 2_160,
            },
        }])
        .expect("init segment");

        assert!(init.windows(4).any(|w| w == b"hev1"));
        assert!(!init.windows(4).any(|w| w == b"hvc1"));
        assert!(init.windows(4).any(|w| w == b"hvcC"));
        assert!(init.windows(4).any(|w| w == b"colr"));
        assert!(init.windows(4).any(|w| w == b"pasp"));
        assert!(init.windows(23).any(|w| {
            w == [
                1, 1, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xfa, 0, 0, 0, 0xf3, 0,
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
    fn init_segment_writes_mp3_sample_description() {
        let init = init_segment(&[Fmp4Track {
            id: 2,
            kind: Fmp4TrackKind::Audio,
            timescale: 48_000,
            default_sample_duration: 1_152,
            default_sample_size: 0,
            default_sample_flags: 0x0200_0000,
            sample_entry: Fmp4SampleEntry::Mp3 {
                channel_count: 2,
                sample_rate: 48_000,
            },
        }])
        .expect("init segment");

        assert!(init.windows(4).any(|w| w == b"mp4a"));
        assert!(init.windows(4).any(|w| w == b"esds"));
        assert!(init.windows(1).any(|w| w == [0x6b]));
    }

    #[test]
    fn init_segment_writes_flac_sample_description() {
        let stream_info = vec![0x11; 34];
        let init = init_segment(&[Fmp4Track {
            id: 2,
            kind: Fmp4TrackKind::Audio,
            timescale: 48_000,
            default_sample_duration: 4_096,
            default_sample_size: 0,
            default_sample_flags: 0x0200_0000,
            sample_entry: Fmp4SampleEntry::Flac {
                stream_info: stream_info.clone(),
                channel_count: 2,
                sample_rate: 48_000,
            },
        }])
        .expect("init segment");

        assert!(init.windows(4).any(|w| w == b"fLaC"));
        assert!(init.windows(4).any(|w| w == b"dfLa"));
        assert!(init.windows(4).any(|w| w == [0x80, 0, 0, 34]));
        assert!(init.windows(34).any(|w| w == stream_info));
    }

    #[test]
    fn init_segment_writes_alac_sample_description() {
        let config = vec![0; 24];
        let init = init_segment(&[Fmp4Track {
            id: 2,
            kind: Fmp4TrackKind::Audio,
            timescale: 48_000,
            default_sample_duration: 4_096,
            default_sample_size: 0,
            default_sample_flags: 0x0200_0000,
            sample_entry: Fmp4SampleEntry::Alac {
                codec_config: config.clone(),
                channel_count: 2,
                sample_rate: 48_000,
            },
        }])
        .expect("init segment");

        assert!(init.windows(4).filter(|w| *w == b"alac").count() >= 2);
        assert!(init.windows(24).any(|w| w == config));
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
        let validation = validate_media_fragment(&fragment).expect("validate fragment");
        assert_eq!(
            validation,
            validation::Fmp4MediaValidation {
                track_count: 1,
                sample_count: 2,
                payload_bytes: 5,
            }
        );
    }

    #[test]
    fn packet_samples_normalize_negative_composition_offsets() {
        let packets = vec![
            PacketRef {
                source_offset: 0,
                size: 10,
                pts: TimePoint::millis(0),
                dts: TimePoint::millis(0),
                duration: TimeDelta::millis(42),
                keyframe: true,
            },
            PacketRef {
                source_offset: 10,
                size: 10,
                pts: TimePoint::millis(40),
                dts: TimePoint::millis(84),
                duration: TimeDelta::millis(42),
                keyframe: false,
            },
            PacketRef {
                source_offset: 20,
                size: 10,
                pts: TimePoint::millis(166),
                dts: TimePoint::millis(42),
                duration: TimeDelta::millis(42),
                keyframe: false,
            },
        ];

        let samples = samples_from_packets_with_timescale(&packets, 1_000);

        assert_eq!(samples[0].composition_time_offset, 44);
        assert_eq!(samples[1].composition_time_offset, 0);
        assert_eq!(samples[2].composition_time_offset, 168);
    }

    #[test]
    fn builds_fragment_track_from_encoded_audio_frames() {
        let frames =
            encoded_audio_frames(&[(48_000, 1_024, b"one".as_slice()), (49_024, 1_024, b"two")]);

        let track =
            fragment_track_from_encoded_audio_frames(2, &frames).expect("audio fragment track");

        assert_eq!(track.track_id, 2);
        assert_eq!(track.base_decode_time, 48_000);
        assert_eq!(track.payload, b"onetwo");
        assert_eq!(
            track.samples,
            vec![
                Fmp4Sample {
                    duration: 1_024,
                    size: 3,
                    flags: 0x0200_0000,
                    composition_time_offset: 0,
                },
                Fmp4Sample {
                    duration: 1_024,
                    size: 3,
                    flags: 0x0200_0000,
                    composition_time_offset: 0,
                },
            ]
        );
    }

    #[test]
    fn media_fragment_rejects_non_sync_start() {
        let err = media_fragment(
            1,
            &[Fmp4FragmentTrack {
                track_id: 1,
                base_decode_time: 0,
                samples: vec![Fmp4Sample {
                    duration: 1,
                    size: 1,
                    flags: 0x0101_0000,
                    composition_time_offset: 0,
                }],
                payload: vec![0xaa],
            }],
        )
        .expect_err("reject non-sync start");

        assert!(err.to_string().contains("non-sync"));
    }

    #[test]
    fn media_fragment_rejects_zero_duration_sample() {
        let err = media_fragment(
            1,
            &[Fmp4FragmentTrack {
                track_id: 1,
                base_decode_time: 0,
                samples: vec![Fmp4Sample {
                    duration: 0,
                    size: 1,
                    flags: 0x0200_0000,
                    composition_time_offset: 0,
                }],
                payload: vec![0xaa],
            }],
        )
        .expect_err("reject zero-duration sample");

        assert!(err.to_string().contains("zero-duration"));
    }

    #[test]
    fn rejects_non_contiguous_encoded_audio_frames() {
        let frames = encoded_audio_frames(&[(0, 1_024, b"one".as_slice()), (2_048, 1_024, b"gap")]);

        let err = fragment_track_from_encoded_audio_frames(2, &frames).expect_err("reject gap");

        assert!(err.to_string().contains("contiguous"));
    }

    #[test]
    fn builds_fragment_track_from_encoded_video_frames() {
        let time_scale = TimeScale {
            units_per_second: 90_000,
        };
        let frames = vec![
            EncodedVideoFrame {
                pts: TimePoint {
                    units: 90_000,
                    scale: time_scale,
                },
                dts: TimePoint {
                    units: 90_000,
                    scale: time_scale,
                },
                duration: TimeDelta {
                    units: 3_003,
                    scale: time_scale,
                },
                payload: b"idr".to_vec(),
                keyframe: true,
            },
            EncodedVideoFrame {
                pts: TimePoint {
                    units: 93_003,
                    scale: time_scale,
                },
                dts: TimePoint {
                    units: 93_003,
                    scale: time_scale,
                },
                duration: TimeDelta {
                    units: 3_003,
                    scale: time_scale,
                },
                payload: b"p".to_vec(),
                keyframe: false,
            },
        ];

        let track =
            fragment_track_from_encoded_video_frames(1, &frames).expect("video fragment track");

        assert_eq!(track.track_id, 1);
        assert_eq!(track.base_decode_time, 90_000);
        assert_eq!(track.payload, b"idrp");
        assert_eq!(track.samples[0].flags, 0x0200_0000);
        assert_eq!(track.samples[1].flags, 0x0101_0000);
    }

    #[test]
    fn builds_fragment_track_from_packet_copy_chunk_samples() {
        let samples = vec![
            ChunkSample {
                index: 0,
                payload_offset: 0,
                byte_count: 3,
                pts: TimePoint::millis(40),
                dts: TimePoint::millis(0),
                duration: TimeDelta::millis(40),
                keyframe: true,
            },
            ChunkSample {
                index: 1,
                payload_offset: 3,
                byte_count: 2,
                pts: TimePoint::millis(80),
                dts: TimePoint::millis(40),
                duration: TimeDelta::millis(40),
                keyframe: false,
            },
        ];

        let track = fragment_track_from_chunk_samples(1, &samples, b"aaabb".to_vec(), 1_000)
            .expect("packet-copy fragment track");

        assert_eq!(track.track_id, 1);
        assert_eq!(track.base_decode_time, 0);
        assert_eq!(track.payload, b"aaabb");
        assert_eq!(track.samples[0].composition_time_offset, 40);
        assert_eq!(track.samples[0].flags, 0x0200_0000);
        assert_eq!(track.samples[1].flags, 0x0101_0000);
    }

    #[test]
    fn packet_copy_fragment_normalizes_negative_composition_offsets() {
        let samples = vec![
            ChunkSample {
                index: 0,
                payload_offset: 0,
                byte_count: 1,
                pts: TimePoint::millis(0),
                dts: TimePoint::millis(40),
                duration: TimeDelta::millis(40),
                keyframe: true,
            },
            ChunkSample {
                index: 1,
                payload_offset: 1,
                byte_count: 1,
                pts: TimePoint::millis(80),
                dts: TimePoint::millis(80),
                duration: TimeDelta::millis(40),
                keyframe: false,
            },
        ];

        let track = fragment_track_from_chunk_samples(1, &samples, b"ab".to_vec(), 1_000)
            .expect("normalized packet-copy fragment");

        assert_eq!(track.samples[0].composition_time_offset, 0);
        assert_eq!(track.samples[1].composition_time_offset, 40);
        media_fragment(1, &[track]).expect("valid media fragment");
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

    fn encoded_audio_frames(frames: &[(u64, u32, &[u8])]) -> Vec<EncodedAudioFrame> {
        frames
            .iter()
            .map(|(start_sample, sample_count, payload)| EncodedAudioFrame {
                timing: crate::transcode::AudioFrameTiming {
                    pts: TimePoint {
                        units: *start_sample,
                        scale: crate::packet::TimeScale {
                            units_per_second: 48_000,
                        },
                    },
                    duration: TimeDelta {
                        units: u64::from(*sample_count),
                        scale: crate::packet::TimeScale {
                            units_per_second: 48_000,
                        },
                    },
                    start_sample: *start_sample,
                    sample_count: *sample_count,
                    reanchored: false,
                },
                payload: payload.to_vec(),
                discontinuity: false,
            })
            .collect()
    }
}
