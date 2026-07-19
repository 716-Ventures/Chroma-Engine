#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSubtitleCue {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}
