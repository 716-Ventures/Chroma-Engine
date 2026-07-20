use anyhow::{bail, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac3SpecificBox {
    pub fscod: u8,
    pub bsid: u8,
    pub bsmod: u8,
    pub acmod: u8,
    pub lfeon: bool,
    pub bit_rate_code: u8,
}

impl Ac3SpecificBox {
    pub fn dac3_payload(self) -> [u8; 3] {
        let mut out = [0_u8; 3];
        let mut writer = BitWriter::new(&mut out);
        writer.write(2, self.fscod);
        writer.write(5, self.bsid);
        writer.write(3, self.bsmod);
        writer.write(3, self.acmod);
        writer.write(1, u8::from(self.lfeon));
        writer.write(5, self.bit_rate_code);
        writer.write(5, 0);
        out
    }
}

pub fn parse_ac3_specific_box(frame: &[u8]) -> Result<Ac3SpecificBox> {
    if frame.len() < 7 || frame[0] != 0x0b || frame[1] != 0x77 {
        bail!("missing AC-3 sync word");
    }
    let mut bits = BitReader::new(frame);
    bits.skip(32)?;
    let fscod = bits.read(2)?;
    if fscod == 3 {
        bail!("AC-3 half-sample-rate fscod is not supported in dac3");
    }
    let frmsizecod = bits.read(6)?;
    let bsid = bits.read(5)?;
    if bsid > 8 {
        bail!("AC-3 bsid {bsid} is outside ISO BMFF dac3 range");
    }
    let bsmod = bits.read(3)?;
    let acmod = bits.read(3)?;
    if (acmod & 0x1) != 0 && acmod != 0x1 {
        bits.skip(2)?;
    }
    if (acmod & 0x4) != 0 {
        bits.skip(2)?;
    }
    if acmod == 0x2 {
        bits.skip(2)?;
    }
    let lfeon = bits.read(1)? != 0;

    Ok(Ac3SpecificBox {
        fscod,
        bsid,
        bsmod,
        acmod,
        lfeon,
        bit_rate_code: frmsizecod >> 1,
    })
}

struct BitReader<'a> {
    bytes: &'a [u8],
    bit_offset: usize,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            bit_offset: 0,
        }
    }

    fn read(&mut self, bit_count: usize) -> Result<u8> {
        if bit_count > 8 {
            bail!("cannot read more than 8 bits");
        }
        if self.bit_offset.saturating_add(bit_count) > self.bytes.len() * 8 {
            bail!("AC-3 header ended early");
        }
        let mut value = 0_u8;
        for _ in 0..bit_count {
            let byte = self.bytes[self.bit_offset / 8];
            let bit = (byte >> (7 - (self.bit_offset % 8))) & 1;
            value = (value << 1) | bit;
            self.bit_offset += 1;
        }
        Ok(value)
    }

    fn skip(&mut self, bit_count: usize) -> Result<()> {
        if self.bit_offset.saturating_add(bit_count) > self.bytes.len() * 8 {
            bail!("AC-3 header ended early");
        }
        self.bit_offset += bit_count;
        Ok(())
    }
}

struct BitWriter<'a> {
    bytes: &'a mut [u8],
    bit_offset: usize,
}

impl<'a> BitWriter<'a> {
    fn new(bytes: &'a mut [u8]) -> Self {
        Self {
            bytes,
            bit_offset: 0,
        }
    }

    fn write(&mut self, bit_count: usize, value: u8) {
        for bit_index in (0..bit_count).rev() {
            let bit = (value >> bit_index) & 1;
            let byte_index = self.bit_offset / 8;
            let shift = 7 - (self.bit_offset % 8);
            self.bytes[byte_index] |= bit << shift;
            self.bit_offset += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ac3_header_for_dac3_payload() {
        let frame = [0x0b, 0x77, 0, 0, 0x50, 0x41, 0x40];
        let dac3 = parse_ac3_specific_box(&frame).expect("ac3");
        assert_eq!(
            dac3,
            Ac3SpecificBox {
                fscod: 1,
                bsid: 8,
                bsmod: 1,
                acmod: 2,
                lfeon: false,
                bit_rate_code: 8,
            }
        );
        assert_eq!(dac3.dac3_payload(), [0x50, 0x51, 0x00]);
    }

    #[test]
    fn rejects_non_ac3_sync() {
        let err = parse_ac3_specific_box(&[0, 0, 0, 0]).expect_err("reject");
        assert!(err.to_string().contains("sync"));
    }
}
