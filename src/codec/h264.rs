#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvcDecoderConfig {
    pub profile_idc: u8,
    pub level_idc: u8,
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
}
