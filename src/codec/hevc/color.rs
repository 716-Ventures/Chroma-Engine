//! Bounded H.265 SPS/VUI color description (H.265 7.3.2.2.1 / E.2.1).
//! Container color fields are optional; bit depth never establishes HDR.
use super::parse_hevc_decoder_config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HevcColorDescription {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub full_range: bool,
}

/// Return explicit, consistent SPS color metadata from a bounded hvcC record.
/// Missing/truncated/contradictory descriptions remain unknown.
pub fn color_description(config: &[u8]) -> Option<HevcColorDescription> {
    if config.len() > 65_536 {
        return None;
    }
    let parsed = parse_hevc_decoder_config(config).ok()?;
    let mut found = None;
    for nal in parsed
        .arrays
        .iter()
        .filter(|a| a.nal_unit_type == 33)
        .flat_map(|a| &a.units)
    {
        let color = sps_color(nal)?;
        if found.is_some_and(|previous| previous != color) {
            return None;
        }
        found = Some(color);
    }
    found
}

struct Bits {
    bytes: Vec<u8>,
    offset: usize,
}
impl Bits {
    fn read(&mut self, n: usize) -> Option<u32> {
        if n > 32 || self.offset.checked_add(n)? > self.bytes.len() * 8 {
            return None;
        }
        let mut value = 0;
        for _ in 0..n {
            value = (value << 1)
                | u32::from((self.bytes[self.offset / 8] >> (7 - self.offset % 8)) & 1);
            self.offset += 1;
        }
        Some(value)
    }
    fn skip(&mut self, n: usize) -> Option<()> {
        self.offset = self.offset.checked_add(n)?;
        (self.offset <= self.bytes.len() * 8).then_some(())
    }
    fn flag(&mut self) -> Option<bool> {
        Some(self.read(1)? != 0)
    }
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while !self.flag()? {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        ((1_u32 << zeros) - 1).checked_add(self.read(zeros)?)
    }
    fn bounded_ue(&mut self, max: u32) -> Option<u32> {
        let v = self.ue()?;
        (v <= max).then_some(v)
    }
}

