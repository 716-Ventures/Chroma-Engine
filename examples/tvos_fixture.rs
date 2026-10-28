//! Small native H.264/Opus source for the real ChromaTV playback smoke test.
#[path = "../tests/support/modern_fixture.rs"]
mod fixture;

use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let output = args
        .next()
        .ok_or("usage: cargo run --example tvos_fixture -- OUTPUT.mkv [SECONDS]")?;
    let seconds: u32 = args.next().as_deref().unwrap_or("120").parse()?;
    if !(1..=3600).contains(&seconds) || args.next().is_some() {
        return Err("duration must be between 1 and 3600 seconds".into());
    }
    let bytes = fixture::modern_mkv(seconds);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    file.write_all(&bytes)?;
    Ok(())
}
