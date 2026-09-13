#[cfg(test)]
use std::borrow::Cow;

use anyhow::{Result, bail};

use super::video_encode::RawVideoFormat;

const H264_WEB_MAX_WIDTH: u32 = 1_920;
const H264_WEB_MAX_HEIGHT: u32 = 1_080;

/// Retained coordinate tables; output storage belongs to the pipeline's pool.
#[derive(Debug)]
pub(super) struct BgraScaler {
    source: (u32, u32),
    destination: (u32, u32),
    horizontal: Vec<(usize, usize, f64)>,
    vertical: Vec<(usize, usize, f64)>,
}

impl BgraScaler {
    pub(super) fn new(source: RawVideoFormat, destination: RawVideoFormat) -> Result<Self> {
        if [
            source.width,
            source.height,
            destination.width,
            destination.height,
        ]
        .contains(&0)
        {
            bail!("scaler dimensions must be positive");
        }
        fn coordinates(source: u32, destination: u32) -> Result<Vec<(usize, usize, f64)>> {
            let mut out = Vec::new();
            out.try_reserve_exact(destination as usize)?;
            for position in 0..destination {
                let coordinate = ((f64::from(position) + 0.5) * f64::from(source)
                    / f64::from(destination)
                    - 0.5)
                    .clamp(0.0, f64::from(source - 1));
                let first = coordinate.floor() as usize;
                out.push((
                    first,
                    (first + 1).min(source as usize - 1),
                    coordinate - first as f64,
                ));
            }
            Ok(out)
        }
        Ok(Self {
            source: (source.width, source.height),
            destination: (destination.width, destination.height),
            horizontal: coordinates(source.width, destination.width)?,
            vertical: coordinates(source.height, destination.height)?,
        })
    }

