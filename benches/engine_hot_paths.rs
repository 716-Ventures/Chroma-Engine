use std::hint::black_box;

use chroma_engine::{
    AudioSelection, PacketRef, PlaybackConstraints, PlaybackTarget, TimeDelta, TimePoint,
    TimeScale, parse_subrip, plan_fixed_chunks, plan_playback, probe_media_source, render_webvtt,
    segment_webvtt,
};
use criterion::{Criterion, criterion_group, criterion_main};
use tempfile::NamedTempFile;

fn bench_probe_and_plan(c: &mut Criterion) {
    let sample = NamedTempFile::new().expect("temp file");
    std::fs::write(sample.path(), minimal_mp4()).expect("write sample mp4");

    c.bench_function("probe/minimal_mp4", |b| {
        b.iter(|| probe_media_source(black_box(sample.path())).expect("probe mp4"));
    });

    let probe = probe_media_source(sample.path()).expect("probe mp4");
    c.bench_function("plan/browser_primary_audio", |b| {
        b.iter(|| {
            plan_playback(
                black_box(&probe),
                PlaybackConstraints {
                    target: PlaybackTarget::Browser,
                    audio_selection: AudioSelection::Primary,
                    include_subtitles: true,
                    max_video_pixels: None,
                    prefer_copy: true,
                },
            )
        });
    });
}

fn bench_packet_planning(c: &mut Criterion) {
    let packets = synthetic_packets(12_000);
    c.bench_function("packet_plan/12000_packets", |b| {
        b.iter(|| plan_fixed_chunks(black_box(&packets), black_box(4_000)));
    });
}

fn bench_subtitles(c: &mut Criterion) {
    let subrip = synthetic_subrip(2_000);
    c.bench_function("subtitles/parse_subrip_2000", |b| {
        b.iter(|| parse_subrip(black_box(&subrip)));
    });

    let cues = parse_subrip(&subrip);
    c.bench_function("subtitles/render_webvtt_2000", |b| {
        b.iter(|| render_webvtt(black_box(&cues)));
    });
    c.bench_function("subtitles/segment_webvtt_2000", |b| {
        b.iter(|| segment_webvtt(black_box(&cues), black_box(4_000), black_box("s0")));
    });
}

fn synthetic_packets(count: usize) -> Vec<PacketRef> {
    let scale = TimeScale {
        units_per_second: 1_000,
    };
    (0..count)
        .map(|index| {
            let units = u64::try_from(index).expect("index") * 40;
            PacketRef {
                source_offset: u64::try_from(index).expect("index") * 1_024,
                size: 1_024,
                pts: TimePoint { units, scale },
                dts: TimePoint { units, scale },
                duration: TimeDelta { units: 40, scale },
                keyframe: index.is_multiple_of(50),
            }
        })
        .collect()
}

fn synthetic_subrip(count: usize) -> String {
    let mut out = String::new();
    for index in 0..count {
        let start = u64::try_from(index).expect("index") * 1_500;
        let end = start + 1_000;
        out.push_str(&format!(
            "{}\n{} --> {}\nCaption line {}\n\n",
            index + 1,
            timestamp(start),
            timestamp(end),
            index + 1
        ));
    }
    out
}

fn timestamp(ms: u64) -> String {
    let total_seconds = ms / 1_000;
    let millis = ms % 1_000;
    let seconds = total_seconds % 60;
    let total_minutes = total_seconds / 60;
    let minutes = total_minutes % 60;
    let hours = total_minutes / 60;
    format!("{hours:02}:{minutes:02}:{seconds:02},{millis:03}")
}

fn minimal_mp4() -> Vec<u8> {
    let mut out = atom(
        *b"ftyp",
        &[
            b"isom".as_slice(),
            &0_u32.to_be_bytes(),
            b"isom".as_slice(),
            b"mp42".as_slice(),
        ]
        .concat(),
    );
    out.extend_from_slice(&atom(
        *b"moov",
        &[
            mvhd(1_000, 12_345),
            trak(*b"vide", *b"avc1", Some((1920, 1080)), None, 12_345, 1_000),
            trak(*b"soun", *b"mp4a", None, Some((2, 48_000)), 592_560, 48_000),
        ]
        .concat(),
    ));
    out
}

fn mvhd(timescale: u32, duration: u32) -> Vec<u8> {
    let mut payload = vec![0_u8; 20];
    payload[12..16].copy_from_slice(&timescale.to_be_bytes());
    payload[16..20].copy_from_slice(&duration.to_be_bytes());
    atom(*b"mvhd", &payload)
}

fn trak(
    handler: [u8; 4],
    sample_entry: [u8; 4],
    size: Option<(u32, u32)>,
    audio: Option<(u16, u32)>,
    duration: u32,
    timescale: u32,
) -> Vec<u8> {
    atom(
        *b"trak",
        &[
            tkhd(size),
            atom(
                *b"mdia",
                &[
                    mdhd(timescale, duration),
                    hdlr(handler),
                    atom(*b"minf", &atom(*b"stbl", &stsd(sample_entry, size, audio))),
                ]
                .concat(),
            ),
        ]
        .concat(),
    )
}

fn tkhd(size: Option<(u32, u32)>) -> Vec<u8> {
    let mut payload = vec![0_u8; 84];
    if let Some((w, h)) = size {
        payload[76..80].copy_from_slice(&(w << 16).to_be_bytes());
        payload[80..84].copy_from_slice(&(h << 16).to_be_bytes());
    }
    atom(*b"tkhd", &payload)
}

fn mdhd(timescale: u32, duration: u32) -> Vec<u8> {
    let mut payload = vec![0_u8; 20];
    payload[12..16].copy_from_slice(&timescale.to_be_bytes());
    payload[16..20].copy_from_slice(&duration.to_be_bytes());
    atom(*b"mdhd", &payload)
}

fn hdlr(handler: [u8; 4]) -> Vec<u8> {
    let mut payload = vec![0_u8; 12];
    payload[8..12].copy_from_slice(&handler);
    atom(*b"hdlr", &payload)
}

fn stsd(sample_entry: [u8; 4], size: Option<(u32, u32)>, audio: Option<(u16, u32)>) -> Vec<u8> {
    let entry = sample_entry_payload(sample_entry, size, audio);
    let mut payload = Vec::new();
    payload.extend_from_slice(&[0, 0, 0, 0]);
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.extend_from_slice(&entry);
    atom(*b"stsd", &payload)
}

fn sample_entry_payload(
    sample_entry: [u8; 4],
    size: Option<(u32, u32)>,
    audio: Option<(u16, u32)>,
) -> Vec<u8> {
    let mut payload = vec![0_u8; 28];
    if let Some((w, h)) = size {
        payload[24..26].copy_from_slice(&u16::try_from(w).expect("width").to_be_bytes());
        payload[26..28].copy_from_slice(&u16::try_from(h).expect("height").to_be_bytes());
    }
    if let Some((channels, sample_rate)) = audio {
        payload[16..18].copy_from_slice(&channels.to_be_bytes());
        payload[24..28].copy_from_slice(&(sample_rate << 16).to_be_bytes());
    }
    atom(sample_entry, &payload)
}

fn atom(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(
        &u32::try_from(payload.len() + 8)
            .expect("atom size")
            .to_be_bytes(),
    );
    out.extend_from_slice(&kind);
    out.extend_from_slice(payload);
    out
}

criterion_group!(
    benches,
    bench_probe_and_plan,
    bench_packet_planning,
    bench_subtitles
);
criterion_main!(benches);
