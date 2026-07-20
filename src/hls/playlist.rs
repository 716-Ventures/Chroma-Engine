pub(super) fn master_playlist_body(
    video_codec: &str,
    audio_codec: &str,
    bandwidth_bits_per_second: u64,
) -> String {
    format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:BANDWIDTH={bandwidth_bits_per_second},CODECS=\"{video_codec},{audio_codec}\"\n0/playlist.m3u8\n"
    )
}

pub(super) fn media_playlist_body(target_duration_seconds: u64, durations_ms: &[u64]) -> String {
    let mut out = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{target_duration_seconds}\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n"
    );
    for (index, duration_ms) in durations_ms.iter().enumerate() {
        out.push_str(&format!(
            "#EXTINF:{:.3},\n{}\n",
            *duration_ms as f64 / 1000.0,
            segment_name(index)
        ));
    }
    out.push_str("#EXT-X-ENDLIST\n");
    out
}

pub(super) fn fmp4_media_playlist_body(
    target_duration_seconds: u64,
    durations_ms: &[u64],
) -> String {
    let mut out = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{target_duration_seconds}\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MAP:URI=\"init.mp4\"\n"
    );
    for (index, duration_ms) in durations_ms.iter().enumerate() {
        out.push_str(&format!(
            "#EXTINF:{:.3},\n{}\n",
            *duration_ms as f64 / 1000.0,
            fmp4_segment_name(index)
        ));
    }
    out.push_str("#EXT-X-ENDLIST\n");
    out
}

pub(super) fn segment_name(index: usize) -> String {
    format!("seg-{index:05}.ts")
}

pub(super) fn fmp4_segment_name(index: usize) -> String {
    format!("seg-{index:05}.m4s")
}