    pub(super) fn scale_into(&self, source: &[u8], out: &mut Vec<u8>) -> Result<()> {
        let stride = (self.source.0 as usize)
            .checked_mul(4)
            .ok_or_else(|| anyhow::anyhow!("scaler stride overflow"))?;
        if stride.checked_mul(self.source.1 as usize) != Some(source.len()) {
            bail!("scaler source size mismatch");
        }
        let destination_stride = (self.destination.0 as usize)
            .checked_mul(4)
            .ok_or_else(|| anyhow::anyhow!("scaler destination stride overflow"))?;
        let length = destination_stride
            .checked_mul(self.destination.1 as usize)
            .ok_or_else(|| anyhow::anyhow!("scaler destination size overflow"))?;
        out.try_reserve_exact(length.saturating_sub(out.len()))?;
        out.resize(length, 0);
        if self.source == self.destination {
            out.copy_from_slice(source);
            return Ok(());
        }
        let half = self.source.0 == self.destination.0.saturating_mul(2)
            && self.source.1 == self.destination.1.saturating_mul(2);
        for (y, &(y0, y1, yw)) in self.vertical.iter().enumerate() {
            for (x, &(x0, x1, xw)) in self.horizontal.iter().enumerate() {
                for channel in 0..4 {
                    let destination = y * destination_stride + x * 4 + channel;
                    if half {
                        let offset = y * 2 * stride + x * 8 + channel;
                        let sum = u16::from(source[offset])
                            + u16::from(source[offset + 4])
                            + u16::from(source[offset + stride])
                            + u16::from(source[offset + stride + 4]);
                        out[destination] = ((sum + 2) / 4) as u8;
                    } else {
                        let tl = f64::from(source[y0 * stride + x0 * 4 + channel]);
                        let tr = f64::from(source[y0 * stride + x1 * 4 + channel]);
                        let bl = f64::from(source[y1 * stride + x0 * 4 + channel]);
                        let br = f64::from(source[y1 * stride + x1 * 4 + channel]);
                        let top = tl + (tr - tl) * xw;
                        let bottom = bl + (br - bl) * xw;
                        out[destination] =
                            (top + (bottom - top) * yw).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
        Ok(())
    }
}

pub(super) fn constrained_h264_format(source: RawVideoFormat) -> RawVideoFormat {
    let width_scale = H264_WEB_MAX_WIDTH as f64 / source.width.max(1) as f64;
    let height_scale = H264_WEB_MAX_HEIGHT as f64 / source.height.max(1) as f64;
    let scale = width_scale.min(height_scale).min(1.0);
    let width = even_dimension((source.width as f64 * scale).round() as u32).max(2);
    let height = even_dimension((source.height as f64 * scale).round() as u32).max(2);
    RawVideoFormat {
        width,
        height,
        frame_rate_num: source.frame_rate_num,
        frame_rate_den: source.frame_rate_den,
        pixel_format: source.pixel_format,
    }
}

fn even_dimension(value: u32) -> u32 {
    if value <= 2 { 2 } else { value & !1 }
}

#[cfg(test)]
#[test]
fn pooled_scaler_matches_reference_and_reuses_capacity() {
    for (sw, sh, dw, dh) in [(16, 12, 8, 6), (17, 13, 10, 8), (8, 6, 14, 10)] {
        let pixels = (0..sw * sh * 4).map(|i| (i * 31) as u8).collect::<Vec<_>>();
        let format = |width, height| RawVideoFormat {
            width,
            height,
            frame_rate_num: 24,
            frame_rate_den: 1,
            pixel_format: super::video_encode::RawVideoPixelFormat::Bgra,
        };
        let scaler = BgraScaler::new(format(sw, sh), format(dw, dh)).unwrap();
        let mut output = Vec::new();
        scaler.scale_into(&pixels, &mut output).unwrap();
        assert_eq!(
            output,
            scale_bgra(&pixels, sw, sh, dw, dh).unwrap().as_ref()
        );
        let pointer = output.as_ptr();
        scaler.scale_into(&pixels, &mut output).unwrap();
        assert_eq!(pointer, output.as_ptr());
    }
}

#[cfg(test)]
pub(super) fn scale_bgra<'a>(
    src: &'a [u8],
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
) -> Result<Cow<'a, [u8]>> {
    let src_stride = usize::try_from(src_width)?
        .checked_mul(4)
        .ok_or_else(|| anyhow::anyhow!("source BGRA stride overflowed"))?;
    let src_len = src_stride
        .checked_mul(usize::try_from(src_height)?)
        .ok_or_else(|| anyhow::anyhow!("source BGRA frame size overflowed"))?;
    if src.len() != src_len {
        bail!("source BGRA frame size does not match declared dimensions");
    }
    if src_width == dst_width && src_height == dst_height {
        return Ok(Cow::Borrowed(src));
    }
    if src_width == dst_width.saturating_mul(2) && src_height == dst_height.saturating_mul(2) {
        return scale_bgra_half(src, src_width, src_height, dst_width, dst_height).map(Cow::Owned);
    }
    let dst_stride = usize::try_from(dst_width)?
        .checked_mul(4)
        .ok_or_else(|| anyhow::anyhow!("destination BGRA stride overflowed"))?;
    let mut out =
        vec![
            0;
            dst_stride
                .checked_mul(usize::try_from(dst_height)?)
                .ok_or_else(|| anyhow::anyhow!("destination BGRA frame size overflowed"))?
        ];
    for y in 0..dst_height {
        let source_y = ((f64::from(y) + 0.5) * f64::from(src_height) / f64::from(dst_height) - 0.5)
            .clamp(0.0, f64::from(src_height.saturating_sub(1)));
        let y0 = source_y.floor() as usize;
        let y1 = (y0 + 1).min(usize::try_from(src_height)? - 1);
        let y_weight = source_y - y0 as f64;
        for x in 0..dst_width {
            let source_x = ((f64::from(x) + 0.5) * f64::from(src_width) / f64::from(dst_width)
                - 0.5)
                .clamp(0.0, f64::from(src_width.saturating_sub(1)));
            let x0 = source_x.floor() as usize;
            let x1 = (x0 + 1).min(usize::try_from(src_width)? - 1);
            let x_weight = source_x - x0 as f64;
            let dst_offset = usize::try_from(y)? * dst_stride + usize::try_from(x)? * 4;
            for channel in 0..4 {
                let top_left = f64::from(src[y0 * src_stride + x0 * 4 + channel]);
                let top_right = f64::from(src[y0 * src_stride + x1 * 4 + channel]);
                let bottom_left = f64::from(src[y1 * src_stride + x0 * 4 + channel]);
                let bottom_right = f64::from(src[y1 * src_stride + x1 * 4 + channel]);
                let top = top_left + (top_right - top_left) * x_weight;
                let bottom = bottom_left + (bottom_right - bottom_left) * x_weight;
                out[dst_offset + channel] =
                    (top + (bottom - top) * y_weight).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Ok(Cow::Owned(out))
}

#[cfg(test)]
fn scale_bgra_half(
    src: &[u8],
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
) -> Result<Vec<u8>> {
    let src_stride = usize::try_from(src_width)?
        .checked_mul(4)
        .ok_or_else(|| anyhow::anyhow!("source BGRA stride overflowed"))?;
    let dst_stride = usize::try_from(dst_width)?
        .checked_mul(4)
        .ok_or_else(|| anyhow::anyhow!("destination BGRA stride overflowed"))?;
    let src_len = src_stride
        .checked_mul(usize::try_from(src_height)?)
        .ok_or_else(|| anyhow::anyhow!("source BGRA frame size overflowed"))?;
    if src.len() != src_len {
        bail!("source BGRA frame size does not match declared dimensions");
    }
    let mut out =
        vec![
            0;
            dst_stride
                .checked_mul(usize::try_from(dst_height)?)
                .ok_or_else(|| anyhow::anyhow!("destination BGRA frame size overflowed"))?
        ];
    for y in 0..usize::try_from(dst_height)? {
        let src_row = (y * 2)
            .checked_mul(src_stride)
            .ok_or_else(|| anyhow::anyhow!("source BGRA row offset overflowed"))?;
        let dst_row = y
            .checked_mul(dst_stride)
            .ok_or_else(|| anyhow::anyhow!("destination BGRA row offset overflowed"))?;
        for x in 0..usize::try_from(dst_width)? {
            let src_offset = src_row + x * 8;
            let dst_offset = dst_row + x * 4;
            for channel in 0..4 {
                let sum = u16::from(src[src_offset + channel])
                    + u16::from(src[src_offset + 4 + channel])
                    + u16::from(src[src_offset + src_stride + channel])
                    + u16::from(src[src_offset + src_stride + 4 + channel]);
                out[dst_offset + channel] = ((sum + 2) / 4) as u8;
            }
        }
    }
    Ok(out)
}
