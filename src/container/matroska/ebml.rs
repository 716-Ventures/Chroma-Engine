#[derive(Debug, Clone, Copy)]
pub(super) struct Element<'a> {
    pub(super) id: u32,
    pub(super) payload: &'a [u8],
    pub(super) payload_offset: usize,
}

pub(super) struct ElementIter<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ElementIter<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
}

impl<'a> Iterator for ElementIter<'a> {
    type Item = Element<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.bytes.len() {
            return None;
        }
        let (id, id_len) = read_vint_id(&self.bytes[self.offset..])?;
        let size_offset = self.offset + id_len;
        let (size, size_len) = read_vint_size(&self.bytes[size_offset..])?;
        let payload_start = size_offset + size_len;
        let payload_end = payload_start.checked_add(size)?;
        if payload_end > self.bytes.len() {
            self.offset = self.bytes.len();
            return None;
        }
        self.offset = payload_end;
        Some(Element {
            id,
            payload: &self.bytes[payload_start..payload_end],
            payload_offset: payload_start,
        })
    }
}

pub(super) fn find_first_child(bytes: &[u8], id: u32) -> Option<&[u8]> {
    ElementIter::new(bytes)
        .find(|element| element.id == id)
        .map(|element| element.payload)
}

pub(super) fn find_first_child_element(bytes: &[u8], id: u32) -> Option<Element<'_>> {
    ElementIter::new(bytes).find(|element| element.id == id)
}

pub(super) fn count_children(bytes: &[u8], id: u32) -> u32 {
    ElementIter::new(bytes)
        .filter(|element| element.id == id)
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

pub(super) fn read_uint(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || bytes.len() > 8 {
        return None;
    }
    let mut value = 0_u64;
    for b in bytes {
        value = (value << 8) | u64::from(*b);
    }
    Some(value)
}

pub(super) fn read_float(bytes: &[u8]) -> Option<f64> {
    match bytes.len() {
        4 => Some(f32::from_be_bytes(bytes.try_into().ok()?) as f64),
        8 => Some(f64::from_be_bytes(bytes.try_into().ok()?)),
        _ => None,
    }
}

pub(super) fn read_string(bytes: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(bytes)
        .trim_matches(char::from(0))
        .trim()
        .to_string();
    if s.is_empty() { None } else { Some(s) }
}

pub(super) fn read_vint_size(bytes: &[u8]) -> Option<(usize, usize)> {
    let first = *bytes.first()?;
    let len = vint_len(first)?;
    if len > 8 || bytes.len() < len {
        return None;
    }
    let mask = 1_u8 << (8 - len);
    let mut value = usize::from(first & !mask);
    for b in &bytes[1..len] {
        value = (value << 8) | usize::from(*b);
    }
    Some((value, len))
}

pub(super) fn read_signed_vint(bytes: &[u8]) -> Option<(isize, usize)> {
    let (value, len) = read_vint_size(bytes)?;
    let bits = 7_usize.checked_mul(len)?;
    let bias = (1_isize.checked_shl((bits - 1) as u32)?).checked_sub(1)?;
    let signed = isize::try_from(value).ok()?.checked_sub(bias)?;
    Some((signed, len))
}

fn read_vint_id(bytes: &[u8]) -> Option<(u32, usize)> {
    let first = *bytes.first()?;
    let len = vint_len(first)?;
    if len > 4 || bytes.len() < len {
        return None;
    }
    let mut value = 0_u32;
    for b in &bytes[..len] {
        value = (value << 8) | u32::from(*b);
    }
    Some((value, len))
}

fn vint_len(first: u8) -> Option<usize> {
    if first == 0 {
        return None;
    }
    Some(first.leading_zeros() as usize + 1)
}
