use serde::{Deserialize, Serialize};

use crate::probe::{CodecFamily, MediaProbe, MediaTrack, SubtitleFormat, TrackKind};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
/// Playback pipeline plan for a probed media source.
pub struct PlaybackPlan {
    /// Playback plan schema version.
    pub schema_version: u32,
    /// Source path being planned.
    pub source_path: String,
    /// Source duration in milliseconds when known.
    pub duration_ms: Option<u64>,
    /// Track identifiers selected for playback.
    pub selected_tracks: Vec<String>,
    /// Ordered processing stages.
    pub stages: Vec<PipelineStage>,
    /// Output transports exposed by the plan.
    pub transports: Vec<TransportPlan>,
    /// Constraints used to produce the plan.
    pub constraints: PlaybackConstraints,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Multi-audio output fanout derived from a playback plan.
pub struct MultiAudioOutputPlan {
    /// Selected video track when the plan includes video.
    pub video_track_id: Option<String>,
    /// Shared stage that emits the selected video representation.
    pub shared_video_stage_id: Option<String>,
    /// One output per selected audio track.
    pub audio_outputs: Vec<AudioOutputPlan>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// One selected audio output in a multi-audio fanout plan.
pub struct AudioOutputPlan {
    /// Selected audio track id.
    pub audio_track_id: String,
    /// Stage that emits this audio representation.
    pub audio_stage_id: String,
    /// Shared video stage used by this output.
    pub shared_video_stage_id: Option<String>,
    /// Whether serving this audio output requires another video encode.
    pub duplicates_video_encode: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Constraints provided by a playback client or server policy.
pub struct PlaybackConstraints {
    /// Playback target type.
    pub target: PlaybackTarget,
    /// Optional maximum decoded video pixel count.
    pub max_video_pixels: Option<u64>,
    /// Whether packet-copy paths should be preferred.
    pub prefer_copy: bool,
    /// Audio track selection policy.
    pub audio_selection: AudioSelection,
    /// Whether subtitles should be included.
    pub include_subtitles: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Playback target family.
pub enum PlaybackTarget {
    /// Chroma-native client transport.
    NativeChroma,
    /// Browser playback target.
    Browser,
    /// Apple-native player target.
    AppleNative,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Audio track selection policy.
pub enum AudioSelection {
    /// Select the primary compatible audio track.
    Primary,
    /// Select every compatible audio track.
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// One stage in a playback pipeline.
pub struct PipelineStage {
    /// Stable stage identifier.
    pub id: String,
    /// Stage responsibility.
    pub kind: StageKind,
    /// Track ids handled by the stage.
    pub track_ids: Vec<String>,
    /// Stage execution mode.
    pub mode: StageMode,
    /// Diagnostic explanation for why the stage exists.
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Playback pipeline stage kind.
pub enum StageKind {
    /// Container demux.
    Demux,
    /// Compressed packet filtering or copying.
    PacketFilter,
    /// Decode to raw media frames.
    Decode,
    /// Encode raw frames to target codecs.
    Encode,
    /// Subtitle normalization or segmentation.
    SubtitleTransform,
    /// Multiplex selected tracks into transport output.
    Mux,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Execution mode for a pipeline stage.
pub enum StageMode {
    /// Shared stage for multiple tracks.
    Shared,
    /// Packet-copy stage.
    Copy,
    /// Metadata or subtitle transform stage.
    Transform,
    /// Decode stage.
    Decode,
    /// Encode stage.
    Encode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Transport emitted by a playback plan.
pub struct TransportPlan {
    /// Stable transport identifier.
    pub id: String,
    /// Transport family.
    pub kind: TransportKind,
    /// Stage id that feeds this transport.
    pub source_stage_id: String,
    /// Diagnostic notes for client/server selection.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Output transport family.
pub enum TransportKind {
    /// Chroma-native frame transport.
    ChromaFrames,
    /// Chroma-native segment transport.
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

/// Plans a playback graph from probe metadata and client constraints.
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

/// Plans multi-audio output fanout without duplicating selected video work.
pub fn plan_multi_audio_outputs(plan: &PlaybackPlan, probe: &MediaProbe) -> MultiAudioOutputPlan {
    let video_track_id = plan
        .selected_tracks
        .iter()
        .find(|id| {
            probe
                .tracks
                .iter()
                .any(|track| track.id == **id && track.kind == TrackKind::Video)
        })
        .cloned();
    let shared_video_stage_id = video_track_id
        .as_deref()
        .and_then(|track_id| output_stage_for_track(&plan.stages, track_id));
    let audio_outputs = plan
        .selected_tracks
        .iter()
        .filter(|id| {
            probe
                .tracks
                .iter()
                .any(|track| track.id == **id && track.kind == TrackKind::Audio)
        })
        .filter_map(|audio_track_id| {
            let audio_stage_id = output_stage_for_track(&plan.stages, audio_track_id)?;
            Some(AudioOutputPlan {
                audio_track_id: audio_track_id.clone(),
                audio_stage_id,
                shared_video_stage_id: shared_video_stage_id.clone(),
                duplicates_video_encode: false,
            })
        })
        .collect();

    MultiAudioOutputPlan {
        video_track_id,
        shared_video_stage_id,
        audio_outputs,
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
        .find(|track| track.flags.default && track_can_copy_for_target(track, target))
        .or_else(|| {
            audio_tracks
                .iter()
                .copied()
                .find(|track| track_can_copy_for_target(track, target))
        })
        .or_else(|| {
            audio_tracks.iter().copied().find(|track| {
                track.flags.default && audio_can_transcode_for_target(track.codec.family, target)
            })
        })
        .or_else(|| {
            audio_tracks
                .iter()
                .copied()
                .find(|track| audio_can_transcode_for_target(track.codec.family, target))
        })
        .or_else(|| {
            audio_tracks
                .iter()
                .copied()
                .find(|track| track.flags.default)
        })
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

fn output_stage_for_track(stages: &[PipelineStage], track_id: &str) -> Option<String> {
    stages
        .iter()
        .rev()
        .find(|stage| {
            matches!(
                stage.kind,
                StageKind::PacketFilter | StageKind::Encode | StageKind::SubtitleTransform
            ) && stage.track_ids.iter().any(|id| id == track_id)
        })
        .map(|stage| stage.id.clone())
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

fn audio_can_transcode_for_target(family: CodecFamily, target: PlaybackTarget) -> bool {
    target != PlaybackTarget::NativeChroma
        && matches!(family, CodecFamily::TrueHd | CodecFamily::Dts)
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
    fn native_chroma_plan_copies_extended_audio_codecs() {
        for family in [
            CodecFamily::Ac3,
            CodecFamily::Eac3,
            CodecFamily::Mp3,
            CodecFamily::Flac,
            CodecFamily::Alac,
        ] {
            let probe = probe_with_tracks(vec![
                track("v0", TrackKind::Video, CodecFamily::H264),
                track("a0", TrackKind::Audio, family),
            ]);
            let plan = plan_playback(&probe, PlaybackConstraints::default());
            let copy = plan
                .stages
                .iter()
                .find(|stage| stage.id == "packet-copy0")
                .expect("copy stage");

            assert!(
                copy.track_ids.contains(&"a0".to_string()),
                "{family:?} should stay on the packet-copy path"
            );
            assert!(
                !plan
                    .stages
                    .iter()
                    .any(|stage| stage.kind == StageKind::Decode),
                "{family:?} should not require native Chroma decode"
            );
        }
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
    fn primary_audio_prefers_copyable_track_before_decode_bridge() {
        let probe = probe_with_tracks(vec![
            track("v0", TrackKind::Video, CodecFamily::H264),
            track("a0", TrackKind::Audio, CodecFamily::Dts),
            track("a1", TrackKind::Audio, CodecFamily::Aac),
        ]);
        let plan = plan_playback(
            &probe,
            PlaybackConstraints {
                target: PlaybackTarget::Browser,
                ..PlaybackConstraints::default()
            },
        );

        assert_eq!(
            plan.selected_tracks,
            vec!["v0".to_string(), "a1".to_string()]
        );
        let copy = plan
            .stages
            .iter()
            .find(|stage| stage.id == "packet-copy0")
            .expect("copy stage");
        assert!(copy.track_ids.contains(&"a1".to_string()));
        assert!(!plan.stages.iter().any(|stage| stage.id == "decode0"));
    }

    #[test]
    fn primary_audio_uses_decode_bridge_when_no_copyable_track_exists() {
        let probe = probe_with_tracks(vec![
            track("v0", TrackKind::Video, CodecFamily::H264),
            track("a0", TrackKind::Audio, CodecFamily::Dts),
            default_track("a1", TrackKind::Audio, CodecFamily::TrueHd),
        ]);
        let plan = plan_playback(
            &probe,
            PlaybackConstraints {
                target: PlaybackTarget::Browser,
                ..PlaybackConstraints::default()
            },
        );

        assert_eq!(
            plan.selected_tracks,
            vec!["v0".to_string(), "a1".to_string()]
        );
        let decode = plan
            .stages
            .iter()
            .find(|stage| stage.id == "decode0")
            .expect("decode stage");
        assert_eq!(decode.track_ids, vec!["a1".to_string()]);
    }

    #[test]
    fn primary_audio_keeps_executable_default_dts_bridge() {
        let probe = probe_with_tracks(vec![
            track("v0", TrackKind::Video, CodecFamily::H264),
            default_track("a0", TrackKind::Audio, CodecFamily::Dts),
            track("a1", TrackKind::Audio, CodecFamily::TrueHd),
        ]);
        let plan = plan_playback(
            &probe,
            PlaybackConstraints {
                target: PlaybackTarget::Browser,
                ..PlaybackConstraints::default()
            },
        );

        assert_eq!(
            plan.selected_tracks,
            vec!["v0".to_string(), "a0".to_string()]
        );
        let decode = plan
            .stages
            .iter()
            .find(|stage| stage.id == "decode0")
            .expect("decode stage");
        assert_eq!(decode.track_ids, vec!["a0".to_string()]);
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

    #[test]
    fn multi_audio_outputs_share_one_video_stage() {
        let probe = probe_with_tracks(vec![
            track("v0", TrackKind::Video, CodecFamily::H264),
            track("a0", TrackKind::Audio, CodecFamily::Aac),
            track("a1", TrackKind::Audio, CodecFamily::Mp3),
        ]);
        let plan = plan_playback(
            &probe,
            PlaybackConstraints {
                audio_selection: AudioSelection::All,
                ..PlaybackConstraints::default()
            },
        );

        let fanout = plan_multi_audio_outputs(&plan, &probe);

        assert_eq!(fanout.video_track_id.as_deref(), Some("v0"));
        assert_eq!(
            fanout.shared_video_stage_id.as_deref(),
            Some("packet-copy0")
        );
        assert_eq!(fanout.audio_outputs.len(), 2);
        assert!(
            fanout
                .audio_outputs
                .iter()
                .all(
                    |output| output.shared_video_stage_id.as_deref() == Some("packet-copy0")
                        && !output.duplicates_video_encode
                )
        );
    }

    #[test]
    fn multi_audio_outputs_do_not_duplicate_encoded_video_stage() {
        let probe = probe_with_tracks(vec![
            track("v0", TrackKind::Video, CodecFamily::Hevc),
            track("a0", TrackKind::Audio, CodecFamily::Aac),
            track("a1", TrackKind::Audio, CodecFamily::Mp3),
        ]);
        let plan = plan_playback(
            &probe,
            PlaybackConstraints {
                target: PlaybackTarget::Browser,
                audio_selection: AudioSelection::All,
                ..PlaybackConstraints::default()
            },
        );

        let fanout = plan_multi_audio_outputs(&plan, &probe);

        assert_eq!(fanout.shared_video_stage_id.as_deref(), Some("encode0"));
        assert_eq!(fanout.audio_outputs.len(), 2);
        assert!(
            fanout
                .audio_outputs
                .iter()
                .all(
                    |output| output.shared_video_stage_id.as_deref() == Some("encode0")
                        && !output.duplicates_video_encode
                )
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
            chapters: Vec::new(),
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
        track_with_default(id, kind, family, false)
    }

    fn default_track(id: &str, kind: TrackKind, family: CodecFamily) -> MediaTrack {
        track_with_default(id, kind, family, true)
    }

    fn track_with_default(
        id: &str,
        kind: TrackKind,
        family: CodecFamily,
        default: bool,
    ) -> MediaTrack {
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
                default,
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
