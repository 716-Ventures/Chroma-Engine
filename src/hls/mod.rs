use std::{
    fs::{create_dir_all, write},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    codec::{
        aac::{adts_header, parse_audio_specific_config, AacAudioSpecificConfig},
        h264::{avc_sample_to_annex_b, parse_avc_decoder_config, AvcParameterSets},
    },
    container::{
        matroska::{self, MatroskaTrackKind},
        mp4::{self, Mp4TrackKind},
    },
    packet::PacketRef,
};

const VIDEO_PID: u16 = 0x0100;
const AUDIO_PID: u16 = 0x0101;
const PMT_PID: u16 = 0x1000;
const VIDEO_STREAM_ID: u8 = 0xe0;
const AUDIO_STREAM_ID: u8 = 0xc0;
const TS_CLOCK: u64 = 90_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HlsOptions {
    pub segment_target_ms: u64,
}

impl Default for HlsOptions {
    fn default() -> Self {
        Self {
            segment_target_ms: 4_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HlsOutput {
    pub master_playlist: PathBuf,
    pub media_playlist: PathBuf,
    pub segment_count: usize,
    pub target_duration_seconds: u64,
    pub video_codec: String,
    pub audio_codec: String,
}

#[derive(Debug, Clone)]
struct HlsTrackSet {
    video: HlsTrack,
    audio: HlsTrack,
}

#[derive(Debug, Clone)]
struct HlsTrack {
    codec_string: String,
    packets: Vec<PacketRef>,
    payload: PayloadKind,
}

#[derive(Debug, Clone)]
enum PayloadKind {
    Avc {
        nalu_length_size: u8,
        parameter_sets: AvcParameterSets,
    },
    Aac {
        config: AacAudioSpecificConfig,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SegmentWindow {
    index: usize,
    start_ms: u64,
    end_ms: u64,
}

#[derive(Debug, Clone)]
struct TimedPayload {
    pts90: u64,
    dts90: u64,
    bytes: Vec<u8>,
}

pub fn write_hls_vod(input: &Path, output_dir: &Path, options: HlsOptions) -> Result<HlsOutput> {
    let bytes = std::fs::read(input).with_context(|| format!("read {}", input.display()))?;
    let tracks = if mp4::looks_like_mp4(&bytes) {
        hls_tracks_from_mp4(&bytes)?
    } else if matroska::looks_like_ebml(&bytes) {
        hls_tracks_from_matroska(&bytes)?
    } else {
        bail!("native HLS currently supports MP4/MOV and Matroska/WebM sources");
    };

    if tracks.video.packets.is_empty() || tracks.audio.packets.is_empty() {
        bail!(
            "native HLS requires one packet-indexed video track and one packet-indexed audio track"
        );
    }

    let segment_target_ms = options.segment_target_ms.max(500);
    let windows = segment_windows(&tracks.video.packets, segment_target_ms);
    if windows.is_empty() {
        bail!("native HLS could not build keyframe-aligned segment windows");
    }

    create_dir_all(output_dir)?;
    let variant_dir = output_dir.join("0");
    create_dir_all(&variant_dir)?;

    let mut durations = Vec::with_capacity(windows.len());
    for window in &windows {
        let segment = mux_segment(&bytes, &tracks, *window)?;
        let duration_ms = window.end_ms.saturating_sub(window.start_ms).max(1);
        durations.push(duration_ms);
        write(variant_dir.join(segment_name(window.index)), segment)?;
    }

    let target_duration_seconds = durations
        .iter()
        .map(|duration| duration.saturating_add(999) / 1000)
        .max()
        .unwrap_or(1)
        .max(1);
    let media_playlist = variant_dir.join("playlist.m3u8");
    write(
        &media_playlist,
        media_playlist_body(target_duration_seconds, &durations),
    )?;
    let master_playlist = output_dir.join("master.m3u8");
    write(
        &master_playlist,
        master_playlist_body(&tracks.video.codec_string, &tracks.audio.codec_string),
    )?;

    Ok(HlsOutput {
        master_playlist,
        media_playlist,
        segment_count: windows.len(),
        target_duration_seconds,
        video_codec: tracks.video.codec_string,
        audio_codec: tracks.audio.codec_string,
    })
}

fn hls_tracks_from_mp4(bytes: &[u8]) -> Result<HlsTrackSet> {
    let meta = mp4::parse_basic_metadata(bytes);
    meta.tracks
        .iter()
        .find(|track| track.kind == Mp4TrackKind::Video && track.codec == "h264")
        .ok_or_else(|| anyhow!("native HLS MP4 path currently requires H.264 video"))?;
    meta.tracks
        .iter()
        .find(|track| track.kind == Mp4TrackKind::Audio && track.codec == "aac")
        .ok_or_else(|| anyhow!("native HLS MP4 path currently requires AAC audio"))?;
    let video_config = mp4::parse_codec_config(bytes, Some("v0"))
        .ok_or_else(|| anyhow!("missing MP4 H.264 decoder config"))?;
    let audio_config = mp4::parse_codec_config(bytes, Some("a0"))
        .ok_or_else(|| anyhow!("missing MP4 AAC decoder config"))?;
    let avc = hex_to_bytes(
        video_config
            .description_hex
            .as_deref()
            .ok_or_else(|| anyhow!("missing avcC"))?,
    )?;
    let asc = hex_to_bytes(
        audio_config
            .description_hex
            .as_deref()
            .ok_or_else(|| anyhow!("missing AudioSpecificConfig"))?,
    )?;
    let video_packets = mp4::parse_packet_track(bytes, Some("v0"))
        .ok_or_else(|| anyhow!("missing MP4 video packet index"))?
        .packets;
    let audio_packets = mp4::parse_packet_track(bytes, Some("a0"))
        .ok_or_else(|| anyhow!("missing MP4 audio packet index"))?
        .packets;

    Ok(HlsTrackSet {
        video: HlsTrack {
            codec_string: video_config
                .codec_string
                .unwrap_or_else(|| "avc1.640028".to_string()),
            packets: video_packets,
            payload: PayloadKind::Avc {
                nalu_length_size: video_config.nalu_length_size.unwrap_or(4),
                parameter_sets: parse_avc_decoder_config(&avc)?,
            },
        },
        audio: HlsTrack {
            codec_string: audio_config
                .codec_string
                .unwrap_or_else(|| "mp4a.40.2".to_string()),
            packets: audio_packets,
            payload: PayloadKind::Aac {
                config: parse_audio_specific_config(&asc)?,
            },
        },
    })
}

fn hls_tracks_from_matroska(bytes: &[u8]) -> Result<HlsTrackSet> {
    let meta = matroska::parse_basic_metadata(bytes);
    let video = meta
        .tracks
        .iter()
        .filter(|track| track.kind == MatroskaTrackKind::Video)
        .find(|track| track.codec == "h264")
        .ok_or_else(|| anyhow!("native HLS Matroska path currently requires H.264 video"))?;
    let audio = meta
        .tracks
        .iter()
        .filter(|track| track.kind == MatroskaTrackKind::Audio)
        .find(|track| track.codec == "aac")
        .ok_or_else(|| anyhow!("native HLS Matroska path currently requires AAC audio"))?;
    let avc = video
        .codec_private
        .as_deref()
        .ok_or_else(|| anyhow!("missing Matroska avcC private data"))?;
    let asc = audio
        .codec_private
        .as_deref()
        .ok_or_else(|| anyhow!("missing Matroska AAC private data"))?;
    let video_packets = matroska::parse_packet_track(bytes, Some("v0"))
        .ok_or_else(|| anyhow!("missing Matroska video packet index"))?
        .packets;
    let audio_packets = matroska::parse_packet_track(bytes, Some("a0"))
        .ok_or_else(|| anyhow!("missing Matroska audio packet index"))?
        .packets;
    let avc_config = parse_avc_decoder_config(avc)?;
    let aac_config = parse_audio_specific_config(asc)?;

    Ok(HlsTrackSet {
        video: HlsTrack {
            codec_string: format!("avc1.{:02X}{:02X}{:02X}", avc[1], avc[2], avc[3]),
            packets: video_packets,
            payload: PayloadKind::Avc {
                nalu_length_size: avc_config.nalu_length_size,
                parameter_sets: avc_config,
            },
        },
        audio: HlsTrack {
            codec_string: format!("mp4a.40.{}", aac_config.object_type),
            packets: audio_packets,
            payload: PayloadKind::Aac { config: aac_config },
        },
    })
}

fn segment_windows(video_packets: &[PacketRef], target_ms: u64) -> Vec<SegmentWindow> {
    let mut windows = Vec::new();
    let mut start_ms = video_packets[0].pts.as_millis();
    for packet in video_packets.iter().skip(1) {
        let packet_ms = packet.pts.as_millis();
        if packet.keyframe && packet_ms.saturating_sub(start_ms) >= target_ms {
            windows.push(SegmentWindow {
                index: windows.len(),
                start_ms,
                end_ms: packet_ms,
            });
            start_ms = packet_ms;
        }
    }
    let end_ms = video_packets
        .last()
        .map(packet_end_ms)
        .unwrap_or(start_ms)
        .max(start_ms.saturating_add(1));
    windows.push(SegmentWindow {
        index: windows.len(),
        start_ms,
        end_ms,
    });
    windows
}

fn packet_end_ms(packet: &PacketRef) -> u64 {
    packet
        .pts
        .as_millis()
        .saturating_add(packet.duration.as_millis())
}

fn mux_segment(bytes: &[u8], tracks: &HlsTrackSet, window: SegmentWindow) -> Result<Vec<u8>> {
    let mut mux = TsMuxer::new();
    mux.write_pat_pmt();

    let mut samples = Vec::new();
    for packet in &tracks.video.packets {
        let pts = packet.pts.as_millis();
        if pts < window.start_ms || pts >= window.end_ms {
            continue;
        }
        samples.push((
            packet.dts.as_millis(),
            true,
            packet.pts.scale.to_90khz(packet.pts.units),
            packet.dts.scale.to_90khz(packet.dts.units),
            packet_to_payload(bytes, packet, &tracks.video.payload)?,
        ));
    }
    for packet in &tracks.audio.packets {
        let pts = packet.pts.as_millis();
        if pts < window.start_ms || pts >= window.end_ms {
            continue;
        }
        samples.push((
            packet.dts.as_millis(),
            false,
            packet.pts.scale.to_90khz(packet.pts.units),
            packet.dts.scale.to_90khz(packet.dts.units),
            packet_to_payload(bytes, packet, &tracks.audio.payload)?,
        ));
    }
    samples.sort_by_key(|sample| (sample.0, !sample.1));

    for (_, is_video, pts90, dts90, payload) in samples {
        let timed = TimedPayload {
            pts90,
            dts90,
            bytes: payload,
        };
        if is_video {
            mux.write_pes(VIDEO_PID, VIDEO_STREAM_ID, &timed, true);
        } else {
            mux.write_pes(AUDIO_PID, AUDIO_STREAM_ID, &timed, false);
        }
    }

    Ok(mux.into_bytes())
}

fn packet_to_payload(bytes: &[u8], packet: &PacketRef, kind: &PayloadKind) -> Result<Vec<u8>> {
    let start = packet.source_offset as usize;
    let end = start
        .checked_add(packet.size as usize)
        .ok_or_else(|| anyhow!("packet range overflows"))?;
    if end > bytes.len() {
        bail!("packet range is outside source");
    }
    match kind {
        PayloadKind::Avc {
            nalu_length_size,
            parameter_sets,
        } => {
            let mut out = Vec::new();
            if packet.keyframe {
                for sps in &parameter_sets.sps {
                    out.extend_from_slice(&[0, 0, 0, 1]);
                    out.extend_from_slice(sps);
                }
                for pps in &parameter_sets.pps {
                    out.extend_from_slice(&[0, 0, 0, 1]);
                    out.extend_from_slice(pps);
                }
            }
            out.extend_from_slice(&h264_sample_to_annex_b(
                &bytes[start..end],
                *nalu_length_size,
            )?);
            Ok(out)
        }
        PayloadKind::Aac { config } => {
            let mut out = Vec::with_capacity(packet.size as usize + 7);
            out.extend_from_slice(&adts_header(packet.size as usize, *config)?);
            out.extend_from_slice(&bytes[start..end]);
            Ok(out)
        }
    }
}

fn h264_sample_to_annex_b(sample: &[u8], nalu_length_size: u8) -> Result<Vec<u8>> {
    if looks_like_annex_b(sample) {
        return Ok(sample.to_vec());
    }
    Ok(avc_sample_to_annex_b(sample, nalu_length_size)?)
}

fn looks_like_annex_b(sample: &[u8]) -> bool {
    sample.starts_with(&[0, 0, 1]) || sample.starts_with(&[0, 0, 0, 1])
}

struct TsMuxer {
    out: Vec<u8>,
    continuity: [u8; 8192],
}

impl TsMuxer {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            continuity: [0; 8192],
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.out
    }

    fn write_pat_pmt(&mut self) {
        self.write_psi(0, &pat_section());
        self.write_psi(PMT_PID, &pmt_section());
    }

    fn write_psi(&mut self, pid: u16, section: &[u8]) {
        let mut payload = Vec::with_capacity(section.len() + 1);
        payload.push(0);
        payload.extend_from_slice(section);
        self.write_ts_packets(pid, true, None, &payload);
    }

    fn write_pes(&mut self, pid: u16, stream_id: u8, sample: &TimedPayload, with_pcr: bool) {
        let mut payload = pes_packet(stream_id, sample.pts90, sample.dts90, &sample.bytes);
        let pcr = if with_pcr { Some(sample.dts90) } else { None };
        self.write_ts_packets(pid, true, pcr, &payload);
        payload.clear();
    }

    fn write_ts_packets(
        &mut self,
        pid: u16,
        payload_start: bool,
        pcr: Option<u64>,
        payload: &[u8],
    ) {
        let mut offset = 0_usize;
        let mut first = true;
        while offset < payload.len() || (payload.is_empty() && first) {
            let remaining = payload.len().saturating_sub(offset);
            let include_pcr = first && pcr.is_some();
            let base_payload_capacity = if include_pcr { 176 } else { 184 };
            let payload_len = remaining.min(base_payload_capacity);
            let needs_stuffing = payload_len < base_payload_capacity;
            let adaptation = include_pcr || needs_stuffing;
            let mut packet = [0xff_u8; 188];
            packet[0] = 0x47;
            packet[1] =
                ((if first && payload_start { 0x40 } else { 0 }) | ((pid >> 8) as u8)) & 0x5f;
            packet[2] = pid as u8;
            let cc = self.continuity[pid as usize] & 0x0f;
            self.continuity[pid as usize] = (cc + 1) & 0x0f;
            packet[3] = (if adaptation { 0x30 } else { 0x10 }) | cc;

            let payload_offset = if adaptation {
                let adaptation_total = 184_usize.saturating_sub(payload_len);
                packet[4] = adaptation_total.saturating_sub(1) as u8;
                packet[5] = if include_pcr { 0x10 } else { 0x00 };
                if let Some(pcr_base) = pcr.filter(|_| include_pcr) {
                    write_pcr(&mut packet[6..12], pcr_base);
                }
                4 + adaptation_total
            } else {
                4
            };
            let payload_end = payload_offset + payload_len;
            packet[payload_offset..payload_end]
                .copy_from_slice(&payload[offset..offset + payload_len]);
            self.out.extend_from_slice(&packet);
            offset += payload_len;
            first = false;
        }
    }
}

fn pat_section() -> Vec<u8> {
    let mut section = vec![
        0x00,
        0xb0,
        0x0d,
        0x00,
        0x01,
        0xc1,
        0x00,
        0x00,
        0x00,
        0x01,
        0xe0 | ((PMT_PID >> 8) as u8 & 0x1f),
        PMT_PID as u8,
    ];
    append_crc32(&mut section);
    section
}

fn pmt_section() -> Vec<u8> {
    let mut section = vec![
        0x02,
        0xb0,
        0x17,
        0x00,
        0x01,
        0xc1,
        0x00,
        0x00,
        0xe0 | ((VIDEO_PID >> 8) as u8 & 0x1f),
        VIDEO_PID as u8,
        0xf0,
        0x00,
        0x1b,
        0xe0 | ((VIDEO_PID >> 8) as u8 & 0x1f),
        VIDEO_PID as u8,
        0xf0,
        0x00,
        0x0f,
        0xe0 | ((AUDIO_PID >> 8) as u8 & 0x1f),
        AUDIO_PID as u8,
        0xf0,
        0x00,
    ];
    append_crc32(&mut section);
    section
}

fn pes_packet(stream_id: u8, pts90: u64, dts90: u64, payload: &[u8]) -> Vec<u8> {
    let has_dts = pts90 != dts90;
    let header_len = if has_dts { 10 } else { 5 };
    let pes_len = payload.len().saturating_add(3 + header_len);
    let pes_len = if pes_len > u16::MAX as usize {
        0
    } else {
        pes_len as u16
    };
    let mut out = Vec::with_capacity(payload.len() + 19);
    out.extend_from_slice(&[0, 0, 1, stream_id]);
    out.extend_from_slice(&pes_len.to_be_bytes());
    out.push(0x80);
    out.push(if has_dts { 0xc0 } else { 0x80 });
    out.push(header_len as u8);
    write_pts(&mut out, if has_dts { 0x03 } else { 0x02 }, pts90);
    if has_dts {
        write_pts(&mut out, 0x01, dts90);
    }
    out.extend_from_slice(payload);
    out
}

fn write_pts(out: &mut Vec<u8>, prefix: u8, ts: u64) {
    let ts = ts & 0x1fff_ffff;
    out.push((prefix << 4) | (((ts >> 30) as u8 & 0x07) << 1) | 1);
    out.push((ts >> 22) as u8);
    out.push((((ts >> 15) as u8 & 0x7f) << 1) | 1);
    out.push((ts >> 7) as u8);
    out.push(((ts as u8 & 0x7f) << 1) | 1);
}

fn write_pcr(out: &mut [u8], pcr_base: u64) {
    let base = pcr_base & 0x1fff_ffff;
    out[0] = (base >> 25) as u8;
    out[1] = (base >> 17) as u8;
    out[2] = (base >> 9) as u8;
    out[3] = (base >> 1) as u8;
    out[4] = ((base as u8 & 0x01) << 7) | 0x7e;
    out[5] = 0;
}

fn append_crc32(section: &mut Vec<u8>) {
    let crc = crc32_mpeg(section);
    section.extend_from_slice(&crc.to_be_bytes());
}

fn crc32_mpeg(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

trait To90Khz {
    fn to_90khz(self, units: u64) -> u64;
}

impl To90Khz for crate::packet::TimeScale {
    fn to_90khz(self, units: u64) -> u64 {
        if self.units_per_second == 0 {
            return 0;
        }
        units.saturating_mul(TS_CLOCK) / u64::from(self.units_per_second)
    }
}

fn master_playlist_body(video_codec: &str, audio_codec: &str) -> String {
    format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:BANDWIDTH=12000000,CODECS=\"{video_codec},{audio_codec}\"\n0/playlist.m3u8\n"
    )
}

fn media_playlist_body(target_duration_seconds: u64, durations_ms: &[u64]) -> String {
    let mut out = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{target_duration_seconds}\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n"
    );
    for (index, duration_ms) in durations_ms.iter().enumerate() {
        out.push_str(&format!(
            "#EXTINF:{:.3},\n{}\n",
            *duration_ms as f64 / 1000.0,
            segment_name(index)
        ));
    }
    out.push_str("#EXT-X-ENDLIST\n");
    out
}

fn segment_name(index: usize) -> String {
    format!("seg-{index:05}.ts")
}

fn hex_to_bytes(hex: &str) -> Result<Vec<u8>> {
    let clean = hex.trim();
    if clean.len() % 2 != 0 {
        bail!("hex string has odd length");
    }
    let mut out = Vec::with_capacity(clean.len() / 2);
    let bytes = clean.as_bytes();
    for pair in bytes.chunks_exact(2) {
        let hi = hex_digit(pair[0])?;
        let lo = hex_digit(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_digit(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid hex digit"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_pat_and_pmt_packets() {
        let mut mux = TsMuxer::new();
        mux.write_pat_pmt();
        let out = mux.into_bytes();
        assert_eq!(out.len(), 376);
        assert_eq!(out[0], 0x47);
        assert_eq!(out[188], 0x47);
    }

    #[test]
    fn renders_media_playlist() {
        let body = media_playlist_body(4, &[1500, 4010]);
        assert!(body.contains("#EXT-X-TARGETDURATION:4"));
        assert!(body.contains("#EXTINF:1.500,"));
        assert!(body.contains("seg-00001.ts"));
        assert!(body.ends_with("#EXT-X-ENDLIST\n"));
    }
}
