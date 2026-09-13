//! Feature-gated fuzzing harness entrypoints.
//!
//! These functions intentionally expose internal parser and mux paths only when
//! the `fuzzing` feature is enabled. They are not part of the shipping API.

use crate::packet::{ChunkSample, TimeDelta, TimePoint};

const MAX_FUZZ_BYTES: usize = 1 << 20;
const MAX_FUZZ_SAMPLES: usize = 32;

/// Exercises untrusted container sniffing and metadata/index parsing.
pub fn fuzz_containers(bytes: &[u8]) {
    let bytes = bounded(bytes);
    if crate::container::validate_metadata_budget(bytes, Default::default()).is_err() {
        return;
    }
    let _ = crate::container::sniff_container(bytes);
    let _ = crate::container::mp4::parse_basic_metadata(bytes);
    let _ = crate::container::mp4::parse_packet_track(bytes, None);
    let _ = crate::container::mp4::parse_codec_config(bytes, None);
    let _ = crate::container::matroska::parse_basic_metadata(bytes);
    let _ = crate::container::matroska::parse_chunk_plan(bytes, None, 4_000);
    let _ = crate::container::matroska::parse_packet_track(bytes, None);
}

/// Exercises compressed codec header parsing and length-prefixed sample conversion.
pub fn fuzz_codecs(bytes: &[u8]) {
    let bytes = bounded(bytes);
    let nalu_length_size = bytes.first().map(|byte| (byte % 4) + 1).unwrap_or(4);
    let _ = crate::codec::aac::parse_audio_specific_config(bytes);
    let _ = crate::codec::ac3::parse_ac3_specific_box(bytes);
    let _ = crate::codec::ac3::parse_eac3_specific_box(bytes);
    let _ = crate::codec::dts::parse_dts_core_frames(bytes);
    let _ = crate::codec::h264::parse_avc_decoder_config(bytes);
    let _ = crate::codec::h264::parse_avc_sample_nalus(0, 0, bytes, nalu_length_size);
    let _ = crate::codec::h264::avc_sample_to_annex_b(bytes, nalu_length_size);
    let _ = crate::codec::hevc::parse_hevc_decoder_config(bytes);
    let _ = crate::codec::hevc::hevc_sample_to_annex_b(bytes, nalu_length_size);
    let _ = crate::codec::hevc::sample_vcl_type(bytes, nalu_length_size);
}

/// Exercises text subtitle parsing and WebVTT segmentation.
pub fn fuzz_subtitles(bytes: &[u8]) {
    let bytes = bounded(bytes);
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    let cues = crate::codec::subtitles::parse_subrip(text);
    let _ = crate::codec::subtitles::render_webvtt(&cues);
    let _ = crate::codec::subtitles::segment_webvtt(&cues, 1_000, "seg-");
    let _ = crate::codec::subtitles::encode_mov_text_sample(text);
}

/// Exercises fMP4 fragment construction and byte-level validation.
pub fn fuzz_fmp4(bytes: &[u8]) {
    let bytes = bounded(bytes);
    // Validate attacker-controlled boxes too, not only our own valid builder output.
    let _ = crate::fmp4::validate_media_fragment(bytes);
    if bytes.is_empty() {
        return;
    }
    let sample_count = usize::from(bytes[0] % MAX_FUZZ_SAMPLES as u8).max(1);
    let payload = &bytes[1..];
    if payload.is_empty() {
        return;
    }
    let samples = contiguous_samples(payload.len(), sample_count);
    let Ok(track) =
        crate::fmp4::fragment_track_from_chunk_samples(1, &samples, payload.to_vec(), 1_000)
    else {
        return;
    };
    let Ok(fragment) = crate::fmp4::media_fragment(1, &[track]) else {
        return;
    };
    let _ = crate::fmp4::validate_media_fragment(&fragment);
}

fn bounded(bytes: &[u8]) -> &[u8] {
    &bytes[..bytes.len().min(MAX_FUZZ_BYTES)]
}

fn contiguous_samples(payload_len: usize, requested_count: usize) -> Vec<ChunkSample> {
    let sample_count = requested_count.min(payload_len).max(1);
    let base = payload_len / sample_count;
    let remainder = payload_len % sample_count;
    let mut out = Vec::with_capacity(sample_count);
    let mut offset = 0_u64;
    for index in 0..sample_count {
        let size = base + usize::from(index < remainder);
        out.push(ChunkSample {
            index: index as u32,
            payload_offset: offset,
            byte_count: size as u32,
            pts: TimePoint::millis(index as u64 * 40),
            dts: TimePoint::millis(index as u64 * 40),
            duration: TimeDelta::millis(40),
            keyframe: index == 0,
        });
        offset = offset.saturating_add(size as u64);
    }
    out
}
