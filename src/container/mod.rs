pub mod matroska;
pub mod mp4;

/// Resource ceilings applied while parsing untrusted container metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseLimits {
    /// Maximum tracks accepted from a single source.
    pub max_tracks: usize,
    /// Maximum top-level or nested boxes/elements visited by one parser.
    pub max_boxes: usize,
    /// Maximum samples materialized for one track index.
    pub max_samples_per_track: usize,
    /// Maximum entries accepted in a single sample table.
    pub max_table_entries: usize,
    /// Maximum bytes retained for metadata/index state.
    pub max_index_bytes: usize,
    /// Maximum metadata nesting depth.
    pub max_depth: usize,
}

impl Default for ParseLimits {
    fn default() -> Self {
        Self {
            max_tracks: 256,
            max_boxes: 100_000,
            max_samples_per_track: 2_000_000,
            max_table_entries: 2_000_000,
            max_index_bytes: 256 * 1024 * 1024,
            max_depth: 32,
        }
    }
}

pub(crate) struct ParseBudget {
    limits: ParseLimits,
    boxes: usize,
    tracks: usize,
    bytes: usize,
}

impl ParseBudget {
    pub(crate) fn new(limits: ParseLimits) -> Self {
        Self {
            limits,
            boxes: 0,
            tracks: 0,
            bytes: 0,
        }
    }

    pub(crate) fn visit(&mut self, depth: usize, track: bool, bytes: usize) -> anyhow::Result<()> {
        self.boxes = self
            .boxes
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("parser work overflow"))?;
        self.tracks += usize::from(track);
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or_else(|| anyhow::anyhow!("parser memory overflow"))?;
        if depth > self.limits.max_depth
            || self.boxes > self.limits.max_boxes
            || self.tracks > self.limits.max_tracks
            || self.bytes > self.limits.max_index_bytes
        {
            anyhow::bail!("container resource limit exceeded");
        }
        Ok(())
    }
}

pub(crate) fn validate_metadata_budget(bytes: &[u8], limits: ParseLimits) -> anyhow::Result<()> {
    match sniff_container(bytes) {
        ContainerKind::Mp4 | ContainerKind::Mov => mp4::validate_metadata_budget(bytes, limits),
        ContainerKind::Matroska | ContainerKind::Webm => {
            matroska::validate_metadata_budget(bytes, limits)
        }
        ContainerKind::Unknown => Ok(()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Container family detected from source bytes.
pub enum ContainerKind {
    /// ISO BMFF MP4 source.
    Mp4,
    /// QuickTime MOV source.
    Mov,
    /// Matroska source.
    Matroska,
    /// WebM source.
    Webm,
    /// Unknown or unsupported source container.
    Unknown,
}

impl ContainerKind {
    /// Returns the stable lowercase name used in Chroma manifests.
    pub fn public_name(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Matroska | Self::Webm => "mkv",
            Self::Unknown => "unknown",
        }
    }

    /// Returns whether the container can be served directly by browser range requests.
    pub fn direct_play(self) -> bool {
        matches!(self, Self::Mp4 | Self::Mov)
    }
}

/// Detects a supported container from the leading bytes of a source file.
pub fn sniff_container(head: &[u8]) -> ContainerKind {
    if mp4::looks_like_mp4(head) {
        return mp4::sniff_mp4_brand(head);
    }
    if matroska::looks_like_ebml(head) {
        return ContainerKind::Matroska;
    }
    ContainerKind::Unknown
}
