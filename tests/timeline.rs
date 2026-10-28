#[path = "support/modern_fixture.rs"]
mod fixture;

use chroma_engine::{NativeFmp4TranscodeOptions, NativeFmp4TranscodeSession};

fn find_box<'a>(bytes: &'a [u8], target: &[u8; 4]) -> Option<&'a [u8]> {
    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().ok()?) as usize;
        if size < 8 || size > bytes.len() - offset {
            return None;
        }
        let kind = &bytes[offset + 4..offset + 8];
        let payload = &bytes[offset + 8..offset + size];
        if kind == target {
            return Some(payload);
        }
        if [b"moov", b"trak", b"mdia", b"moof", b"traf"].contains(&kind.try_into().ok()?)
            && let Some(found) = find_box(payload, target)
        {
            return Some(found);
        }
        offset += size;
    }
    None
}

#[test]
fn late_seek_fragment_keeps_the_source_decode_timeline() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("two-minutes.mkv");
    std::fs::write(&source, fixture::modern_mkv(120)).unwrap();
    let init = directory.path().join("init.mp4");
    let segment = directory.path().join("late.m4s");
    let session =
        NativeFmp4TranscodeSession::open(&source, NativeFmp4TranscodeOptions::default()).unwrap();
    session.write_start(&init, &segment, 27).unwrap();
    let init_bytes = std::fs::read(init).unwrap();
    let header = find_box(&init_bytes, b"mdhd").unwrap();
    assert_eq!(header[0], 0);
    let video_timescale = u32::from_be_bytes(header[12..16].try_into().unwrap());
    let media_bytes = std::fs::read(segment).unwrap();
    let decode = find_box(&media_bytes, b"tfdt").unwrap();
    assert_eq!(decode[0], 1);
    let base_decode_time = u64::from_be_bytes(decode[4..12].try_into().unwrap());
    assert_eq!(
        base_decode_time,
        108 * u64::from(video_timescale),
        "a seek to 108 seconds must not wrap the video decode clock to ~18 seconds"
    );
}
