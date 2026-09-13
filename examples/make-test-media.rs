//! Generate original synthetic media for CLI smoke/benchmark workflows.
#[path = "../tests/support/modern_fixture.rs"]
mod fixture;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: make-test-media OUTPUT.mkv [SECONDS]")?;
    let seconds: u32 = args
        .next()
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(6);
    // Never overwrite an existing user file.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    std::io::Write::write_all(&mut file, &fixture::modern_mkv(seconds))?;
    Ok(())
}
