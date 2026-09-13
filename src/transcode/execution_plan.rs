use serde::{Deserialize, Serialize};

use crate::{
    probe::{CodecFamily, MediaProbe, MediaTrack, TrackKind},
    session::PlaybackTarget,
    transcode::{AudioCodec, VideoCodec},
};

/// Request for a Chroma Engine-owned HLS transcode execution plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HlsTranscodeRequest {
    /// Playback target that will consume the produced HLS.
    pub target: PlaybackTarget,
    /// Optional stable audio track id to select.
    pub audio_track_id: Option<String>,
    /// Target media segment size in milliseconds.
    pub segment_target_ms: u64,
    /// Force video decode/encode even when packet copy may be playable.
    pub force_video_transcode: bool,
}

/// Chroma-native transcode execution plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TranscodeExecutionPlan {
    /// Schema version for this execution-plan document.
    pub schema_version: u32,
    /// Source path from the media probe.
    pub source_path: String,
    /// Playback target the plan is shaped for.
    pub target: PlaybackTarget,
    /// Selected source video track id.
    pub selected_video_track_id: Option<String>,
    /// Selected source audio track id.
    pub selected_audio_track_id: Option<String>,
    /// Planned HLS output properties.
    pub output: TranscodeOutputPlan,
    /// Ordered native stages required to execute this plan.
    pub stages: Vec<TranscodeStage>,
    /// True when every required stage is executable by Chroma Engine in this build.
    pub engine_executable: bool,
    /// Native capabilities that must be implemented before this plan can run fully inside Chroma Engine.
    pub missing_capabilities: Vec<String>,
    /// Human-readable planning notes.
    pub reasons: Vec<String>,
}

/// Stable output contract for a planned HLS transcode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TranscodeOutputPlan {
    /// Output transport shape.
    pub transport: String,
    /// Target segment duration in milliseconds.
    pub segment_target_ms: u64,
    /// Planned video output.
    pub video: Option<TranscodeOutputVideo>,
    /// Planned audio output.
    pub audio: Option<TranscodeOutputAudio>,
}

/// Planned video output properties.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TranscodeOutputVideo {
    /// Output video codec when encoding, or source codec when copying.
    pub codec: VideoCodec,
    /// True when source compressed packets are copied unchanged.
    pub packet_copy: bool,
    /// Target average bitrate in bits per second.
    pub bitrate_bps: u64,
    /// Output width in pixels when known.
    pub width: Option<u32>,
    /// Output height in pixels when known.
    pub height: Option<u32>,
}

/// Planned audio output properties.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TranscodeOutputAudio {
    /// Output audio codec when encoding, or source codec when copying.
    pub codec: AudioCodec,
    /// True when source compressed packets are copied unchanged.
    pub packet_copy: bool,
    /// Output channel count.
    pub channels: u32,
    /// Output sample rate in hertz.
    pub sample_rate: u32,
    /// Target average bitrate in bits per second.
    pub bitrate_bps: u64,
}

/// One executable stage in a Chroma-native transcode plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TranscodeStage {
    /// Stable stage id.
    pub id: String,
    /// Stage kind.
    pub kind: TranscodeStageKind,
    /// Source track ids consumed by the stage.
    pub track_ids: Vec<String>,
    /// Current native execution status for this stage.
    pub status: TranscodeStageStatus,
    /// Diagnostic reason for the stage decision.
    pub reason: String,
    /// Missing native capability when the stage is not executable.
    pub missing_capability: Option<String>,
}

/// Media operation represented by a transcode stage.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TranscodeStageKind {
    /// Container demux and packet scheduling.
    Demux,
    /// Compressed video packet copy.
    VideoCopy,
    /// Compressed video decode to raw frames.
    VideoDecode,
    /// Raw video encode to compressed output.
    VideoEncode,
    /// Compressed audio packet copy.
    AudioCopy,
    /// Compressed audio decode to PCM.
    AudioDecode,
    /// PCM audio encode to compressed output.
    AudioEncode,
    /// HLS/fMP4 muxing.
    HlsFmp4Mux,
}

