use assert_cmd::Command;
use serde_json::Value;
use tempfile::tempdir;

#[test]
fn probe_cli_reports_mp4_streams() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.mp4");
    std::fs::write(&file, minimal_mp4()).unwrap();

    let output = Command::cargo_bin("chroma-engine")
        .unwrap()
        .arg("probe")
        .arg(&file)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();

    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["source"]["container"]["family"], "isoBmff");
    assert_eq!(json["source"]["container"]["brand"], "mp4");
    assert_eq!(json["durationMs"], 12_345);
    assert_eq!(json["tracks"][0]["id"], "v0");
    assert_eq!(json["tracks"][0]["kind"], "video");
    assert_eq!(json["tracks"][0]["codec"]["family"], "h264");
    assert_eq!(json["tracks"][0]["video"]["width"], 1920);
    assert_eq!(json["tracks"][0]["video"]["height"], 1080);
    assert_eq!(json["tracks"][1]["id"], "a0");
    assert_eq!(json["tracks"][1]["codec"]["family"], "aac");
    assert_eq!(json["tracks"][1]["audio"]["channels"], 2);
    assert_eq!(json["capabilities"]["canRemuxWithoutDecode"], true);
}

#[test]
fn probe_cli_reports_mkv_streams() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.mkv");
    std::fs::write(&file, minimal_mkv()).unwrap();

    let output = Command::cargo_bin("chroma-engine")
        .unwrap()
        .arg("probe")
        .arg(&file)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();

    assert_eq!(json["source"]["container"]["family"], "matroska");
    assert_eq!(json["source"]["container"]["brand"], "matroska");
    assert_eq!(json["durationMs"], 12_500);
    assert_eq!(json["tracks"][0]["id"], "v0");
    assert_eq!(json["tracks"][0]["codec"]["family"], "h264");
    assert_eq!(json["tracks"][0]["video"]["width"], 1920);
    assert_eq!(json["tracks"][1]["id"], "a0");
    assert_eq!(json["tracks"][1]["codec"]["family"], "aac");
    assert_eq!(json["tracks"][2]["id"], "s0");
    assert_eq!(json["tracks"][2]["codec"]["family"], "textSubtitle");
    assert_eq!(json["tracks"][2]["subtitle"]["format"], "text");
}

