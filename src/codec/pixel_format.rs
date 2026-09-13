/// Returns a Chroma pixel-format label from an AVC/H.264 decoder configuration record.
pub fn pixel_format_from_avc_decoder_config(payload: &[u8]) -> Option<String> {
    if payload.len() < 7 || payload[0] != 1 {
        return None;
    }
    let sps_count = payload[5] & 0x1f;
    let mut offset = 6_usize;
    for _ in 0..sps_count {
        let len = read_u16(payload, offset)? as usize;
        offset = offset.checked_add(2)?.checked_add(len)?;
        if offset > payload.len() {
            return None;
        }
    }
    let pps_count = *payload.get(offset)? as usize;
    offset += 1;
    for _ in 0..pps_count {
        let len = read_u16(payload, offset)? as usize;
        offset = offset.checked_add(2)?.checked_add(len)?;
        if offset > payload.len() {
            return None;
        }
    }

    let profile_idc = payload[1];
    if is_high_profile(profile_idc) && offset + 3 <= payload.len() {
        let chroma_format_idc = payload[offset] & 0x03;
        let bit_depth_luma = 8_u8.saturating_add(payload[offset + 1] & 0x07);
        return pixel_format_label(chroma_format_idc, bit_depth_luma);
    }

    pixel_format_label(1, 8)
}

/// Returns a Chroma pixel-format label from an HEVC decoder configuration record.
pub fn pixel_format_from_hevc_decoder_config(payload: &[u8]) -> Option<String> {
    if payload.len() < 23 || payload[0] != 1 {
        return None;
    }
    // HEVCDecoderConfigurationRecord: bytes 13–14 are spatial segmentation,
    // byte 15 is parallelism; chroma/depth begin at 16, not at 13.
    let chroma_format_idc = payload[16] & 0x03;
    let bit_depth_luma = 8_u8.saturating_add(payload[17] & 0x07);
    pixel_format_label(chroma_format_idc, bit_depth_luma)
}

fn pixel_format_label(chroma_format_idc: u8, bit_depth_luma: u8) -> Option<String> {
    let chroma = match chroma_format_idc {
        0 => "monochrome",
        1 => "yuv420",
        2 => "yuv422",
        3 => "yuv444",
        _ => return None,
    };
    Some(format!("{chroma}-{bit_depth_luma}bit"))
}

fn is_high_profile(profile_idc: u8) -> bool {
    matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    )
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let end = offset.checked_add(2)?;
    let raw = bytes.get(offset..end)?;
    Some(u16::from_be_bytes(raw.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_baseline_avc_pixel_format() {
        let config = [1, 66, 0, 31, 0xff, 0xe1, 0, 1, 0x67, 1, 0, 1, 0x68];
        assert_eq!(
            pixel_format_from_avc_decoder_config(&config).as_deref(),
            Some("yuv420-8bit")
        );
    }

    #[test]
    fn derives_high_bit_depth_avc_pixel_format() {
        let config = [
            1, 110, 0, 31, 0xff, 0xe1, 0, 1, 0x67, 1, 0, 1, 0x68, 0xfd, 0xfa, 0xfa,
        ];
        assert_eq!(
            pixel_format_from_avc_decoder_config(&config).as_deref(),
            Some("yuv420-10bit")
        );
    }

    #[test]
    fn derives_hevc_pixel_format() {
        let mut config = [0_u8; 23];
        config[0] = 1;
        config[13] = 0xf0;
        config[14] = 0;
        config[16] = 0xfd;
        config[17] = 0xfa;
        assert_eq!(
            pixel_format_from_hevc_decoder_config(&config).as_deref(),
            Some("yuv420-10bit")
        );
    }
}