/// Native readiness status for a transcode stage.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TranscodeStageStatus {
    /// Stage is executable by Chroma Engine in this build.
    NativeReady,
    /// Stage is a zero-decode packet copy.
    PacketCopy,
    /// Stage is planned but native implementation is not present yet.
    NativeMissing,
}

/// Builds a Chroma-native HLS transcode execution plan from a media probe.
pub fn plan_hls_transcode(
    probe: &MediaProbe,
    request: HlsTranscodeRequest,
) -> TranscodeExecutionPlan {
    let video = probe
        .tracks
        .iter()
        .find(|track| track.kind == TrackKind::Video);
    let audio = select_audio_track(probe, request.audio_track_id.as_deref(), request.target);
    let mut stages = Vec::new();
    let mut reasons = Vec::new();

    stages.push(ready_stage(
        "demux0",
        TranscodeStageKind::Demux,
        selected_ids(video, audio),
        "bounded native container probe and packet scheduling",
    ));

    let output_video =
        video.map(|track| {
            let can_copy =
                !request.force_video_transcode && video_can_copy_for_hls(track, request.target);
            if can_copy {
                stages.push(copy_stage(
                    "video-copy0",
                    TranscodeStageKind::VideoCopy,
                    vec![track.id.clone()],
                    "target can consume the compressed video packets without decode",
                ));
                reasons.push(format!("video {} stays on packet-copy path", track.id));
                TranscodeOutputVideo {
                    codec: video_codec_for_family(track.codec.family).unwrap_or(VideoCodec::H264),
                    packet_copy: true,
                    bitrate_bps: video_bitrate_bps(track, true),
                    width: track.video.as_ref().and_then(|video| video.width),
                    height: track.video.as_ref().and_then(|video| video.height),
                }
            } else {
                if track.video.as_ref().is_some_and(|video| {
                    matches!(
                        video.dynamic_range,
                        crate::probe::DynamicRange::Hdr10
                            | crate::probe::DynamicRange::Hdr10Plus
                            | crate::probe::DynamicRange::Hlg
                            | crate::probe::DynamicRange::DolbyVision
                    )
                }) {
                    stages.push(missing_stage(
                    "tone-map0", TranscodeStageKind::VideoEncode, vec![track.id.clone()],
                    "HDR-to-SDR requires verified tone mapping; use compatible compressed HDR copy",
                    "videoToneMap:hdrToSdr".into(),
                ));
                }
                if native_video_decode_ready(track.codec.family) {
                    stages.push(ready_stage(
                        "video-decode0",
                        TranscodeStageKind::VideoDecode,
                        vec![track.id.clone()],
                        native_video_decode_reason(track.codec.family),
                    ));
                } else {
                    let missing = format!("videoDecode:{}", capability_label(track.codec.family));
                    stages.push(missing_stage(
                        "video-decode0",
                        TranscodeStageKind::VideoDecode,
                        vec![track.id.clone()],
                        "compressed source video must decode before target-native HLS encode",
                        missing,
                    ));
                }
                stages.push(ready_stage(
                    "video-encode0",
                    TranscodeStageKind::VideoEncode,
                    vec![track.id.clone()],
                    "preferred native H.264 encode with portable OpenH264 fallback",
                ));
                reasons.push(format!(
                    "video {} is planned for native H.264 encode",
                    track.id
                ));
                TranscodeOutputVideo {
                    codec: VideoCodec::H264,
                    packet_copy: false,
                    bitrate_bps: video_bitrate_bps(track, false),
                    width: track.video.as_ref().and_then(|video| video.width),
                    height: track.video.as_ref().and_then(|video| video.height),
                }
            }
        });

    let output_audio = audio.map(|track| {
        let can_copy = audio_can_copy_for_hls(track, request.target);
        if can_copy {
            stages.push(copy_stage(
                "audio-copy0",
                TranscodeStageKind::AudioCopy,
                vec![track.id.clone()],
                "target can consume the compressed audio packets without decode",
            ));
            reasons.push(format!("audio {} stays on packet-copy path", track.id));
            TranscodeOutputAudio {
                codec: audio_codec_for_family(track.codec.family).unwrap_or(AudioCodec::Aac),
                packet_copy: true,
                channels: audio_channels(track),
                sample_rate: audio_sample_rate(track),
                bitrate_bps: audio_bitrate_bps(track, true),
            }
        } else {
            let target_codec = audio_transcode_codec(request.target, track.codec.family);
            if native_audio_decode_ready(track.codec.family) {
                stages.push(ready_stage(
                    "audio-decode0",
                    TranscodeStageKind::AudioDecode,
                    vec![track.id.clone()],
                    native_audio_decode_reason(track.codec.family),
                ));
            } else {
                let missing = format!("audioDecode:{}", capability_label(track.codec.family));
                stages.push(missing_stage(
                    "audio-decode0",
                    TranscodeStageKind::AudioDecode,
                    vec![track.id.clone()],
                    "compressed source audio must decode before target-native HLS encode",
                    missing,
                ));
            }
            stages.push(ready_stage(
                "audio-encode0",
                TranscodeStageKind::AudioEncode,
                vec![track.id.clone()],
                "native audio encode backend exists for the selected output codec",
            ));
            reasons.push(format!(
                "audio {} is planned for native {:?} encode",
                track.id, target_codec
            ));
            TranscodeOutputAudio {
                codec: target_codec,
                packet_copy: false,
                channels: audio_transcode_channels(track, target_codec),
                sample_rate: audio_sample_rate(track),
                bitrate_bps: audio_bitrate_bps(track, false),
            }
        }
    });

    stages.push(ready_stage(
        "hls-fmp4-mux0",
        TranscodeStageKind::HlsFmp4Mux,
        selected_ids(video, audio),
        "native HLS/fMP4 muxer owns playlists, init segments, and media fragments",
    ));

    let missing_capabilities = stages
        .iter()
        .filter_map(|stage| stage.missing_capability.clone())
        .collect::<Vec<_>>();
    let engine_executable = missing_capabilities.is_empty();

    TranscodeExecutionPlan {
        schema_version: 1,
        source_path: probe.source.path.display().to_string(),
        target: request.target,
        selected_video_track_id: video.map(|track| track.id.clone()),
        selected_audio_track_id: audio.map(|track| track.id.clone()),
        output: TranscodeOutputPlan {
            transport: "hlsFmp4".to_string(),
            segment_target_ms: request.segment_target_ms,
            video: output_video,
            audio: output_audio,
        },
        stages,
        engine_executable,
        missing_capabilities,
        reasons,
    }
}

