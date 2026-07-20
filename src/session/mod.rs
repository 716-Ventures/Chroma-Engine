use serde::{Deserialize, Serialize};

use crate::probe::{CodecFamily, MediaProbe, MediaTrack, SubtitleFormat, TrackKind};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackPlan {
    pub schema_version: u32,
    pub source_path: String,
    pub duration_ms: Option<u64>,
    pub selected_tracks: Vec<String>,
    pub stages: Vec<PipelineStage>,
    pub transports: Vec<TransportPlan>,
    pub constraints: PlaybackConstraints,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackConstraints {
    pub target: PlaybackTarget,
    pub max_video_pixels: Option<u64>,
    pub prefer_copy: bool,
    pub audio_selection: AudioSelection,
    pub include_subtitles: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PlaybackTarget {
    NativeChroma,
    Browser,
    AppleNative,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AudioSelection {
    Primary,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PipelineStage {
    pub id: String,
    pub kind: StageKind,
    pub track_ids: Vec<String>,
    pub mode: StageMode,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum StageKind {
    Demux,
    PacketFilter,
    Decode,
    Encode,
    SubtitleTransform,
    Mux,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum StageMode {
    Shared,
    Copy,
    Transform,
    Decode,
    Encode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TransportPlan {
    pub id: String,
    pub kind: TransportKind,
    pub source_stage_id: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TransportKind {
    ChromaFrames,
    ChromaSegments,
}

impl Default for PlaybackConstraints {
    fn default() -> Self {
        Self {
            target: PlaybackTarget::NativeChroma,
            max_video_pixels: None,
            prefer_copy: true,
            audio_selection: AudioSelection::Primary,
            include_subtitles: true,
        }
    }
}

pub fn plan_playback(probe: &MediaProbe, constraints: PlaybackConstraints) -> PlaybackPlan {
    let selected = select_tracks(probe, &constraints);
    let selected_ids = selected
        .iter()
        .map(|track| track.id.clone())
        .collect::<Vec<_>>();
    let mut stages = Vec::new();

    stages.push(PipelineStage {
        id: "demux0".to_string(),
        kind: StageKind::Demux,
        track_ids: selected_ids.clone(),
        mode: StageMode::Shared,
        reason: "one shared demux stage feeds every selected track".to_string(),
    });

    let copy_tracks = selected
        .iter()
        .filter(|track| track_can_copy_for_target(track, constraints.target))
        .map(|track| track.id.clone())
        .collect::<Vec<_>>();
    if !copy_tracks.is_empty() {
        stages.push(PipelineStage {
            id: "packet-copy0".to_string(),
            kind: StageKind::PacketFilter,
            track_ids: copy_tracks,
            mode: StageMode::Copy,
            reason: "copy-compatible tracks stay compressed and bypass decode".to_string(),
        });
    }

    let decode_tracks = selected
        .iter()
        .filter(|track| !track_can_copy_for_target(track, constraints.target))
        .map(|track| track.id.clone())
        .collect::<Vec<_>>();
    let has_decode = !decode_tracks.is_empty();
    if !decode_tracks.is_empty() {
        stages.push(PipelineStage {
            id: "decode0".to_string(),
            kind: StageKind::Decode,
            track_ids: decode_tracks.clone(),
            mode: StageMode::Decode,
            reason: "target cannot consume these compressed track families directly".to_string(),
        });
        stages.push(PipelineStage {
            id: "encode0".to_string(),
            kind: StageKind::Encode,
            track_ids: decode_tracks,
            mode: StageMode::Encode,
            reason: "decoded tracks need a target-native compressed representation".to_string(),
        });
    }

    let text_subtitles = selected
        .iter()
        .filter(|track| {
            track.kind == TrackKind::Subtitle && track.codec.family == CodecFamily::TextSubtitle
        })
        .map(|track| track.id.clone())
        .collect::<Vec<_>>();
    if !text_subtitles.is_empty() {
        stages.push(PipelineStage {
            id: "subtitle-text0".to_string(),
            kind: StageKind::SubtitleTransform,
            track_ids: text_subtitles,
            mode: StageMode::Transform,
            reason: "text subtitles are normalized once and can feed multiple transports"
                .to_string(),
        });
    }

    stages.push(PipelineStage {
        id: "mux0".to_string(),
        kind: StageKind::Mux,
        track_ids: selected_ids.clone(),
        mode: StageMode::Shared,
        reason: "one mux graph owns timing, interleaving, and transport fanout".to_string(),
    });

    PlaybackPlan {
        schema_version: 1,
        source_path: probe.source.path.display().to_string(),
        duration_ms: probe.duration_ms,
        selected_tracks: selected_ids,
        stages,
        transports: transports_for_plan(has_decode),
        constraints,
    }
}

fn select_tracks<'a>(
    probe: &'a MediaProbe,
    constraints: &PlaybackConstraints,
) -> Vec<&'a MediaTrack> {
    let mut selected = Vec::new();

    if let Some(video) = probe
        .tracks
        .iter()
        .find(|track| track.kind == TrackKind::Video)
    {
        selected.push(video);
    }

    match constraints.audio_selection {
        AudioSelection::Primary => {
            if let Some(audio) = primary_audio_track(probe, constraints.target) {
                selected.push(audio);
            }
        }
        AudioSelection::All => {
            selected.extend(
                probe
                    .tracks
                    .iter()
                    .filter(|track| track.kind == TrackKind::Audio)
                    .filter(|track| target_can_use_track(track, constraints.target)),
            );
        }
    }

    if constraints.include_subtitles {
        selected.extend(
            probe
                .tracks
                .iter()
                .filter(|track| track.kind == TrackKind::Subtitle)
                .filter(|track| target_can_use_track(track, constraints.target)),
        );
    }

    selected
}

fn primary_audio_track(probe: &MediaProbe, target: PlaybackTarget) -> Option<&MediaTrack> {
    let audio_tracks = probe
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Audio)
        .filter(|track| target_can_use_track(track, target))
        .collect::<Vec<_>>();
    audio_tracks
        .iter()
        .copied()
        .find(|track| track.flags.default)
        .or_else(|| audio_tracks.first().copied())
}

fn target_can_use_track(track: &MediaTrack, target: PlaybackTarget) -> bool {
    match (track.kind, target) {
        (TrackKind::Subtitle, PlaybackTarget::Browser | PlaybackTarget::AppleNative) => track
            .subtitle
            .as_ref()
            .is_none_or(|subtitle| subtitle.format != SubtitleFormat::Bitmap),
        _ => true,
    }
}

fn transports_for_plan(has_decode: bool) -> Vec<TransportPlan> {
    let mut out = vec![TransportPlan {
        id: "chroma-segments0".to_string(),
        kind: TransportKind::ChromaSegments,
        source_stage_id: "mux0".to_string(),
        notes: vec!["compressed timed chunks for copy-first playback".to_string()],
    }];
    if has_decode {
        out.push(TransportPlan {
            id: "chroma-frames0".to_string(),
            kind: TransportKind::ChromaFrames,
            source_stage_id: "decode0".to_string(),
            notes: vec!["available only when a plan contains decode stages".to_string()],
        });
    }
    out
}

fn track_can_copy_for_target(track: &MediaTrack, target: PlaybackTarget) -> bool {
    match track.kind {
        TrackKind::Video => video_can_copy_for_target(track.codec.family, target),
        TrackKind::Audio => audio_can_copy_for_target(track.codec.family, target),
        TrackKind::Subtitle => subtitle_can_copy_for_target(track.codec.family, target),
        TrackKind::Unknown => false,
    }
}

fn video_can_copy_for_target(family: CodecFamily, target: PlaybackTarget) -> bool {
    match target {
        PlaybackTarget::NativeChroma => matches!(
            family,
            CodecFamily::H264 | CodecFamily::Hevc | CodecFamily::Av1 | CodecFamily::Vp9
        ),
        PlaybackTarget::AppleNative => matches!(family, CodecFamily::H264 | CodecFamily::Hevc),
        PlaybackTarget::Browser => matches!(
            family,
            CodecFamily::H264 | CodecFamily::Av1 | CodecFamily::Vp9
        ),
    }
}

fn audio_can_copy_for_target(family: CodecFamily, target: PlaybackTarget) -> bool {
    match target {
        PlaybackTarget::NativeChroma => matches!(
            family,
            CodecFamily::Aac
                | CodecFamily::Ac3
                | CodecFamily::Eac3
                | CodecFamily::Dts
                | CodecFamily::TrueHd
                | CodecFamily::Flac
                | CodecFamily::Alac
                | CodecFamily::Opus
                | CodecFamily::Mp3
        ),
        PlaybackTarget::AppleNative => matches!(
            family,
            CodecFamily::Aac
                | CodecFamily::Ac3
                | CodecFamily::Eac3
                | CodecFamily::Alac
                | CodecFamily::Mp3
        ),
        PlaybackTarget::Browser => matches!(
            family,
            CodecFamily::Aac | CodecFamily::Mp3 | CodecFamily::Opus
        ),
    }
}

fn subtitle_can_copy_for_target(family: CodecFamily, target: PlaybackTarget) -> bool {
    match target {
        PlaybackTarget::NativeChroma => {
            matches!(
                family,
                CodecFamily::TextSubtitle | CodecFamily::BitmapSubtitle
            )
        }
        PlaybackTarget::Browser | PlaybackTarget::AppleNative => {
            matches!(family, CodecFamily::TextSubtitle)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::probe::{
        AttachmentSummary, CodecDescriptor, ContainerDescriptor, ContainerFamily,
        MediaCapabilities, MediaProbe, MediaSource, MediaTrack, ProbeEngine, SubtitleDescriptor,
        TrackFlags,
    };

    use super::*;

    #[test]
    fn native_chroma_plan_keeps_remux_tracks_compressed() {
        let probe = probe_with_tracks(vec![
            track("v0", TrackKind::Video, CodecFamily::Hevc),
            track("a0", TrackKind::Audio, CodecFamily::TrueHd),
            track("s0", TrackKind::Subtitle, CodecFamily::BitmapSubtitle),
        ]);
        let plan = plan_playback(&probe, PlaybackConstraints::default());
        assert!(plan.stages.iter().any(|stage| stage.id == "packet-copy0"));
        assert!(!plan.stages.iter().any(|stage| stage.id == "decode0"));
        assert_eq!(plan.transports[0].kind, TransportKind::ChromaSegments);
    }

    #[test]
    fn browser_plan_uses_decode_for_hevc_truehd() {
        let probe = probe_with_tracks(vec![
            track("v0", TrackKind::Video, CodecFamily::Hevc),
            track("a0", TrackKind::Audio, CodecFamily::TrueHd),
            track("s0", TrackKind::Subtitle, CodecFamily::TextSubtitle),
        ]);
        let plan = plan_playback(
            &probe,
            PlaybackConstraints {
                target: PlaybackTarget::Browser,
                ..PlaybackConstraints::default()
            },
        );
        let decode = plan
            .stages
            .iter()
            .find(|stage| stage.id == "decode0")
            .unwrap();
        assert_eq!(decode.track_ids, vec!["v0".to_string(), "a0".to_string()]);
        assert!(
            plan.transports
                .iter()
                .any(|transport| transport.kind == TransportKind::ChromaSegments)
        );
    }

    #[test]
    fn browser_plan_excludes_bitmap_subtitles() {
        let probe = probe_with_tracks(vec![
            track("v0", TrackKind::Video, CodecFamily::H264),
            track("s0", TrackKind::Subtitle, CodecFamily::BitmapSubtitle),
        ]);
        let plan = plan_playback(
            &probe,
            PlaybackConstraints {
                target: PlaybackTarget::Browser,
                ..PlaybackConstraints::default()
            },
        );
        assert_eq!(plan.selected_tracks, vec!["v0".to_string()]);
    }

    #[test]
    fn all_audio_selection_keeps_every_audio_track() {
        let probe = probe_with_tracks(vec![
            track("v0", TrackKind::Video, CodecFamily::H264),
            track("a0", TrackKind::Audio, CodecFamily::Aac),
            track("a1", TrackKind::Audio, CodecFamily::Ac3),
        ]);
        let plan = plan_playback(
            &probe,
            PlaybackConstraints {
                audio_selection: AudioSelection::All,
                ..PlaybackConstraints::default()
            },
        );
        assert_eq!(
            plan.selected_tracks,
            vec!["v0".to_string(), "a0".to_string(), "a1".to_string()]
        );
    }

    fn probe_with_tracks(tracks: Vec<MediaTrack>) -> MediaProbe {
        MediaProbe {
            schema_version: 1,
            engine: ProbeEngine {
                name: "test".to_string(),
                version: "0".to_string(),
            },
            source: MediaSource {
                path: PathBuf::from("/tmp/sample.mkv"),
                size_bytes: 1,
                container: ContainerDescriptor {
                    family: ContainerFamily::Matroska,
                    brand: "matroska".to_string(),
                    extension_hint: Some("mkv".to_string()),
                },
            },
            duration_ms: Some(1000),
            tracks,
            attachments: AttachmentSummary { count: 0 },
            capabilities: MediaCapabilities {
                can_remux_without_decode: true,
                can_segment_without_decode: true,
                requires_video_decode: false,
                requires_audio_decode: false,
                unsupported_track_ids: Vec::new(),
            },
        }
    }

    fn track(id: &str, kind: TrackKind, family: CodecFamily) -> MediaTrack {
        MediaTrack {
            id: id.to_string(),
            index: 0,
            kind,
            codec: CodecDescriptor {
                id: format!("{family:?}"),
                family,
                profile: None,
            },
            duration_ms: Some(1000),
            language: None,
            title: None,
            flags: TrackFlags {
                default: false,
                forced: false,
            },
            video: None,
            audio: None,
            subtitle: (kind == TrackKind::Subtitle).then_some(SubtitleDescriptor {
                format: match family {
                    CodecFamily::BitmapSubtitle => SubtitleFormat::Bitmap,
                    CodecFamily::TextSubtitle => SubtitleFormat::Text,
                    _ => SubtitleFormat::Unknown,
                },
            }),
        }
    }
}
