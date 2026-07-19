pub mod matroska;
pub mod mp4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerKind {
    Mp4,
    Mov,
    Matroska,
    Webm,
    Unknown,
}

impl ContainerKind {
    pub fn public_name(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Matroska | Self::Webm => "mkv",
            Self::Unknown => "unknown",
        }
    }

    pub fn direct_play(self) -> bool {
        matches!(self, Self::Mp4 | Self::Mov)
    }
}

pub fn sniff_container(head: &[u8]) -> ContainerKind {
    if mp4::looks_like_mp4(head) {
        return mp4::sniff_mp4_brand(head);
    }
    if matroska::looks_like_ebml(head) {
        return ContainerKind::Matroska;
    }
    ContainerKind::Unknown
}
