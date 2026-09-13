//! Original, redistributable synthetic H.264/Opus media, generated without FFmpeg.
use chroma_engine::{
    CpuH264EncoderSession, RawVideoFormat, RawVideoFrameRef, RawVideoPixelFormat, TimeDelta,
    TimePoint,
};

fn element(id: u32, payload: &[u8]) -> Vec<u8> {
    let bytes = id.to_be_bytes();
    let first = bytes.iter().position(|byte| *byte != 0).unwrap();
    let mut output = bytes[first..].to_vec();
    output.extend_from_slice(&((1u64 << 56) | payload.len() as u64).to_be_bytes());
    output.extend_from_slice(payload);
    output
}
fn uint(id: u32, value: u64) -> Vec<u8> {
    element(id, &value.to_be_bytes())
}
fn block(track: u8, time: i16, data: &[u8]) -> Vec<u8> {
    let mut payload = vec![0x80 | track];
    payload.extend_from_slice(&time.to_be_bytes());
    payload.push(0x80);
    payload.extend_from_slice(data);
    element(0xa3, &payload)
}

pub fn modern_mkv(seconds: u32) -> Vec<u8> {
    assert!((1..=3600).contains(&seconds));
    let format = RawVideoFormat {
        width: 32,
        height: 32,
        frame_rate_num: 2,
        frame_rate_den: 1,
        pixel_format: RawVideoPixelFormat::Bgra,
    };
    let mut encoder = CpuH264EncoderSession::new(format, 100_000).unwrap();
    let pixels = [32, 160, 64, 255].repeat(32 * 32);
    let encoded = encoder
        .encode(&[RawVideoFrameRef {
            pts: TimePoint::millis(0),
            dts: TimePoint::millis(0),
            duration: TimeDelta::millis(500),
            bytes: &pixels,
            keyframe: true,
        }])
        .unwrap();
    assert!(encoded.frames[0].keyframe);
    let config = encoded.stream.decoder_config.unwrap();
    let video = element(
        0xae,
        &[
            uint(0xd7, 1),
            uint(0x83, 1),
            element(0x86, b"V_MPEG4/ISO/AVC"),
            element(0x63a2, &config),
            uint(0x23e383, 500_000_000),
            element(0xe0, &[uint(0xb0, 32), uint(0xba, 32)].concat()),
        ]
        .concat(),
    );
    let opus_head = [
        b"OpusHead".as_slice(),
        &[1, 2, 0, 0, 0x80, 0xbb, 0, 0, 0, 0, 0],
    ]
    .concat();
    let audio = element(
        0xae,
        &[
            uint(0xd7, 2),
            uint(0x83, 2),
            element(0x86, b"A_OPUS"),
            element(0x63a2, &opus_head),
            uint(0x23e383, 20_000_000),
            element(
                0xe1,
                &[uint(0x9f, 2), element(0xb5, &48_000f64.to_be_bytes())].concat(),
            ),
        ]
        .concat(),
    );
    let mut segment = [
        element(
            0x1549a966,
            &[
                uint(0x2ad7b1, 1_000_000),
                element(0x4489, &(f64::from(seconds) * 1000.0).to_be_bytes()),
            ]
            .concat(),
        ),
        element(0x1654ae6b, &[video, audio].concat()),
    ]
    .concat();
    for second in 0..seconds {
        let mut cluster = uint(0xe7, u64::from(second) * 1000);
        for time in (0..1000).step_by(20) {
            if time % 500 == 0 {
                cluster.extend(block(1, time, &encoded.frames[0].payload));
            }
            cluster.extend(block(2, time, &[0xf8, 0xff, 0xfe]));
        }
        segment.extend(element(0x1f43b675, &cluster));
    }
    [
        element(0x1a45dfa3, &element(0x4282, b"matroska")),
        element(0x18538067, &segment),
    ]
    .concat()
}
