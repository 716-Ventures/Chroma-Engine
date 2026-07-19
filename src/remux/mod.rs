use std::path::Path;

use anyhow::{bail, Result};

use crate::container::{sniff_container, ContainerKind};

pub fn remux_mp4(input: &Path, _output: &Path) -> Result<()> {
    let mut file = std::fs::File::open(input)?;
    let mut head = [0_u8; 4096];
    let n = std::io::Read::read(&mut file, &mut head)?;
    let kind = sniff_container(&head[..n]);
    match kind {
        ContainerKind::Matroska | ContainerKind::Webm | ContainerKind::Mp4 | ContainerKind::Mov => {
            bail!("remux_mp4 packet writer not implemented yet")
        }
        ContainerKind::Unknown => bail!("unsupported container"),
    }
}
