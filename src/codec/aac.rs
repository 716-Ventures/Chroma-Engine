#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AacAudioSpecificConfig {
    pub object_type: u8,
    pub sample_rate: u32,
    pub channel_config: u8,
}