fn select_audio_track<'a>(
    probe: &'a MediaProbe,
    requested: Option<&str>,
    target: PlaybackTarget,
) -> Option<&'a MediaTrack> {
    let tracks = probe
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Audio)
        .collect::<Vec<_>>();
    if let Some(requested) = requested {
        return tracks.into_iter().find(|track| track.id == requested);
    }

    tracks
        .iter()
        .copied()
        .find(|track| track.flags.default && audio_can_copy_for_hls(track, target))
        .or_else(|| {
            tracks
                .iter()
                .copied()
                .find(|track| audio_can_copy_for_hls(track, target))
        })
        .or_else(|| {
            tracks
                .iter()
                .copied()
                .find(|track| track.flags.default && native_audio_decode_ready(track.codec.family))
        })
        .or_else(|| {
            tracks
                .iter()
                .copied()
                .find(|track| native_audio_decode_ready(track.codec.family))
        })
        .or_else(|| tracks.iter().copied().find(|track| track.flags.default))
        .or_else(|| tracks.first().copied())
}

fn video_can_copy_for_hls(track: &MediaTrack, target: PlaybackTarget) -> bool {
    match target {
        PlaybackTarget::NativeChroma | PlaybackTarget::AppleNative => {
            matches!(track.codec.family, CodecFamily::H264 | CodecFamily::Hevc)
        }
        PlaybackTarget::Browser => matches!(track.codec.family, CodecFamily::H264),
    }
}

