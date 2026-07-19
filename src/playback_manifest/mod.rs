use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{
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
    pub config_box: Option<String>,
    pub decoder_config_hex: Option<String>,
    pub chunks: Vec<NativeChunk>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mp4ManifestOptions {
    pub chunk_target_ms: u64,
    pub include_primary_audio: bool,
}

impl Default for Mp4ManifestOptions {
    fn default() -> Self {
        Self {
            chunk_target_ms: 4_000,
            include_primary_audio: true,
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

    if options.include_primary_audio
        && metadata
            .tracks
            .iter()
            .any(|track| track.kind == Mp4TrackKind::Audio)
    {
        if let Some(track) = manifest_track(bytes, "a0", options.chunk_target_ms) {
            tracks.push(track);
        }
    }

    Some(NativePlaybackManifest {
        schema_version: 1,
        source_path: source_path.display().to_string(),
        duration_ms: metadata.duration_ms,
        chunk_target_ms: options.chunk_target_ms,
        tracks,
    })
}

fn manifest_track(bytes: &[u8], track_id: &str, target_ms: u64) -> Option<ManifestTrack> {
    let config = parse_codec_config(bytes, Some(track_id))?;
    let plan = parse_chunk_plan(bytes, Some(track_id), target_ms)?;
    Some(ManifestTrack {
        id: config.track_id,
        kind: config.track_kind,
        codec: config.codec,
        codec_string: config.codec_string,
        config_box: config.config_box,
        decoder_config_hex: config.description_hex,
        chunks: plan.chunks,
    })
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
                include_primary_audio: true,
            },
        )
        .unwrap();

        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.duration_ms, Some(3_000));
        assert_eq!(manifest.tracks.len(), 2);
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
        data.extend_from_slice(&atom(b"moov", &[mvhd(1000, 3000), video, audio].concat()));
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
