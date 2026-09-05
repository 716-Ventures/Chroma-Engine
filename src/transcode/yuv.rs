use super::VideoDecodeError;

pub(super) fn limited_yuv_to_bgra(y: u8, u: u8, v: u8) -> [u8; 4] {
    let c = i32::from(y).saturating_sub(16);
    let d = i32::from(u) - 128;
    let e = i32::from(v) - 128;
    let red = ((298 * c + 459 * e + 128) >> 8).clamp(0, 255) as u8;
    let green = ((298 * c - 55 * d - 136 * e + 128) >> 8).clamp(0, 255) as u8;
    let blue = ((298 * c + 541 * d + 128) >> 8).clamp(0, 255) as u8;
    [blue, green, red, 255]
}

/// Copies a mapped 8-bit NV12 hardware surface into tightly packed BGRA.
///
/// The source planes may have independent row padding. Odd display dimensions are
/// accepted, with the chroma plane rounded up to the next complete UV sample.
pub fn convert_nv12_to_bgra(
    y_plane: &[u8],
    y_stride: usize,
    uv_plane: &[u8],
    uv_stride: usize,
    width: usize,
    height: usize,
) -> Result<Vec<u8>, VideoDecodeError> {
    if width == 0 || height == 0 {
        return Err(invalid_output("NV12 dimensions must be non-zero"));
    }

    let chroma_width = width.div_ceil(2);
    let chroma_height = height.div_ceil(2);
    let uv_row_bytes = chroma_width
        .checked_mul(2)
        .ok_or_else(|| invalid_output("NV12 chroma row byte count overflowed"))?;
    validate_plane(y_plane, y_stride, width, height, "Y")?;
    validate_plane(uv_plane, uv_stride, uv_row_bytes, chroma_height, "UV")?;

    let output_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| invalid_output("NV12 BGRA byte count overflowed"))?;
    let mut output = vec![0; output_len];
    for row in 0..height {
        let y_row = row * y_stride;
        let uv_row = (row / 2) * uv_stride;
        for column in 0..width {
            let uv_offset = uv_row + (column / 2) * 2;
            let pixel = limited_yuv_to_bgra(
                y_plane[y_row + column],
                uv_plane[uv_offset],
                uv_plane[uv_offset + 1],
            );
            let output_offset = (row * width + column) * 4;
            output[output_offset..output_offset + 4].copy_from_slice(&pixel);
        }
    }
    Ok(output)
}

/// Copies a mapped 10-bit P010 hardware surface into tightly packed BGRA.
///
/// P010 stores each component in the upper ten bits of a little-endian 16-bit
/// sample. The source planes may have independent row padding.
pub fn convert_p010_to_bgra(
    y_plane: &[u8],
    y_stride: usize,
    uv_plane: &[u8],
    uv_stride: usize,
    width: usize,
    height: usize,
) -> Result<Vec<u8>, VideoDecodeError> {
    if width == 0 || height == 0 {
        return Err(invalid_output("P010 dimensions must be non-zero"));
    }

    let y_row_bytes = width
        .checked_mul(2)
        .ok_or_else(|| invalid_output("P010 luma row byte count overflowed"))?;
    let chroma_width = width.div_ceil(2);
    let chroma_height = height.div_ceil(2);
    let uv_row_bytes = chroma_width
        .checked_mul(4)
        .ok_or_else(|| invalid_output("P010 chroma row byte count overflowed"))?;
    validate_plane(y_plane, y_stride, y_row_bytes, height, "Y")?;
    validate_plane(uv_plane, uv_stride, uv_row_bytes, chroma_height, "UV")?;

    let output_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| invalid_output("P010 BGRA byte count overflowed"))?;
    let mut output = vec![0; output_len];
    for row in 0..height {
        let y_row = row * y_stride;
        let uv_row = (row / 2) * uv_stride;
        for column in 0..width {
            let y_offset = y_row + column * 2;
            let uv_offset = uv_row + (column / 2) * 4;
            let y = p010_sample_to_u8(&y_plane[y_offset..y_offset + 2]);
            let u = p010_sample_to_u8(&uv_plane[uv_offset..uv_offset + 2]);
            let v = p010_sample_to_u8(&uv_plane[uv_offset + 2..uv_offset + 4]);
            let pixel = limited_yuv_to_bgra(y, u, v);
            let output_offset = (row * width + column) * 4;
            output[output_offset..output_offset + 4].copy_from_slice(&pixel);
        }
    }
    Ok(output)
}

