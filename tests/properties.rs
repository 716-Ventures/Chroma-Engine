use chroma_engine::{
    ContainerKind, PacketRef, ResourceError, TimeDelta, TimePoint, parse_subrip, plan_fixed_chunks,
    segment_webvtt, sniff_container, try_segment_webvtt,
};
use proptest::prelude::*;

proptest! {
    #[test]
    fn packet_chunks_cover_ordered_packets_once(
        packet_count in 1_usize..500,
        frame_ms in 1_u64..100,
        key_stride in 1_usize..60,
        target_ms in 1_u64..2_000,
    ) {
        let packets = synthetic_packets(packet_count, frame_ms, key_stride);
        let plan = plan_fixed_chunks(&packets, target_ms);

        prop_assert!(!plan.chunks.is_empty());
        prop_assert_eq!(plan.chunks.first().unwrap().packet_range.start, 0);
        prop_assert_eq!(
            usize::try_from(plan.chunks.last().unwrap().packet_range.end).unwrap(),
            packet_count
        );

        let mut expected_start = 0_u32;
        for (index, chunk) in plan.chunks.iter().enumerate() {
            prop_assert_eq!(usize::try_from(chunk.index).unwrap(), index);
            prop_assert_eq!(chunk.packet_range.start, expected_start);
            prop_assert!(chunk.packet_range.end > chunk.packet_range.start);
            prop_assert!(usize::try_from(chunk.packet_range.end).unwrap() <= packet_count);
            prop_assert!(chunk.duration.as_millis() > 0);
            if chunk.key_aligned {
                let start = usize::try_from(chunk.packet_range.start).unwrap();
                prop_assert!(packets[start].keyframe);
            }
            expected_start = chunk.packet_range.end;
        }
    }

    #[test]
    fn subrip_round_trip_produces_ordered_webvtt_segments(
        cue_count in 1_usize..300,
        cue_duration_ms in 1_u64..4_000,
        gap_ms in 0_u64..1_000,
        // Keep the success property within the documented segmentation budgets.
        // Tiny windows over long tracks are covered by the rejection regression below.
        segment_ms in 100_u64..10_000,
    ) {
        let source = synthetic_subrip(cue_count, cue_duration_ms, gap_ms);
        let cues = parse_subrip(&source);
        prop_assert_eq!(cues.len(), cue_count);
        prop_assert!(cues.iter().all(|cue| cue.end_ms > cue.start_ms));

        let segments = segment_webvtt(&cues, segment_ms, "s0");
        prop_assert!(!segments.is_empty());

        let mut expected_start = 0_u64;
        for (index, segment) in segments.iter().enumerate() {
            prop_assert_eq!(usize::try_from(segment.index).unwrap(), index);
            prop_assert_eq!(segment.start_ms, expected_start);
            prop_assert!(segment.duration_ms > 0);
            prop_assert!(segment.duration_ms <= segment_ms);
            let expected_uri_suffix = format!("seg-{index:05}.vtt");
            prop_assert!(segment.uri.ends_with(&expected_uri_suffix));
            prop_assert!(segment.body.starts_with("WEBVTT\n\n"));
            expected_start = expected_start.saturating_add(segment_ms);
        }
    }

    #[test]
    fn container_sniffing_never_panics_for_arbitrary_headers(head in prop::collection::vec(any::<u8>(), 0..512)) {
        let kind = sniff_container(&head);
        prop_assert!(matches!(
            kind,
            ContainerKind::Mp4
                | ContainerKind::Mov
                | ContainerKind::Matroska
                | ContainerKind::Webm
                | ContainerKind::Unknown
        ));
    }
}

#[test]
fn subtitle_segmentation_rejects_ci_regression_over_budget() {
    let cues = parse_subrip(&synthetic_subrip(119, 2823, 543));
    assert_eq!(cues.len(), 119);
    assert!(matches!(
        try_segment_webvtt(&cues, 1, "s0"),
        Err(ResourceError::Exceeded {
            resource: "subtitle segments",
            requested,
            limit: 100_000,
        }) if requested == cues.last().unwrap().end_ms && requested > 100_000
    ));
    assert!(segment_webvtt(&cues, 1, "s0").is_empty());
}

#[test]
fn subtitle_segmentation_accepts_tiny_windows_within_budget() {
    let cues = parse_subrip(&synthetic_subrip(1, 3, 0));
    let segments = try_segment_webvtt(&cues, 1, "s0").unwrap();
    assert_eq!(segments.len(), 3);
    for (index, segment) in segments.iter().enumerate() {
        assert_eq!(segment.start_ms, index as u64);
        assert_eq!(segment.duration_ms, 1);
        assert!(segment.body.contains("Caption 1"));
    }
}

fn synthetic_packets(count: usize, frame_ms: u64, key_stride: usize) -> Vec<PacketRef> {
    (0..count)
        .map(|index| {
            let units = u64::try_from(index).unwrap().saturating_mul(frame_ms);
            PacketRef {
                source_offset: u64::try_from(index).unwrap().saturating_mul(1_024),
                size: 1_024,
                pts: TimePoint::millis(units),
                dts: TimePoint::millis(units),
                duration: TimeDelta::millis(frame_ms),
                keyframe: index.is_multiple_of(key_stride),
            }
        })
        .collect()
}

fn synthetic_subrip(count: usize, cue_duration_ms: u64, gap_ms: u64) -> String {
    let mut out = String::new();
    let mut start = 0_u64;
    for index in 0..count {
        let end = start.saturating_add(cue_duration_ms);
        out.push_str(&format!(
            "{}\n{} --> {}\nCaption {}\n\n",
            index + 1,
            subrip_timestamp(start),
            subrip_timestamp(end),
            index + 1
        ));
        start = end.saturating_add(gap_ms);
    }
    out
}

fn subrip_timestamp(ms: u64) -> String {
    let total_seconds = ms / 1_000;
    let millis = ms % 1_000;
    let seconds = total_seconds % 60;
    let total_minutes = total_seconds / 60;
    let minutes = total_minutes % 60;
    let hours = total_minutes / 60;
    format!("{hours:02}:{minutes:02}:{seconds:02},{millis:03}")
}
