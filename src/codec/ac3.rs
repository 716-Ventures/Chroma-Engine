use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac3SpecificBox {
    pub fscod: u8,
    pub bsid: u8,
    pub bsmod: u8,
    pub acmod: u8,
    pub lfeon: bool,
    pub bit_rate_code: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Eac3SpecificBox {
    pub data_rate: u16,
    pub independent_substreams: Vec<Eac3Substream>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Eac3Substream {
    pub fscod: u8,
    pub bsid: u8,
    pub bsmod: u8,
    pub acmod: u8,
    pub lfeon: bool,
    pub num_dep_sub: u8,
    pub chan_loc: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Eac3Header {
    frame_type: u8,
    substream_id: u8,
    frame_size: usize,
    sample_rate: u32,
    num_blocks: u8,
    fscod: u8,
    bsid: u8,
    bsmod: u8,
    acmod: u8,
    lfeon: bool,
    channel_map_present: bool,
    channel_map: u16,
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

impl Eac3SpecificBox {
    pub fn dec3_payload(&self) -> Vec<u8> {
        let substream_count = self.independent_substreams.len().max(1);
        let mut out = vec![0_u8; 2 + substream_count * 4 + substream_count * 2];
        let bytes_written = {
            let mut writer = BitWriter::new(&mut out);
            writer.write_u16(13, self.data_rate.min(0x1fff));
            writer.write(3, substream_count.saturating_sub(1).min(7) as u8);
            for substream in self.independent_substreams.iter().take(8) {
                writer.write(2, substream.fscod);
                writer.write(5, substream.bsid);
                writer.write(1, 0);
                writer.write(1, 0);
                writer.write(3, substream.bsmod);
                writer.write(3, substream.acmod);
                writer.write(1, u8::from(substream.lfeon));
                writer.write(3, 0);
                writer.write(4, substream.num_dep_sub.min(0x0f));
                if substream.num_dep_sub == 0 {
                    writer.write(1, 0);
                } else {
                    writer.write_u16(9, substream.chan_loc & 0x01ff);
                }
            }
            writer.bytes_written()
        };
        out.truncate(bytes_written);
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

pub fn parse_eac3_specific_box(access_unit: &[u8]) -> Result<Eac3SpecificBox> {
    let mut offset = 0_usize;
    let mut independent = Vec::<Eac3Substream>::new();
    let mut total_frame_size = 0_usize;
    let mut total_blocks = 0_u32;
    let mut sample_rate = 0_u32;

    while offset < access_unit.len() {
        let header = parse_eac3_header(&access_unit[offset..])?;
        if offset.saturating_add(header.frame_size) > access_unit.len() {
            bail!("E-AC-3 frame size exceeds access unit");
        }
        total_frame_size = total_frame_size.saturating_add(header.frame_size);
        total_blocks = total_blocks.saturating_add(u32::from(header.num_blocks));
        sample_rate = sample_rate.max(header.sample_rate);

        if header.frame_type == 0 {
            if independent.len() >= 8 {
                bail!("too many E-AC-3 independent substreams");
            }
            if header.substream_id != independent.len() as u8 {
                bail!("unsupported E-AC-3 independent substream order");
            }
            independent.push(Eac3Substream {
                fscod: header.fscod,
                bsid: header.bsid,
                bsmod: header.bsmod,
                acmod: header.acmod,
                lfeon: header.lfeon,
                num_dep_sub: 0,
                chan_loc: 0,
            });
        } else if header.frame_type == 1 {
            let parent = independent
                .get_mut(header.substream_id as usize)
                .ok_or_else(|| anyhow::anyhow!("E-AC-3 dependent substream without parent"))?;
            parent.num_dep_sub = parent.num_dep_sub.saturating_add(1);
            parent.chan_loc |= if header.channel_map_present {
                (header.channel_map >> 5) & 0x01ff
            } else {
                u16::from(header.acmod)
            };
        } else {
            bail!("unsupported E-AC-3 frame type {}", header.frame_type);
        }

        offset += header.frame_size;
    }

    if independent.is_empty() {
        bail!("E-AC-3 access unit has no independent substream");
    }
    let blocks = total_blocks.max(1);
    let data_rate = ((total_frame_size as u64 * 8 * u64::from(sample_rate.max(1)))
        / (u64::from(blocks) * 256)
        / 1000)
        .min(0x1fff) as u16;
    Ok(Eac3SpecificBox {
        data_rate,
        independent_substreams: independent,
    })
}

fn parse_eac3_header(frame: &[u8]) -> Result<Eac3Header> {
    if frame.len() < 10 || frame[0] != 0x0b || frame[1] != 0x77 {
        bail!("missing E-AC-3 sync word");
    }
    let mut bits = BitReader::new(frame);
    bits.skip(16)?;
    let frame_type = bits.read(2)?;
    let substream_id = bits.read(3)?;
    let frame_size = (usize::from(bits.read_u16(11)?) + 1) * 2;
    let fscod = bits.read(2)?;
    let (sample_rate, num_blocks) = if fscod == 3 {
        let fscod2 = bits.read(2)?;
        (sample_rate_from_eac3_fscod2(fscod2)?, 6)
    } else {
        let numblkscod = bits.read(2)?;
        (
            sample_rate_from_fscod(fscod)?,
            match numblkscod {
                0 => 1,
                1 => 2,
                2 => 3,
                _ => 6,
            },
        )
    };
    let acmod = bits.read(3)?;
    let lfeon = bits.read(1)? != 0;
    let bsid = bits.read(5)?;
    bits.skip(5)?;
    let bsmod = bits.read(3)?;

    let mut channel_map_present = false;
    let mut channel_map = 0_u16;
    if frame_type == 1 {
        channel_map_present = bits.read(1)? != 0;
        if channel_map_present {
            channel_map = bits.read_u16(16)?;
        }
    }

    Ok(Eac3Header {
        frame_type,
        substream_id,
        frame_size,
        sample_rate,
        num_blocks,
        fscod,
        bsid,
        bsmod,
        acmod,
        lfeon,
        channel_map_present,
        channel_map,
    })
}

fn sample_rate_from_fscod(fscod: u8) -> Result<u32> {
    match fscod {
        0 => Ok(48_000),
        1 => Ok(44_100),
        2 => Ok(32_000),
        _ => bail!("invalid E-AC-3 fscod"),
    }
}

fn sample_rate_from_eac3_fscod2(fscod2: u8) -> Result<u32> {
    match fscod2 {
        0 => Ok(24_000),
        1 => Ok(22_050),
        2 => Ok(16_000),
        _ => bail!("invalid E-AC-3 fscod2"),
    }
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

    fn read_u16(&mut self, bit_count: usize) -> Result<u16> {
        if bit_count > 16 {
            bail!("cannot read more than 16 bits");
        }
        if self.bit_offset.saturating_add(bit_count) > self.bytes.len() * 8 {
            bail!("AC-3 header ended early");
        }
        let mut value = 0_u16;
        for _ in 0..bit_count {
            let byte = self.bytes[self.bit_offset / 8];
            let bit = (byte >> (7 - (self.bit_offset % 8))) & 1;
            value = (value << 1) | u16::from(bit);
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
        self.write_u16(bit_count, u16::from(value));
    }

    fn write_u16(&mut self, bit_count: usize, value: u16) {
        for bit_index in (0..bit_count).rev() {
            let bit = (value >> bit_index) & 1;
            let byte_index = self.bit_offset / 8;
            let shift = 7 - (self.bit_offset % 8);
            self.bytes[byte_index] |= (bit as u8) << shift;
            self.bit_offset += 1;
        }
    }

    fn bytes_written(&self) -> usize {
        self.bit_offset.div_ceil(8)
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

    #[test]
    fn parses_eac3_header_for_dec3_payload() {
        let frame = eac3_frame(0, 0, 10, false);
        let dec3 = parse_eac3_specific_box(&frame).expect("eac3");
        assert_eq!(dec3.data_rate, 2);
        assert_eq!(
            dec3.independent_substreams,
            vec![Eac3Substream {
                fscod: 0,
                bsid: 16,
                bsmod: 0,
                acmod: 7,
                lfeon: true,
                num_dep_sub: 0,
                chan_loc: 0,
            }]
        );
        assert_eq!(dec3.dec3_payload(), vec![0x00, 0x10, 0x20, 0x0f, 0x00]);
    }

    #[test]
    fn parses_eac3_dependent_substream_for_dec3() {
        let mut access_unit = eac3_frame(0, 0, 10, false);
        access_unit.extend_from_slice(&eac3_frame(1, 0, 12, true));
        let dec3 = parse_eac3_specific_box(&access_unit).expect("eac3");
        assert_eq!(dec3.independent_substreams[0].num_dep_sub, 1);
        assert_ne!(dec3.independent_substreams[0].chan_loc, 0);
    }

    fn eac3_frame(
        frame_type: u8,
        substream_id: u8,
        frame_size: usize,
        channel_map: bool,
    ) -> Vec<u8> {
        let mut frame = vec![0_u8; frame_size];
        frame[0] = 0x0b;
        frame[1] = 0x77;
        let mut writer = BitWriter {
            bytes: &mut frame[2..],
            bit_offset: 0,
        };
        writer.write(2, frame_type);
        writer.write(3, substream_id);
        writer.write_u16(11, (frame_size / 2 - 1) as u16);
        writer.write(2, 0);
        writer.write(2, 3);
        writer.write(3, 7);
        writer.write(1, 1);
        writer.write(5, 16);
        writer.write(5, 0);
        writer.write(3, 0);
        if frame_type == 1 {
            writer.write(1, u8::from(channel_map));
            if channel_map {
                writer.write_u16(16, 0x120);
            }
        }
        frame
    }
}
