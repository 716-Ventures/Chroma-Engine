use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{
    codec::aac::parse_audio_specific_config,
    container::matroska::{
        parse_basic_metadata as parse_matroska_basic_metadata,
        parse_chunk_plan as parse_matroska_chunk_plan, MatroskaTrack, MatroskaTrackKind,
    },
    container::mp4::{parse_basic_metadata, parse_chunk_plan, parse_codec_config, Mp4TrackKind},
    packet::NativeChunk,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NativePlaybackManifest {
    pub schema_version: u32,
    pub source_path: String,
    pub duration_ms: Option<u64>,
    pub chunk_target_ms: u64,
    pub tracks: Vec<ManifestTrack>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManifestTrack {
    pub id: String,
    pub kind: String,
    pub codec: String,
    pub codec_string: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub channels: Option<u32>,
    pub sample_rate: Option<u32>,
    pub default: bool,
    pub forced: bool,
    pub config_box: Option<String>,
    pub decoder_config_hex: Option<String>,
    pub chunks: Vec<NativeChunk>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mp4ManifestOptions {
    pub chunk_target_ms: u64,
    pub include_audio: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatroskaManifestOptions {
    pub chunk_target_ms: u64,
    pub include_audio: bool,
}

impl Default for MatroskaManifestOptions {
    fn default() -> Self {
        Self {
            chunk_target_ms: 4_000,
            include_audio: true,
        }
    }
}

impl Default for Mp4ManifestOptions {
    fn default() -> Self {
        Self {
            chunk_target_ms: 4_000,
            include_audio: true,
        }
    }
}

pub fn build_mp4_playback_manifest(
    bytes: &[u8],
    source_path: &Path,
    options: Mp4ManifestOptions,
) -> Option<NativePlaybackManifest> {
    let metadata = parse_basic_metadata(bytes);
    let mut tracks = Vec::new();

    if metadata
        .tracks
        .iter()
        .any(|track| track.kind == Mp4TrackKind::Video)
    {
        tracks.push(manifest_track(bytes, "v0", options.chunk_target_ms)?);
    }

    if options.include_audio {
        for track_id in mp4_audio_track_ids(&metadata.tracks) {
            if let Some(track) = manifest_track(bytes, &track_id, options.chunk_target_ms) {
                tracks.push(track);
            }
        }
    }

    Some(NativePlaybackManifest {
        schema_version: 2,
        source_path: source_path.display().to_string(),
        duration_ms: metadata.duration_ms,
        chunk_target_ms: options.chunk_target_ms,
        tracks,
    })
}

pub fn build_matroska_playback_manifest(
    bytes: &[u8],
    source_path: &Path,
    options: MatroskaManifestOptions,
) -> Option<NativePlaybackManifest> {
    let metadata = parse_matroska_basic_metadata(bytes);
    let mut tracks = Vec::new();

    if let Some(video) = metadata
        .tracks
        .iter()
        .filter(|track| track.kind == MatroskaTrackKind::Video)
        .find(|track| matches!(track.codec.as_str(), "h264" | "hevc"))
    {
        tracks.push(matroska_manifest_track(
            bytes,
            video,
            "v0",
            options.chunk_target_ms,
            metadata.duration_ms,
        )?);
    }

    if options.include_audio {
        for (track_id, audio) in matroska_audio_tracks(&metadata.tracks) {
            if let Some(track) = matroska_manifest_track(
                bytes,
                audio,
                &track_id,
                options.chunk_target_ms,
                metadata.duration_ms,
            ) {
                tracks.push(track);
            }
        }
    }

    Some(NativePlaybackManifest {
        schema_version: 2,
        source_path: source_path.display().to_string(),
        duration_ms: metadata.duration_ms,
        chunk_target_ms: options.chunk_target_ms,
        tracks,
    })
}

fn mp4_audio_track_ids(tracks: &[crate::container::mp4::Mp4Track]) -> Vec<String> {
    let mut video_index = 0_u32;
    let mut audio_index = 0_u32;
    let mut subtitle_index = 0_u32;
    let mut unknown_index = 0_u32;
    let mut out = Vec::new();
    for track in tracks {
        let track_id = match track.kind {
            Mp4TrackKind::Video => next_semantic_track_id("v", &mut video_index),
            Mp4TrackKind::Audio => next_semantic_track_id("a", &mut audio_index),
            Mp4TrackKind::Subtitle => next_semantic_track_id("s", &mut subtitle_index),
            Mp4TrackKind::Unknown => next_semantic_track_id("x", &mut unknown_index),
        };
        if track.kind == Mp4TrackKind::Audio {
            out.push(track_id);
        }
    }
    out
}

fn matroska_audio_tracks(tracks: &[MatroskaTrack]) -> Vec<(String, &MatroskaTrack)> {
    let mut video_index = 0_u32;
    let mut audio_index = 0_u32;
    let mut subtitle_index = 0_u32;
    let mut unknown_index = 0_u32;
    let mut out = Vec::new();
    for track in tracks {
        let track_id = match track.kind {
            MatroskaTrackKind::Video => next_semantic_track_id("v", &mut video_index),
            MatroskaTrackKind::Audio => next_semantic_track_id("a", &mut audio_index),
            MatroskaTrackKind::Subtitle => next_semantic_track_id("s", &mut subtitle_index),
            MatroskaTrackKind::Unknown => next_semantic_track_id("x", &mut unknown_index),
        };
        if track.kind == MatroskaTrackKind::Audio {
            out.push((track_id, track));
        }
    }
    out
}

fn next_semantic_track_id(prefix: &str, counter: &mut u32) -> String {
    let id = format!("{prefix}{counter}");
    *counter = counter.saturating_add(1);
    id
}

fn manifest_track(bytes: &[u8], track_id: &str, target_ms: u64) -> Option<ManifestTrack> {
    let config = parse_codec_config(bytes, Some(track_id))?;
    let plan = parse_chunk_plan(bytes, Some(track_id), target_ms)?;
    let metadata = parse_basic_metadata(bytes);
    let track_meta = mp4_track_by_semantic_id(&metadata.tracks, track_id);
    Some(ManifestTrack {
        id: config.track_id,
        kind: config.track_kind,
        codec_string: config
            .codec_string
            .or_else(|| fallback_codec_string(config.codec.as_str())),
        codec: config.codec,
        language: None,
        title: None,
        channels: track_meta.and_then(|track| track.channels),
        sample_rate: track_meta.and_then(|track| track.sample_rate),
        default: track_id.ends_with('0'),
        forced: false,
        config_box: config.config_box,
        decoder_config_hex: config.description_hex,
        chunks: plan.chunks,
    })
}

fn mp4_track_by_semantic_id<'a>(
    tracks: &'a [crate::container::mp4::Mp4Track],
    target_id: &str,
) -> Option<&'a crate::container::mp4::Mp4Track> {
    let mut video_index = 0_u32;
    let mut audio_index = 0_u32;
    let mut subtitle_index = 0_u32;
    let mut unknown_index = 0_u32;
    for track in tracks {
        let track_id = match track.kind {
            Mp4TrackKind::Video => next_semantic_track_id("v", &mut video_index),
            Mp4TrackKind::Audio => next_semantic_track_id("a", &mut audio_index),
            Mp4TrackKind::Subtitle => next_semantic_track_id("s", &mut subtitle_index),
            Mp4TrackKind::Unknown => next_semantic_track_id("x", &mut unknown_index),
        };
        if track_id == target_id {
            return Some(track);
        }
    }
    None
}

fn fallback_codec_string(codec: &str) -> Option<String> {
    match codec {
        "ac3" => Some("ac-3".to_string()),
        "eac3" => Some("ec-3".to_string()),
        _ => None,
    }
}

fn matroska_manifest_track(
    bytes: &[u8],
    track: &MatroskaTrack,
    track_id: &str,
    target_ms: u64,
    duration_ms: Option<u64>,
) -> Option<ManifestTrack> {
    let chunks = parse_matroska_chunk_plan(bytes, Some(track_id), target_ms)
        .map(|plan| plan.chunks)
        .or_else(|| Some(duration_chunks(duration_ms?, target_ms)))?;
    let private = track.codec_private.as_deref();
    Some(ManifestTrack {
        id: track_id.to_string(),
        kind: matroska_track_kind_name(track.kind).to_string(),
        codec: track.codec.clone(),
        codec_string: matroska_codec_string(track, private),
        language: track.language.clone(),
        title: track.name.clone(),
        channels: track.channels,
        sample_rate: track.sample_rate,
        default: track.default,
        forced: track.forced,
        config_box: matroska_config_box(track, private).map(str::to_string),
        decoder_config_hex: private.map(hex_string),
        chunks,
    })
}

fn duration_chunks(duration_ms: u64, target_ms: u64) -> Vec<NativeChunk> {
    if duration_ms == 0 || target_ms == 0 {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut start = 0_u64;
    while start < duration_ms {
        let len = duration_ms.saturating_sub(start).min(target_ms);
        chunks.push(NativeChunk {
            index: chunks.len() as u32,
            start: crate::packet::TimePoint::millis(start),
            duration: crate::packet::TimeDelta::millis(len),
            packet_range: crate::packet::PacketRange { start: 0, end: 0 },
            key_aligned: true,
        });
        start = start.saturating_add(len);
    }
    chunks
}

fn matroska_track_kind_name(kind: MatroskaTrackKind) -> &'static str {
    match kind {
        MatroskaTrackKind::Video => "video",
        MatroskaTrackKind::Audio => "audio",
        MatroskaTrackKind::Subtitle => "subtitle",
        MatroskaTrackKind::Unknown => "unknown",
    }
}

fn matroska_config_box(track: &MatroskaTrack, private: Option<&[u8]>) -> Option<&'static str> {
    match (track.codec.as_str(), private) {
        ("h264", Some(_)) => Some("avcC"),
        ("hevc", Some(_)) => Some("hvcC"),
        ("aac", Some(_)) => Some("asc"),
        _ => None,
    }
}

fn matroska_codec_string(track: &MatroskaTrack, private: Option<&[u8]>) -> Option<String> {
    match (track.codec.as_str(), private) {
        ("h264", Some(config)) if config.len() >= 4 => Some(format!(
            "avc1.{:02X}{:02X}{:02X}",
            config[1], config[2], config[3]
        )),
        ("hevc", Some(config)) => hevc_codec_string(config),
        ("aac", Some(config)) => parse_audio_specific_config(config)
            .ok()
            .map(|config| format!("mp4a.40.{}", config.object_type)),
        _ => None,
    }
}

fn hevc_codec_string(config: &[u8]) -> Option<String> {
    if config.len() < 13 {
        return None;
    }
    let profile_space = match (config[1] >> 6) & 0x03 {
        0 => "",
        1 => "A",
        2 => "B",
        3 => "C",
        _ => "",
    };
    let tier = if config[1] & 0x20 != 0 { "H" } else { "L" };
    let profile_idc = config[1] & 0x1f;
    let compatibility = u32::from_be_bytes(config[2..6].try_into().ok()?);
    let level_idc = config[12];
    Some(format!(
        "hvc1.{profile_space}{profile_idc}.{compatibility:X}.{tier}{level_idc}"
    ))
}

fn hex_string(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn builds_mp4_manifest_with_video_and_audio_tracks() {
        let data = fixture_mp4();
        let manifest = build_mp4_playback_manifest(
            &data,
            &PathBuf::from("/tmp/sample.mp4"),
            Mp4ManifestOptions {
                chunk_target_ms: 2_000,
                include_audio: true,
            },
        )
        .unwrap();

        assert_eq!(manifest.schema_version, 2);
        assert_eq!(manifest.duration_ms, Some(3_000));
        assert_eq!(manifest.tracks.len(), 3);
        assert_eq!(manifest.tracks[0].id, "v0");
        assert_eq!(
            manifest.tracks[0].codec_string.as_deref(),
            Some("avc1.64001F")
        );
        assert_eq!(manifest.tracks[0].chunks.len(), 2);
        assert_eq!(manifest.tracks[1].id, "a0");
        assert_eq!(
            manifest.tracks[1].codec_string.as_deref(),
            Some("mp4a.40.2")
        );
        assert_eq!(manifest.tracks[2].id, "a1");
        assert_eq!(
            manifest.tracks[2].codec_string.as_deref(),
            Some("mp4a.40.2")
        );
    }

    fn fixture_mp4() -> Vec<u8> {
        let video = trak(
            b"vide",
            b"avc1",
            video_sample_entry_payload(
                Some((1920, 1080)),
                &atom(b"avcC", &[1, 0x64, 0x00, 0x1f, 0xff, 0xe1, 0, 0]),
            ),
            &[100, 120, 140],
            &[1000, 1000, 1000],
            &[1, 3],
            &[500, 900],
            &[(1, 2), (2, 1)],
        );
        let audio = trak(
            b"soun",
            b"mp4a",
            audio_sample_entry_payload(
                2,
                48_000,
                &atom(
                    b"esds",
                    &[
                        [0, 0, 0, 0].as_slice(),
                        &es_descriptor(&decoder_config_descriptor(&decoder_specific_descriptor(
                            &[0x12, 0x10],
                        ))),
                    ]
                    .concat(),
                ),
            ),
            &[10, 10, 10],
            &[1000, 1000, 1000],
            &[1, 2, 3],
            &[1000, 1100, 1200],
            &[(1, 1)],
        );
        let audio_secondary = trak(
            b"soun",
            b"mp4a",
            audio_sample_entry_payload(
                2,
                48_000,
                &atom(
                    b"esds",
                    &[
                        [0, 0, 0, 0].as_slice(),
                        &es_descriptor(&decoder_config_descriptor(&decoder_specific_descriptor(
                            &[0x12, 0x10],
                        ))),
                    ]
                    .concat(),
                ),
            ),
            &[12, 12, 12],
            &[1000, 1000, 1000],
            &[1, 2, 3],
            &[1300, 1400, 1500],
            &[(1, 1)],
        );

        let mut data = atom(
            b"ftyp",
            &[
                b"isom".as_slice(),
                &0_u32.to_be_bytes(),
                b"isom".as_slice(),
                b"mp42".as_slice(),
            ]
            .concat(),
        );
        data.extend_from_slice(&atom(
            b"moov",
            &[mvhd(1000, 3000), video, audio, audio_secondary].concat(),
        ));
        data
    }

    fn trak(
        handler: &[u8; 4],
        sample_entry: &[u8; 4],
        sample_entry_payload: Vec<u8>,
        sample_sizes: &[u32],
        sample_durations: &[u32],
        sync_samples: &[u32],
        chunk_offsets: &[u32],
        sample_to_chunk: &[(u32, u32)],
    ) -> Vec<u8> {
        atom(
            b"trak",
            &[
                tkhd(Some((1920, 1080))),
                atom(
                    b"mdia",
                    &[
                        mdhd(1000, sample_durations.iter().sum()),
                        hdlr(handler),
                        atom(
                            b"minf",
                            &atom(
                                b"stbl",
                                &[
                                    stsd(sample_entry, &sample_entry_payload),
                                    stts(sample_durations),
                                    stss(sync_samples),
                                    stsc(sample_to_chunk),
                                    stsz(sample_sizes),
                                    stco(chunk_offsets),
                                ]
                                .concat(),
                            ),
                        ),
                    ]
                    .concat(),
                ),
            ]
            .concat(),
        )
    }

    fn mvhd(timescale: u32, duration: u32) -> Vec<u8> {
        let mut payload = vec![0_u8; 20];
        payload[12..16].copy_from_slice(&timescale.to_be_bytes());
        payload[16..20].copy_from_slice(&duration.to_be_bytes());
        atom(b"mvhd", &payload)
    }

    fn tkhd(size: Option<(u32, u32)>) -> Vec<u8> {
        let mut payload = vec![0_u8; 84];
        if let Some((w, h)) = size {
            payload[76..80].copy_from_slice(&(w << 16).to_be_bytes());
            payload[80..84].copy_from_slice(&(h << 16).to_be_bytes());
        }
        atom(b"tkhd", &payload)
    }

    fn mdhd(timescale: u32, duration: u32) -> Vec<u8> {
        let mut payload = vec![0_u8; 20];
        payload[12..16].copy_from_slice(&timescale.to_be_bytes());
        payload[16..20].copy_from_slice(&duration.to_be_bytes());
        atom(b"mdhd", &payload)
    }

    fn hdlr(handler: &[u8; 4]) -> Vec<u8> {
        let mut payload = vec![0_u8; 12];
        payload[8..12].copy_from_slice(handler);
        atom(b"hdlr", &payload)
    }

    fn stsd(sample_entry: &[u8; 4], entry_payload: &[u8]) -> Vec<u8> {
        let entry = atom(sample_entry, entry_payload);
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(&1_u32.to_be_bytes());
        payload.extend_from_slice(&entry);
        atom(b"stsd", &payload)
    }

    fn stts(sample_durations: &[u32]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(&(sample_durations.len() as u32).to_be_bytes());
        for duration in sample_durations {
            payload.extend_from_slice(&1_u32.to_be_bytes());
            payload.extend_from_slice(&duration.to_be_bytes());
        }
        atom(b"stts", &payload)
    }

    fn stss(sync_samples: &[u32]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(&(sync_samples.len() as u32).to_be_bytes());
        for sample in sync_samples {
            payload.extend_from_slice(&sample.to_be_bytes());
        }
        atom(b"stss", &payload)
    }

    fn stsc(entries: &[(u32, u32)]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (first_chunk, samples_per_chunk) in entries {
            payload.extend_from_slice(&first_chunk.to_be_bytes());
            payload.extend_from_slice(&samples_per_chunk.to_be_bytes());
            payload.extend_from_slice(&1_u32.to_be_bytes());
        }
        atom(b"stsc", &payload)
    }

    fn stsz(sample_sizes: &[u32]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(&0_u32.to_be_bytes());
        payload.extend_from_slice(&(sample_sizes.len() as u32).to_be_bytes());
        for size in sample_sizes {
            payload.extend_from_slice(&size.to_be_bytes());
        }
        atom(b"stsz", &payload)
    }

    fn stco(chunk_offsets: &[u32]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0, 0, 0, 0]);
        payload.extend_from_slice(&(chunk_offsets.len() as u32).to_be_bytes());
        for offset in chunk_offsets {
            payload.extend_from_slice(&offset.to_be_bytes());
        }
        atom(b"stco", &payload)
    }

    fn video_sample_entry_payload(size: Option<(u32, u32)>, child_boxes: &[u8]) -> Vec<u8> {
        let mut payload = vec![0_u8; 78];
        if let Some((w, h)) = size {
            payload[24..26].copy_from_slice(&(w as u16).to_be_bytes());
            payload[26..28].copy_from_slice(&(h as u16).to_be_bytes());
        }
        payload.extend_from_slice(child_boxes);
        payload
    }

    fn audio_sample_entry_payload(channels: u16, sample_rate: u32, child_boxes: &[u8]) -> Vec<u8> {
        let mut payload = vec![0_u8; 28];
        payload[16..18].copy_from_slice(&channels.to_be_bytes());
        payload[24..28].copy_from_slice(&(sample_rate << 16).to_be_bytes());
        payload.extend_from_slice(child_boxes);
        payload
    }

    fn es_descriptor(nested: &[u8]) -> Vec<u8> {
        descriptor(0x03, &[[0, 1, 0].as_slice(), nested].concat())
    }

    fn decoder_config_descriptor(nested: &[u8]) -> Vec<u8> {
        descriptor(
            0x04,
            &[
                [0x40, 0x15].as_slice(),
                &[0, 0, 0],
                &128_000_u32.to_be_bytes(),
                &128_000_u32.to_be_bytes(),
                nested,
            ]
            .concat(),
        )
    }

    fn decoder_specific_descriptor(config: &[u8]) -> Vec<u8> {
        descriptor(0x05, config)
    }

    fn descriptor(tag: u8, payload: &[u8]) -> Vec<u8> {
        assert!(payload.len() < 0x80);
        let mut out = Vec::with_capacity(payload.len() + 2);
        out.push(tag);
        out.push(payload.len() as u8);
        out.extend_from_slice(payload);
        out
    }

    fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload.len() + 8);
        out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }
}