#[test]
fn plan_cli_reports_browser_decode_for_mkv_hevc_truehd() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("sample.mkv");
    std::fs::write(&file, minimal_browser_unfriendly_mkv()).unwrap();

    let output = Command::cargo_bin("chroma-engine")
        .unwrap()
        .arg("plan")
        .arg(&file)
        .arg("--target")
        .arg("browser")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();

    assert_eq!(json["constraints"]["target"], "browser");
    assert_eq!(json["selectedTracks"], serde_json::json!(["v0", "a0"]));
    assert_eq!(json["stages"][0]["kind"], "demux");
    assert!(json["stages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|stage| stage["id"] == "decode0"));
    assert!(json["transports"]
        .as_array()
        .unwrap()
        .iter()
        .any(|transport| transport["kind"] == "chromaSegments"));
}

fn minimal_mp4() -> Vec<u8> {
    let mut out = atom(
        b"ftyp",
        &[
            b"isom".as_slice(),
            &0_u32.to_be_bytes(),
            b"isom".as_slice(),
            b"mp42".as_slice(),
        ]
        .concat(),
    );
    out.extend_from_slice(&atom(
        b"moov",
        &[
            mvhd(1000, 12_345),
            trak(b"vide", b"avc1", Some((1920, 1080)), None, 12_345, 1000),
            trak(b"soun", b"mp4a", None, Some((2, 48000)), 592_560, 48000),
        ]
        .concat(),
    ));
    out
}

fn mvhd(timescale: u32, duration: u32) -> Vec<u8> {
    let mut payload = vec![0_u8; 20];
    payload[12..16].copy_from_slice(&timescale.to_be_bytes());
    payload[16..20].copy_from_slice(&duration.to_be_bytes());
    atom(b"mvhd", &payload)
}

fn trak(
    handler: &[u8; 4],
    sample_entry: &[u8; 4],
    size: Option<(u32, u32)>,
    audio: Option<(u16, u32)>,
    duration: u32,
    timescale: u32,
) -> Vec<u8> {
    atom(
        b"trak",
        &[
            tkhd(size),
            atom(
                b"mdia",
                &[
                    mdhd(timescale, duration),
                    hdlr(handler),
                    atom(b"minf", &atom(b"stbl", &stsd(sample_entry, size, audio))),
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
    atom(b"tkhd", &payload)
}

fn mdhd(timescale: u32, duration: u32) -> Vec<u8> {
    let mut payload = vec![0_u8; 20];
    payload[12..16].copy_from_slice(&timescale.to_be_bytes());
    payload[16..20].copy_from_slice(&duration.to_be_bytes());
    atom(b"mdhd", &payload)
}

fn hdlr(handler: &[u8; 4]) -> Vec<u8> {
    let mut payload = vec![0_u8; 12];
    payload[8..12].copy_from_slice(handler);
    atom(b"hdlr", &payload)
}

fn stsd(sample_entry: &[u8; 4], size: Option<(u32, u32)>, audio: Option<(u16, u32)>) -> Vec<u8> {
    let entry = sample_entry_atom(sample_entry, size, audio);
    let mut payload = Vec::new();
    payload.extend_from_slice(&[0, 0, 0, 0]);
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.extend_from_slice(&entry);
    atom(b"stsd", &payload)
}

fn sample_entry_atom(
    fourcc: &[u8; 4],
    size: Option<(u32, u32)>,
    audio: Option<(u16, u32)>,
) -> Vec<u8> {
    let mut payload = vec![0_u8; 28];
    if let Some((w, h)) = size {
        payload[24..26].copy_from_slice(&(w as u16).to_be_bytes());
        payload[26..28].copy_from_slice(&(h as u16).to_be_bytes());
    }
    if let Some((channels, sample_rate)) = audio {
        payload[16..18].copy_from_slice(&channels.to_be_bytes());
        payload[24..28].copy_from_slice(&(sample_rate << 16).to_be_bytes());
    }
    atom(fourcc, &payload)
}

fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

fn minimal_mkv() -> Vec<u8> {
    let info = ebml_elem(
        0x1549_a966,
        &[
            ebml_elem(0x002a_d7b1, &1_000_000_u64.to_be_bytes()[5..]),
            ebml_elem(0x4489, &12_500_f64.to_be_bytes()),
        ]
        .concat(),
    );
    let video = mkv_track(
        1,
        1,
        "V_MPEG4/ISO/AVC",
        &[ebml_elem(
            0xe0,
            &[
                ebml_elem(0xb0, &[0x07, 0x80]),
                ebml_elem(0xba, &[0x04, 0x38]),
            ]
            .concat(),
        )],
    );
    let audio = mkv_track(
        2,
        2,
        "A_AAC",
        &[ebml_elem(
            0xe1,
            &[
                ebml_elem(0x9f, &[0x02]),
                ebml_elem(0xb5, &48_000_f64.to_be_bytes()),
            ]
            .concat(),
        )],
    );
    let subtitle = mkv_track(3, 0x11, "S_TEXT/UTF8", &[]);
    let tracks = ebml_elem(0x1654_ae6b, &[video, audio, subtitle].concat());
    let segment = ebml_elem(0x1853_8067, &[info, tracks].concat());
    let mut bytes = ebml_elem(0x1a45_dfa3, &[]);
    bytes.extend_from_slice(&segment);
    bytes
}

fn minimal_browser_unfriendly_mkv() -> Vec<u8> {
    let info = ebml_elem(
        0x1549_a966,
        &[
            ebml_elem(0x002a_d7b1, &1_000_000_u64.to_be_bytes()[5..]),
            ebml_elem(0x4489, &12_500_f64.to_be_bytes()),
        ]
        .concat(),
    );
    let video = mkv_track(
        1,
        1,
        "V_MPEGH/ISO/HEVC",
        &[ebml_elem(
            0xe0,
            &[
                ebml_elem(0xb0, &[0x0f, 0x00]),
                ebml_elem(0xba, &[0x08, 0x70]),
            ]
            .concat(),
        )],
    );
    let audio = mkv_track(
        2,
        2,
        "A_TRUEHD",
        &[ebml_elem(
            0xe1,
            &[
                ebml_elem(0x9f, &[0x08]),
                ebml_elem(0xb5, &48_000_f64.to_be_bytes()),
            ]
            .concat(),
        )],
    );
    let tracks = ebml_elem(0x1654_ae6b, &[video, audio].concat());
    let segment = ebml_elem(0x1853_8067, &[info, tracks].concat());
    let mut bytes = ebml_elem(0x1a45_dfa3, &[]);
    bytes.extend_from_slice(&segment);
    bytes
}

fn mkv_track(number: u8, kind: u8, codec: &str, extra: &[Vec<u8>]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&ebml_elem(0xd7, &[number]));
    payload.extend_from_slice(&ebml_elem(0x83, &[kind]));
    payload.extend_from_slice(&ebml_elem(0x86, codec.as_bytes()));
    for e in extra {
        payload.extend_from_slice(e);
    }
    ebml_elem(0xae, &payload)
}

fn ebml_elem(id: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    write_ebml_id(id, &mut out);
    write_ebml_size(payload.len(), &mut out);
    out.extend_from_slice(payload);
    out
}

fn write_ebml_id(id: u32, out: &mut Vec<u8>) {
    let bytes = id.to_be_bytes();
    let first = bytes
        .iter()
        .position(|b| *b != 0)
        .unwrap_or(bytes.len() - 1);
    out.extend_from_slice(&bytes[first..]);
}

fn write_ebml_size(size: usize, out: &mut Vec<u8>) {
    assert!(size < 0x7f);
    out.push(0x80 | size as u8);
}
