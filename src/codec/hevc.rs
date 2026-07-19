#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HevcDecoderConfig {
    pub general_profile_idc: u8,
    pub general_level_idc: u8,
    pub arrays: Vec<HevcNalArray>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HevcNalArray {
    pub nal_unit_type: u8,
    pub units: Vec<Vec<u8>>,
}