fn native_video_decode_ready(family: CodecFamily) -> bool {
    matches!(
        family,
        CodecFamily::H264 | CodecFamily::Hevc | CodecFamily::Av1
    )
}

fn native_video_decode_reason(family: CodecFamily) -> &'static str {
    match family {
        CodecFamily::H264 => {
            "preferred native H.264 decode with portable OpenH264 fallback emits BGRA frames"
        }
        CodecFamily::Hevc => {
            "preferred native HEVC decode with portable safe-Rust fallback emits BGRA frames"
        }
        CodecFamily::Av1 => "portable dav1d AV1 decode emits BGRA frames",
        _ => "native video decode emits BGRA frames",
    }
}

fn native_audio_decode_ready(family: CodecFamily) -> bool {
    matches!(
        family,
        CodecFamily::Opus | CodecFamily::TrueHd | CodecFamily::Dts
    )
}

fn native_audio_decode_reason(family: CodecFamily) -> &'static str {
    match family {
        CodecFamily::Opus => {
            "portable pure-Rust Opus decode can feed the target-native audio bridge"
        }
        CodecFamily::TrueHd => "portable TrueHD decode can feed the target-native audio bridge",
        CodecFamily::Dts => "portable DTS Core decode can feed the target-native audio bridge",
        _ => "portable audio decode can feed the target-native audio bridge",
    }
}

fn audio_can_copy_for_hls(track: &MediaTrack, target: PlaybackTarget) -> bool {
    match target {
        PlaybackTarget::NativeChroma => matches!(
            track.codec.family,
            CodecFamily::Aac | CodecFamily::Ac3 | CodecFamily::Eac3 | CodecFamily::Mp3
        ),
        PlaybackTarget::AppleNative => matches!(
            track.codec.family,
            CodecFamily::Aac | CodecFamily::Ac3 | CodecFamily::Eac3 | CodecFamily::Mp3
        ),
        PlaybackTarget::Browser => {
            matches!(track.codec.family, CodecFamily::Aac | CodecFamily::Mp3)
        }
    }
}

fn video_codec_for_family(family: CodecFamily) -> Option<VideoCodec> {
    match family {
        CodecFamily::H264 => Some(VideoCodec::H264),
        CodecFamily::Hevc => Some(VideoCodec::Hevc),
        CodecFamily::Av1 => Some(VideoCodec::Av1),
        _ => None,
    }
}

fn audio_codec_for_family(family: CodecFamily) -> Option<AudioCodec> {
    match family {
        CodecFamily::Aac => Some(AudioCodec::Aac),
        CodecFamily::Ac3 => Some(AudioCodec::Ac3),
        CodecFamily::Eac3 => Some(AudioCodec::Eac3),
        _ => None,
    }
}

fn audio_transcode_codec(target: PlaybackTarget, _family: CodecFamily) -> AudioCodec {
    match target {
        PlaybackTarget::Browser => AudioCodec::Aac,
        PlaybackTarget::AppleNative | PlaybackTarget::NativeChroma => AudioCodec::Aac,
    }
}

fn audio_transcode_channels(track: &MediaTrack, codec: AudioCodec) -> u32 {
    match codec {
        AudioCodec::Aac
            if matches!(
                track.codec.family,
                CodecFamily::Opus | CodecFamily::TrueHd | CodecFamily::Dts
            ) =>
        {
            audio_channels(track).min(6)
        }
        AudioCodec::Aac => audio_channels(track).min(2),
        AudioCodec::Ac3 | AudioCodec::Eac3 => audio_channels(track).min(6),
    }
}

