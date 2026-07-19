use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct HlsSessionRequest {
    pub input: PathBuf,
    pub work_dir: PathBuf,
    pub anchor_seconds: f64,
    pub segment_seconds: f64,
}