fn sps_color(nal: &[u8]) -> Option<HevcColorDescription> {
    if nal.len() < 3 || nal[0] & 0x80 != 0 || (nal[0] >> 1) & 63 != 33 || nal[1] & 7 == 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(nal.len() - 2);
    let mut zeros = 0;
    let mut i = 2;
    while i < nal.len() {
        let byte = nal[i];
        if zeros >= 2 && byte == 3 {
            if *nal.get(i + 1)? > 3 {
                return None;
            }
            zeros = 0;
            i += 1;
            continue;
        }
        bytes.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        i += 1;
    }
    let mut b = Bits { bytes, offset: 0 };
    b.skip(4)?;
    let layers = b.read(3)? as usize;
    if layers > 6 {
        return None;
    }
    b.skip(1 + 96)?; // nesting + general profile/tier/level
    let mut sub = [(false, false); 7];
    for flags in &mut sub[..layers] {
        *flags = (b.flag()?, b.flag()?);
    }
    if layers > 0 {
        b.skip((8 - layers) * 2)?;
    }
    for (profile, level) in &sub[..layers] {
        if *profile {
            b.skip(88)?;
        }
        if *level {
            b.skip(8)?;
        }
    }
    b.bounded_ue(15)?; // SPS id
    let chroma = b.bounded_ue(3)?;
    if chroma == 3 {
        b.skip(1)?;
    }
    b.ue()?;
    b.ue()?; // coded dimensions
    if b.flag()? {
        for _ in 0..4 {
            b.ue()?;
        }
    }
    b.bounded_ue(8)?;
    b.bounded_ue(8)?; // bit depths
    let poc = b.bounded_ue(12)? as usize + 4;
    let start = if b.flag()? { 0 } else { layers };
    for _ in start..=layers {
        for _ in 0..3 {
            b.ue()?;
        }
    }
    for _ in 0..6 {
        b.ue()?;
    } // block sizes and transform hierarchy
    if b.flag()? && b.flag()? {
        // scaling_list_data
        for size in 0..4 {
            let step = if size == 3 { 3 } else { 1 };
            for _ in (0..6).step_by(step) {
                if !b.flag()? {
                    b.ue()?;
                } else {
                    if size > 1 {
                        b.ue()?;
                    }
                    for _ in 0..64.min(1 << (4 + 2 * size)) {
                        b.ue()?;
                    }
                }
            }
        }
    }
    b.skip(2)?; // AMP, SAO
    if b.flag()? {
        b.skip(8)?;
        b.ue()?;
        b.ue()?;
        b.skip(1)?;
    }
    let count = b.bounded_ue(64)? as usize;
    let mut refs = [0_usize; 64];
    for i in 0..count {
        if i > 0 && b.flag()? {
            b.skip(1)?;
            b.ue()?;
            let mut used = 0;
            for _ in 0..=refs[i - 1] {
                let current = b.flag()?;
                if current || b.flag()? {
                    used += 1;
                }
            }
            if used > 64 {
                return None;
            }
            refs[i] = used;
        } else {
            let negative = b.bounded_ue(64)?;
            let positive = b.bounded_ue(64)?;
            let total = negative.checked_add(positive)?;
            if total > 64 {
                return None;
            }
            for _ in 0..total {
                b.ue()?;
                b.skip(1)?;
            }
            refs[i] = total as usize;
        }
    }
    if b.flag()? {
        let count = b.bounded_ue(32)?;
        for _ in 0..count {
            b.skip(poc + 1)?;
        }
    }
    b.skip(2)?; // temporal MVP, strong intra smoothing
    if !b.flag()? {
        return None;
    } // VUI present
    if b.flag()? && b.read(8)? == 255 {
        b.skip(32)?;
    }
    if b.flag()? {
        b.skip(1)?;
    }
    if !b.flag()? {
        return None;
    } // video signal type present
    b.skip(3)?;
    let full_range = b.flag()?;
    if !b.flag()? {
        return None;
    } // explicit color description
    Some(HevcColorDescription {
        primaries: b.read(8)? as u8,
        transfer: b.read(8)? as u8,
        matrix: b.read(8)? as u8,
        full_range,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const HDR: &str = "01222000000090000000000096f000fcfdfafa00000704a00001001940010c01ffff22200000030090000003000003009695890240a100010033420101222000000300900000030000030096a001e020021c4d965626ed208ffc00040005a84880482000000300200000030301a2000100064401c171c312a70002000a4e019004000003000080001e4e01891821342134199619968a488a483d133d1300989680000003000180";
    fn decode_hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
    fn synthetic_sps(color: Option<(u8, u8, u8)>) -> Vec<u8> {
        let mut bits = String::new();
        bits.push_str("00000001"); // VPS id, layers, nesting
        bits.push_str(&"0".repeat(96)); // profile/tier/level
        bits.push_str("101011"); // SPS id 0, chroma 1, dimensions 0/0
        bits.push('0'); // no conformance window
        bits.push_str("111"); // depths and POC delta = 0
        bits.push('0'); // ordering only highest layer
        bits.push_str("111"); // ordering
        bits.push_str("111111"); // block / transform hierarchy
        bits.push_str("0000"); // scaling, AMP, SAO, PCM
        bits.push('1'); // no short-term sets
        bits.push_str("000"); // no long term, temporal MVP, smoothing
        bits.push_str("1001"); // VUI present, no aspect/overscan, video signal
        bits.push_str("1010"); // video format, limited range
        bits.push(if color.is_some() { '1' } else { '0' });
        if let Some((p, t, m)) = color {
            for v in [p, t, m] {
                bits.push_str(&format!("{v:08b}"));
            }
        }
        bits.push('1');
        while !bits.len().is_multiple_of(8) {
            bits.push('0');
        }
        let raw: Vec<u8> = bits
            .as_bytes()
            .chunks(8)
            .map(|c| c.iter().fold(0, |a, b| (a << 1) | (b - b'0')))
            .collect();
        let mut nal = vec![0x42, 1];
        let mut zeros = 0;
        for byte in raw {
            if zeros >= 2 && byte <= 3 {
                nal.push(3);
                zeros = 0;
            }
            nal.push(byte);
            zeros = if byte == 0 { zeros + 1 } else { 0 };
        }
        nal
    }
    #[test]
    fn reads_explicit_sdr_pq_and_hlg_without_guessing_from_depth() {
        for transfer in [1, 16, 18, 2] {
            let nal = synthetic_sps(Some((9, transfer, 9)));
            assert_eq!(sps_color(&nal).unwrap().transfer, transfer);
            for end in 0..nal.len() - 1 {
                assert_eq!(sps_color(&nal[..end]), None);
            }
        }
        assert_eq!(sps_color(&synthetic_sps(None)), None);
    }
    #[test]
    fn rejects_conflicting_sequence_headers() {
        let mut config = decode_hex(HDR);
        config.truncate(23);
        config[22] = 1;
        config.extend_from_slice(&[33, 0, 2]);
        for transfer in [16, 18] {
            let nal = synthetic_sps(Some((9, transfer, 9)));
            config.extend_from_slice(&(nal.len() as u16).to_be_bytes());
            config.extend_from_slice(&nal);
        }
        assert_eq!(color_description(&config), None);
    }
    #[test]
    fn hdr_vui_is_read_without_container_color_or_video_decoding() {
        let config = decode_hex(HDR);
        assert_eq!(
            color_description(&config),
            Some(HevcColorDescription {
                primaries: 9,
                transfer: 16,
                matrix: 9,
                full_range: false
            })
        );
        for end in 0..23 {
            assert_eq!(color_description(&config[..end]), None);
        }
        assert_eq!(color_description(&vec![0; 65_537]), None);
    }
}