fn video_bitrate_bps(track: &MediaTrack, packet_copy: bool) -> u64 {
    let fallback = || {
        let pixels = track
            .video
            .as_ref()
            .and_then(|video| video.width.zip(video.height))
            .map(|(width, height)| u64::from(width) * u64::from(height))
            .unwrap_or(1_920 * 1_080);
        if pixels >= 3_840 * 2_160 {
            16_000_000
        } else if pixels >= 1_920 * 1_080 {
            8_000_000
        } else if pixels >= 1_280 * 720 {
            4_500_000
        } else {
            2_500_000
        }
    };
    let source = track
        .video
        .as_ref()
        .and_then(|video| video.bitrate_bps)
        .unwrap_or_else(fallback);
    if packet_copy {
        source
    } else {
        source.min(fallback())
    }
}

fn audio_channels(track: &MediaTrack) -> u32 {
    track
        .audio
        .as_ref()
        .and_then(|audio| audio.channels)
        .unwrap_or(2)
}

fn audio_sample_rate(track: &MediaTrack) -> u32 {
    track
        .audio
        .as_ref()
        .and_then(|audio| audio.sample_rate)
        .unwrap_or(48_000)
}

fn audio_bitrate_bps(track: &MediaTrack, packet_copy: bool) -> u64 {
    if packet_copy {
        return track
            .audio
            .as_ref()
            .and_then(|audio| audio.bitrate_bps)
            .unwrap_or(384_000);
    }
    match audio_channels(track).min(6) {
        0..=2 => 192_000,
        3..=6 => 640_000,
        _ => 640_000,
    }
}

fn selected_ids(video: Option<&MediaTrack>, audio: Option<&MediaTrack>) -> Vec<String> {
    video
        .into_iter()
        .chain(audio)
        .map(|track| track.id.clone())
        .collect()
}

fn ready_stage(
    id: &str,
    kind: TranscodeStageKind,
    track_ids: Vec<String>,
    reason: &str,
) -> TranscodeStage {
    TranscodeStage {
        id: id.to_string(),
        kind,
        track_ids,
        status: TranscodeStageStatus::NativeReady,
        reason: reason.to_string(),
        missing_capability: None,
    }
}

fn copy_stage(
    id: &str,
    kind: TranscodeStageKind,
    track_ids: Vec<String>,
    reason: &str,
) -> TranscodeStage {
    TranscodeStage {
        id: id.to_string(),
        kind,
        track_ids,
        status: TranscodeStageStatus::PacketCopy,
        reason: reason.to_string(),
        missing_capability: None,
    }
}

fn missing_stage(
    id: &str,
    kind: TranscodeStageKind,
    track_ids: Vec<String>,
    reason: &str,
    missing_capability: String,
) -> TranscodeStage {
    TranscodeStage {
        id: id.to_string(),
        kind,
        track_ids,
        status: TranscodeStageStatus::NativeMissing,
        reason: reason.to_string(),
        missing_capability: Some(missing_capability),
    }
}

