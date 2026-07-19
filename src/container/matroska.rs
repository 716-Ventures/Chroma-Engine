pub fn looks_like_ebml(head: &[u8]) -> bool {
    head.len() >= 4 && head[0..4] == [0x1a, 0x45, 0xdf, 0xa3]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_ebml() {
        assert!(looks_like_ebml(&[0x1a, 0x45, 0xdf, 0xa3, 0x9f]));
    }
}
