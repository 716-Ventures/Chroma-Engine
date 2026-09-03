use super::*;

#[derive(Debug, Default)]
pub(super) struct OutputTimestampSanitizer {
    last_video_dts90: Option<u64>,
    last_audio_dts90: Option<u64>,
}

impl OutputTimestampSanitizer {
    pub(super) fn sanitize(&mut self, is_video: bool, pts90: u64, dts90: u64) -> (u64, u64) {
        let slot = if is_video {
            &mut self.last_video_dts90
        } else {
            &mut self.last_audio_dts90
        };
        let out_dts = match *slot {
            Some(last) if dts90 <= last => last.saturating_add(1),
            _ => dts90,
        };
        *slot = Some(out_dts);
        (pts90.max(out_dts), out_dts)
    }
}

pub(super) fn packet_to_payload(
    bytes: &[u8],
    packet: &PacketRef,
    kind: &PayloadKind,
) -> Result<Vec<u8>> {
    let start = packet.source_offset as usize;
    let end = start
        .checked_add(packet.size as usize)
        .ok_or_else(|| anyhow!("packet range overflows"))?;
    if end > bytes.len() {
        return Err(HlsError::message(
            "packet range is outside source".to_string(),
        ));
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
        PayloadKind::Hevc {
            nalu_length_size,
            parameter_sets_annex_b,
        } => {
            let mut out = Vec::new();
            if packet.keyframe {
                out.extend_from_slice(parameter_sets_annex_b);
            }
            out.extend_from_slice(&hevc_sample_to_annex_b(
                &bytes[start..end],
                *nalu_length_size,
            )?);
            Ok(out)
        }
        PayloadKind::Ac3 | PayloadKind::Eac3 | PayloadKind::RawAudio => {
            Ok(bytes[start..end].to_vec())
        }
    }
}

pub(super) fn h264_sample_to_annex_b(sample: &[u8], nalu_length_size: u8) -> Result<Vec<u8>> {
    if looks_like_annex_b(sample) {
        return Ok(sample.to_vec());
    }
    Ok(avc_sample_to_annex_b(sample, nalu_length_size)?)
}

pub(super) fn looks_like_annex_b(sample: &[u8]) -> bool {
    sample.starts_with(&[0, 0, 1]) || sample.starts_with(&[0, 0, 0, 1])
}

pub(super) struct TsMuxer {
    out: Vec<u8>,
    continuity: [u8; 8192],
    video_stream_type: u8,
    audio_stream_type: u8,
}

impl TsMuxer {
    pub(super) fn new(video_stream_type: u8, audio_stream_type: u8) -> Self {
        Self {
            out: Vec::new(),
            continuity: [0; 8192],
            video_stream_type,
            audio_stream_type,
        }
    }

    pub(super) fn into_bytes(self) -> Vec<u8> {
        self.out
    }

    pub(super) fn write_pat_pmt(&mut self) {
        self.write_psi(0, &pat_section());
        self.write_psi(
            PMT_PID,
            &pmt_section(self.video_stream_type, self.audio_stream_type),
        );
    }

    pub(super) fn write_psi(&mut self, pid: u16, section: &[u8]) {
        let mut payload = Vec::with_capacity(section.len() + 1);
        payload.push(0);
        payload.extend_from_slice(section);
        self.write_ts_packets(pid, true, None, &payload);
    }

    pub(super) fn write_pes(
        &mut self,
        pid: u16,
        stream_id: u8,
        sample: &TimedPayload,
        with_pcr: bool,
    ) {
        let mut payload = pes_packet(stream_id, sample.pts90, sample.dts90, &sample.bytes);
        let pcr = if with_pcr { Some(sample.dts90) } else { None };
        self.write_ts_packets(pid, true, pcr, &payload);
        payload.clear();
    }