fn capability_label(family: CodecFamily) -> &'static str {
    match family {
        CodecFamily::H264 => "h264",
        CodecFamily::Hevc => "hevc",
        CodecFamily::Av1 => "av1",
        CodecFamily::Vp9 => "vp9",
        CodecFamily::Aac => "aac",
        CodecFamily::Ac3 => "ac3",
        CodecFamily::Eac3 => "eac3",
        CodecFamily::Dts => "dts",
        CodecFamily::TrueHd => "truehd",
        CodecFamily::Flac => "flac",
        CodecFamily::Alac => "alac",
        CodecFamily::Opus => "opus",
        CodecFamily::Mp3 => "mp3",
        CodecFamily::TextSubtitle => "textSubtitle",
        CodecFamily::BitmapSubtitle => "bitmapSubtitle",
        CodecFamily::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::probe::{
        AttachmentSummary, AudioDescriptor, CodecDescriptor, ContainerDescriptor, ContainerFamily,
        DynamicRange, MediaCapabilities, MediaProbe, MediaSource, MediaTrack, ProbeEngine,
        TrackFlags, VideoDescriptor,
    };

    use super::*;

    #[test]
    fn h264_aac_browser_plan_is_engine_executable_copy_hls() {
        let probe = probe_with_tracks(vec![
            video_track("v0", CodecFamily::H264, 1_920, 1_080, 8_000_000, None),
            audio_track("a0", CodecFamily::Aac, true, 2, 48_000, Some(192_000)),
        ]);

        let plan = plan_hls_transcode(&probe, request(PlaybackTarget::Browser, false, None));

        assert!(plan.engine_executable);
        assert!(plan.missing_capabilities.is_empty());
        assert!(plan.output.video.as_ref().unwrap().packet_copy);
        assert!(plan.output.audio.as_ref().unwrap().packet_copy);
    }

    #[test]
    fn hdr_transcode_is_not_advertised_without_tone_mapping() {
        let mut video = video_track("v0", CodecFamily::Hevc, 3840, 2160, 20_000_000, None);
        video.video.as_mut().unwrap().dynamic_range = crate::probe::DynamicRange::Hdr10;
        let probe = probe_with_tracks(vec![
            video,
            audio_track("a0", CodecFamily::Aac, true, 2, 48_000, Some(192_000)),
        ]);
        let transcode = plan_hls_transcode(&probe, request(PlaybackTarget::Browser, true, None));
        assert!(!transcode.engine_executable);
        assert!(
            transcode
                .missing_capabilities
                .iter()
                .any(|capability| capability == "videoToneMap:hdrToSdr")
        );
        let copy = plan_hls_transcode(&probe, request(PlaybackTarget::AppleNative, false, None));
        assert!(copy.output.video.unwrap().packet_copy);
    }

    #[test]
    fn forced_h264_transcode_is_portable_with_copyable_audio() {
        let probe = probe_with_tracks(vec![
            video_track("v0", CodecFamily::H264, 1_920, 1_080, 8_000_000, None),
            audio_track("a0", CodecFamily::Aac, true, 2, 48_000, Some(192_000)),
        ]);

        let plan = plan_hls_transcode(&probe, request(PlaybackTarget::Browser, true, None));

        assert!(plan.engine_executable);
        assert!(!plan.output.video.as_ref().unwrap().packet_copy);
        assert!(plan.stages.iter().any(|stage| {
            stage.kind == TranscodeStageKind::VideoDecode
                && stage.status == TranscodeStageStatus::NativeReady
                && stage.reason.contains("OpenH264")
        }));
        assert!(plan.stages.iter().any(|stage| {
            stage.kind == TranscodeStageKind::VideoEncode
                && stage.status == TranscodeStageStatus::NativeReady
                && stage.reason.contains("OpenH264")
        }));
    }

    #[test]
    fn forced_apple_hevc_transcode_reports_video_decode_readiness() {
        let probe = probe_with_tracks(vec![
            video_track(
                "v0",
                CodecFamily::Hevc,
                3_840,
                2_160,
                24_000_000,
                Some("H153"),
            ),
            audio_track("a0", CodecFamily::Eac3, true, 8, 48_000, Some(768_000)),
        ]);

        let plan = plan_hls_transcode(&probe, request(PlaybackTarget::AppleNative, true, None));

        assert_eq!(plan.output.video.as_ref().unwrap().codec, VideoCodec::H264);
        assert!(!plan.output.video.as_ref().unwrap().packet_copy);
        assert_eq!(plan.output.video.as_ref().unwrap().bitrate_bps, 16_000_000);
        assert!(plan.engine_executable);
        assert!(plan.stages.iter().any(|stage| {
            stage.kind == TranscodeStageKind::VideoDecode
                && stage.status == TranscodeStageStatus::NativeReady
        }));
    }

    #[test]
    fn av1_transcode_uses_native_dav1d_decode() {
        let probe = probe_with_tracks(vec![
            video_track("v0", CodecFamily::Av1, 3_840, 2_160, 18_000_000, None),
            audio_track("a0", CodecFamily::Aac, true, 2, 48_000, Some(192_000)),
        ]);

        let plan = plan_hls_transcode(&probe, request(PlaybackTarget::AppleNative, false, None));

        assert!(plan.engine_executable);
        assert_eq!(plan.output.video.as_ref().unwrap().codec, VideoCodec::H264);
        assert!(!plan.output.video.as_ref().unwrap().packet_copy);
        assert!(plan.stages.iter().any(|stage| {
            stage.kind == TranscodeStageKind::VideoDecode
                && stage.status == TranscodeStageStatus::NativeReady
                && stage.reason.contains("dav1d")
        }));
    }

    #[test]
    fn dts_audio_uses_portable_decode_and_aac_encode() {
        let probe = probe_with_tracks(vec![
            video_track("v0", CodecFamily::H264, 1_920, 1_080, 8_000_000, None),
            audio_track("a0", CodecFamily::Dts, true, 6, 48_000, Some(1_536_000)),
        ]);

        let plan = plan_hls_transcode(&probe, request(PlaybackTarget::AppleNative, false, None));

        assert!(plan.engine_executable);
        assert!(plan.missing_capabilities.is_empty());
        let audio = plan.output.audio.as_ref().unwrap();
        assert_eq!(audio.codec, AudioCodec::Aac);
        assert_eq!(audio.channels, 6);
        assert!(plan.stages.iter().any(|stage| {
            stage.kind == TranscodeStageKind::AudioDecode
                && stage.status == TranscodeStageStatus::NativeReady
                && stage.reason.contains("portable DTS Core")
        }));
    }

    #[test]
    fn opus_audio_uses_pure_rust_decode_and_aac_encode() {
        let probe = probe_with_tracks(vec![
            video_track("v0", CodecFamily::H264, 1_920, 1_080, 8_000_000, None),
            audio_track("a0", CodecFamily::Opus, true, 6, 48_000, Some(384_000)),
        ]);

        let plan = plan_hls_transcode(&probe, request(PlaybackTarget::AppleNative, false, None));

        assert!(plan.engine_executable);
        let audio = plan.output.audio.as_ref().unwrap();
        assert_eq!(audio.codec, AudioCodec::Aac);
        assert_eq!(audio.channels, 6);
        assert!(plan.stages.iter().any(|stage| {
            stage.kind == TranscodeStageKind::AudioDecode
                && stage.status == TranscodeStageStatus::NativeReady
                && stage.reason.contains("pure-Rust Opus")
        }));
    }

    #[test]
    fn truehd_audio_uses_portable_decode_and_aac_encode() {
        let probe = probe_with_tracks(vec![
            video_track("v0", CodecFamily::H264, 1_920, 1_080, 8_000_000, None),
            audio_track("a0", CodecFamily::TrueHd, true, 8, 48_000, Some(4_000_000)),
        ]);

        let plan = plan_hls_transcode(&probe, request(PlaybackTarget::Browser, false, None));

        assert!(plan.engine_executable);
        assert!(plan.missing_capabilities.is_empty());
        let audio = plan.output.audio.as_ref().unwrap();
        assert_eq!(audio.codec, AudioCodec::Aac);
        assert!(!audio.packet_copy);
        assert_eq!(audio.channels, 6);
        assert!(plan.stages.iter().any(|stage| {
            stage.kind == TranscodeStageKind::AudioDecode
                && stage.status == TranscodeStageStatus::NativeReady
                && stage.reason.contains("portable TrueHD")
        }));
    }

    #[test]
    fn primary_audio_prefers_target_copyable_track() {
        let probe = probe_with_tracks(vec![
            video_track("v0", CodecFamily::H264, 1_920, 1_080, 8_000_000, None),
            audio_track("a0", CodecFamily::Dts, true, 6, 48_000, Some(1_536_000)),
            audio_track("a1", CodecFamily::Ac3, false, 6, 48_000, Some(448_000)),
        ]);

        let plan = plan_hls_transcode(&probe, request(PlaybackTarget::AppleNative, false, None));

        assert_eq!(plan.selected_audio_track_id.as_deref(), Some("a1"));
        assert!(plan.engine_executable);
        assert_eq!(plan.output.audio.as_ref().unwrap().codec, AudioCodec::Ac3);
    }

    #[test]
    fn primary_audio_keeps_executable_default_dts() {
        let probe = probe_with_tracks(vec![
            video_track("v0", CodecFamily::H264, 1_920, 1_080, 8_000_000, None),
            audio_track("a0", CodecFamily::Dts, true, 6, 48_000, Some(1_536_000)),
            audio_track("a1", CodecFamily::TrueHd, false, 8, 48_000, Some(4_000_000)),
        ]);

        let plan = plan_hls_transcode(&probe, request(PlaybackTarget::Browser, false, None));

        assert_eq!(plan.selected_audio_track_id.as_deref(), Some("a0"));
        assert!(plan.engine_executable);
        assert_eq!(plan.output.audio.as_ref().unwrap().codec, AudioCodec::Aac);
    }

    #[test]
    fn explicit_audio_track_overrides_copyable_preference() {
        let probe = probe_with_tracks(vec![
            video_track("v0", CodecFamily::H264, 1_920, 1_080, 8_000_000, None),
            audio_track("a0", CodecFamily::Dts, true, 6, 48_000, Some(1_536_000)),
            audio_track("a1", CodecFamily::Ac3, false, 6, 48_000, Some(448_000)),
        ]);

        let plan = plan_hls_transcode(
            &probe,
            request(PlaybackTarget::AppleNative, false, Some("a0".to_string())),
        );

        assert_eq!(plan.selected_audio_track_id.as_deref(), Some("a0"));
        assert_eq!(plan.output.audio.as_ref().unwrap().codec, AudioCodec::Aac);
        assert!(plan.engine_executable);
        assert!(plan.missing_capabilities.is_empty());
    }

    fn request(
        target: PlaybackTarget,
        force_video_transcode: bool,
        audio_track_id: Option<String>,
    ) -> HlsTranscodeRequest {
        HlsTranscodeRequest {
            target,
            audio_track_id,
            segment_target_ms: 4_000,
            force_video_transcode,
        }
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

    fn video_track(
        id: &str,
        family: CodecFamily,
        width: u32,
        height: u32,
        bitrate_bps: u64,
        profile: Option<&str>,
    ) -> MediaTrack {
        MediaTrack {
            id: id.to_string(),
            index: 0,
            kind: TrackKind::Video,
            codec: CodecDescriptor {
                id: format!("{family:?}"),
                family,
                profile: profile.map(str::to_string),
            },
            duration_ms: Some(1000),
            language: None,
            title: None,
            flags: TrackFlags {
                default: true,
                forced: false,
            },
            video: Some(VideoDescriptor {
                width: Some(width),
                height: Some(height),
                frame_rate: Some(23.976),
                bitrate_bps: Some(bitrate_bps),
                pixel_format: Some("yuv420p".to_string()),
                dynamic_range: DynamicRange::Sdr,
            }),
            audio: None,
            subtitle: None,
        }
    }

    fn audio_track(
        id: &str,
        family: CodecFamily,
        default: bool,
        channels: u32,
        sample_rate: u32,
        bitrate_bps: Option<u64>,
    ) -> MediaTrack {
        MediaTrack {
            id: id.to_string(),
            index: 0,
            kind: TrackKind::Audio,
            codec: CodecDescriptor {
                id: format!("{family:?}"),
                family,
                profile: None,
            },
            duration_ms: Some(1000),
            language: Some("eng".to_string()),
            title: None,
            flags: TrackFlags {
                default,
                forced: false,
            },
            video: None,
            audio: Some(AudioDescriptor {
                channels: Some(channels),
                sample_rate: Some(sample_rate),
                bitrate_bps,
                atmos: false,
                object_audio_candidate: false,
                lossless: matches!(
                    family,
                    CodecFamily::TrueHd | CodecFamily::Flac | CodecFamily::Alac
                ),
            }),
            subtitle: None,
        }
    }
}