fn p010_sample_to_u8(bytes: &[u8]) -> u8 {
    (u16::from_le_bytes([bytes[0], bytes[1]]) >> 8) as u8
}

fn validate_plane(
    plane: &[u8],
    stride: usize,
    row_bytes: usize,
    height: usize,
    name: &str,
) -> Result<(), VideoDecodeError> {
    if stride < row_bytes {
        return Err(invalid_output(format!(
            "NV12 {name} stride {stride} is smaller than its {row_bytes}-byte row"
        )));
    }
    let required = (height - 1)
        .checked_mul(stride)
        .and_then(|offset| offset.checked_add(row_bytes))
        .ok_or_else(|| invalid_output(format!("NV12 {name} plane size overflowed")))?;
    if plane.len() < required {
        return Err(invalid_output(format!(
            "NV12 {name} plane has {} byte(s), expected at least {required}",
            plane.len()
        )));
    }
    Ok(())
}

fn invalid_output(reason: impl Into<String>) -> VideoDecodeError {
    VideoDecodeError::InvalidOutputFormat {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_limited_range_black_and_white() {
        let black = convert_nv12_to_bgra(&[16; 4], 2, &[128, 128], 2, 2, 2).unwrap();
        assert_eq!(black, [0, 0, 0, 255].repeat(4));

        let white = convert_nv12_to_bgra(&[235; 4], 2, &[128, 128], 2, 2, 2).unwrap();
        assert_eq!(white, [255, 255, 255, 255].repeat(4));
    }

    #[test]
    fn converts_p010_black_and_white() {
        let black_y = [0x00, 0x10].repeat(4);
        let white_y = [0x00, 0xeb].repeat(4);
        let neutral_uv = [0x00, 0x80].repeat(2);

        let black = convert_p010_to_bgra(&black_y, 4, &neutral_uv, 4, 2, 2).unwrap();
        assert_eq!(black, [0, 0, 0, 255].repeat(4));

        let white = convert_p010_to_bgra(&white_y, 4, &neutral_uv, 4, 2, 2).unwrap();
        assert_eq!(white, [255, 255, 255, 255].repeat(4));
    }

    #[test]
    fn p010_respects_padding_and_rejects_short_planes() {
        let y = [0x00, 0x10, 0x00, 0xeb, 0, 0, 0x00, 0x10, 0x00, 0xeb];
        let uv = [0x00, 0x80, 0x00, 0x80];
        let output = convert_p010_to_bgra(&y, 6, &uv, 4, 2, 2).unwrap();
        assert_eq!(output[0..4], [0, 0, 0, 255]);
        assert_eq!(output[4..8], [255, 255, 255, 255]);

        let error = convert_p010_to_bgra(&y[..9], 6, &uv, 4, 2, 2).unwrap_err();
        assert!(error.to_string().contains("Y plane"));
    }

    #[test]
    fn respects_plane_strides_and_odd_dimensions() {
        let y = [81, 81, 81, 0, 81, 81, 81, 0, 81, 81, 81];
        let uv = [90, 240, 90, 240, 0, 0, 90, 240, 90, 240];
        let output = convert_nv12_to_bgra(&y, 4, &uv, 6, 3, 3).unwrap();
        assert_eq!(output.len(), 3 * 3 * 4);
        assert!(
            output
                .chunks_exact(4)
                .all(|pixel| pixel == [0, 24, 255, 255])
        );
    }

    #[test]
    fn rejects_short_planes_and_invalid_strides() {
        let short_y = convert_nv12_to_bgra(&[16; 3], 2, &[128, 128], 2, 2, 2).unwrap_err();
        assert!(short_y.to_string().contains("Y plane"));

        let short_uv = convert_nv12_to_bgra(&[16; 4], 2, &[128], 2, 2, 2).unwrap_err();
        assert!(short_uv.to_string().contains("UV plane"));

        let narrow_uv = convert_nv12_to_bgra(&[16; 4], 2, &[128, 128], 1, 2, 2).unwrap_err();
        assert!(narrow_uv.to_string().contains("UV stride"));
    }
}
