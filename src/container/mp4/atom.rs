#[derive(Debug, Clone, Copy)]
pub(super) struct Atom<'a> {
    pub(super) kind: [u8; 4],
    pub(super) payload: &'a [u8],
}

pub(super) struct AtomIter<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> AtomIter<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
}

impl<'a> Iterator for AtomIter<'a> {
    type Item = Atom<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self
            .offset
            .checked_add(8)
            .is_none_or(|end| end > self.bytes.len())
        {
            return None;
        }

        let start = self.offset;
        let size32 = usize::try_from(read_u32(&self.bytes[start..start + 4])?).ok()?;
        let kind_start = start.checked_add(4)?;
        let kind_end = start.checked_add(8)?;
        let kind: [u8; 4] = self.bytes[kind_start..kind_end].try_into().ok()?;
        let (header, size) = if size32 == 1 {
            if start
                .checked_add(16)
                .is_none_or(|end| end > self.bytes.len())
            {
                return None;
            }
            let size_start = start.checked_add(8)?;
            let size_end = start.checked_add(16)?;
            let size64 = usize::try_from(read_u64(&self.bytes[size_start..size_end])?).ok()?;
            (16, size64)
        } else if size32 == 0 {
            (8, self.bytes.len() - start)
        } else {
            (8, size32)
        };

        let payload_start = start.checked_add(header)?;
        let end = start.checked_add(size)?;
        if size < header || end > self.bytes.len() {
            self.offset = self.bytes.len();
            return None;
        }

        self.offset = end;
        Some(Atom {
            kind,
            payload: &self.bytes[payload_start..end],
        })
    }
}

pub(super) fn read_u32(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.try_into().ok()?))
}

pub(super) fn read_i32(bytes: &[u8]) -> Option<i32> {
    Some(i32::from_be_bytes(bytes.try_into().ok()?))
}

pub(super) fn read_u16(bytes: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.try_into().ok()?))
}

pub(super) fn read_u64(bytes: &[u8]) -> Option<u64> {
    Some(u64::from_be_bytes(bytes.try_into().ok()?))
}