    pub(super) fn write_ts_packets(
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

pub(super) fn pat_section() -> Vec<u8> {
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

pub(super) fn pmt_section(video_stream_type: u8, audio_stream_type: u8) -> Vec<u8> {
    let audio_descriptors = pmt_audio_descriptors(audio_stream_type);
    let section_length = 5 + 4 + 2 + 5 + 5 + audio_descriptors.len() + 4;
    let mut section = Vec::with_capacity(3 + section_length);
    section.extend_from_slice(&[
        0x02,
        0xb0 | ((section_length >> 8) as u8 & 0x0f),
        section_length as u8,
        0x00,
        0x01,
        0xc1,
        0x00,
        0x00,
        0xe0 | ((VIDEO_PID >> 8) as u8 & 0x1f),
        VIDEO_PID as u8,
        0xf0,
        0x00,
        video_stream_type,
        0xe0 | ((VIDEO_PID >> 8) as u8 & 0x1f),
        VIDEO_PID as u8,
        0xf0,
        0x00,
        audio_stream_type,
        0xe0 | ((AUDIO_PID >> 8) as u8 & 0x1f),
        AUDIO_PID as u8,
        0xf0 | ((audio_descriptors.len() >> 8) as u8 & 0x0f),
        audio_descriptors.len() as u8,
    ]);
    section.extend_from_slice(&audio_descriptors);
    append_crc32(&mut section);
    section
}

pub(super) fn pmt_audio_descriptors(audio_stream_type: u8) -> Vec<u8> {
    match audio_stream_type {
        0x81 => registration_descriptor(*b"AC-3"),
        0x87 => {
            let mut descriptors = registration_descriptor(*b"AC-3");
            descriptors.extend_from_slice(&registration_descriptor(*b"EAC3"));
            descriptors
        }
        _ => Vec::new(),
    }
}

pub(super) fn registration_descriptor(format_identifier: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(6);
    out.push(0x05);
    out.push(0x04);
    out.extend_from_slice(&format_identifier);
    out
}

pub(super) fn ts_stream_type(payload: &PayloadKind) -> u8 {
    match payload {
        PayloadKind::Avc { .. } => 0x1b,
        PayloadKind::Hevc { .. } => 0x24,
        PayloadKind::Aac { .. } => 0x0f,
        PayloadKind::Ac3 => 0x81,
        PayloadKind::Eac3 => 0x87,
        PayloadKind::RawAudio => 0x06,
    }
}

pub(super) fn audio_stream_id(payload: &PayloadKind) -> u8 {
    match payload {
        PayloadKind::Ac3 | PayloadKind::Eac3 | PayloadKind::RawAudio => PRIVATE_STREAM_ID,
        _ => AUDIO_STREAM_ID,
    }
}

pub(super) fn supports_fmp4_audio(payload: &PayloadKind) -> bool {
    matches!(
        payload,
        PayloadKind::Aac { .. } | PayloadKind::Ac3 | PayloadKind::Eac3 | PayloadKind::RawAudio
    )
}

pub(super) fn pes_packet(stream_id: u8, pts90: u64, dts90: u64, payload: &[u8]) -> Vec<u8> {
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

pub(super) fn write_pts(out: &mut Vec<u8>, prefix: u8, ts: u64) {
    let ts = ts & 0x1fff_ffff;
    out.push((prefix << 4) | (((ts >> 30) as u8 & 0x07) << 1) | 1);
    out.push((ts >> 22) as u8);
    out.push((((ts >> 15) as u8 & 0x7f) << 1) | 1);
    out.push((ts >> 7) as u8);
    out.push(((ts as u8 & 0x7f) << 1) | 1);
}

pub(super) fn write_pcr(out: &mut [u8], pcr_base: u64) {
    let base = pcr_base & 0x1fff_ffff;
    out[0] = (base >> 25) as u8;
    out[1] = (base >> 17) as u8;
    out[2] = (base >> 9) as u8;
    out[3] = (base >> 1) as u8;
    out[4] = ((base as u8 & 0x01) << 7) | 0x7e;
    out[5] = 0;
}

pub(super) fn append_crc32(section: &mut Vec<u8>) {
    let crc = crc32_mpeg(section);
    section.extend_from_slice(&crc.to_be_bytes());
}

pub(super) fn crc32_mpeg(bytes: &[u8]) -> u32 {
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

pub(super) trait To90Khz {
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
