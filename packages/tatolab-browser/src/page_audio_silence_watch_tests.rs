// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::atomic::Ordering;

use tatolab_browser_tab_protocol::{PageInputTrack, PageInputTrackKind};

use crate::browser_host_tab_client::BrowserPageCounters;
use crate::page_audio_silence_watch::{PAGE_AUDIO_SILENCE_GRACE, PageAudioSilenceWatch};

const FIRST_SEEN_AT_NS: i64 = 1_000_000_000;

fn grace_ns() -> i64 {
    PAGE_AUDIO_SILENCE_GRACE.as_nanos() as i64
}

fn watch_over(tracks: &[(&str, PageInputTrackKind)]) -> PageAudioSilenceWatch {
    let tracks: Vec<PageInputTrack> = tracks
        .iter()
        .map(|(input_port_name, kind)| PageInputTrack {
            input_port_name: input_port_name.to_string(),
            kind: *kind,
        })
        .collect();
    PageAudioSilenceWatch::of_the_page_input_tracks(&tracks)
}

/// Counters of a page handed `input_audio_blocks_delivered` blocks, all with
/// sound, that has published `audio_blocks_published`.
fn counters_with(
    input_audio_blocks_delivered: u64,
    audio_blocks_published: u64,
) -> BrowserPageCounters {
    let counters = BrowserPageCounters::default();
    counters
        .page_input_audio_blocks_delivered
        .store(input_audio_blocks_delivered, Ordering::Relaxed);
    counters
        .page_input_audio_blocks_with_sound_delivered
        .store(input_audio_blocks_delivered, Ordering::Relaxed);
    counters
        .audio_blocks_published
        .store(audio_blocks_published, Ordering::Relaxed);
    counters
}

#[test]
fn a_page_silent_past_the_grace_is_warned_about_once_naming_every_wired_audio_input() {
    let mut watch = watch_over(&[
        ("audio_in_0", PageInputTrackKind::Audio),
        ("video_in_0", PageInputTrackKind::Video),
        ("audio_in_2", PageInputTrackKind::Audio),
    ]);
    assert_eq!(
        watch.warning_due(FIRST_SEEN_AT_NS, &counters_with(1, 0)),
        None
    );

    let warning = watch
        .warning_due(FIRST_SEEN_AT_NS + grace_ns(), &counters_with(250, 0))
        .expect("a page silent for the whole grace is warned about");
    assert!(warning.contains("(audio_in_0, audio_in_2)"), "{warning}");
    assert!(
        warning.contains("published no audio in the 5 s"),
        "{warning}"
    );
    assert!(
        warning.contains("window.tatolab.inputTrack(\"audio_in_0\")"),
        "{warning}"
    );

    assert_eq!(
        watch.warning_due(FIRST_SEEN_AT_NS + 10 * grace_ns(), &counters_with(2500, 0)),
        None
    );
}

#[test]
fn the_grace_runs_from_when_an_input_block_is_first_seen_to_reach_the_page() {
    let mut watch = watch_over(&[("audio_in_0", PageInputTrackKind::Audio)]);
    assert_eq!(
        watch.warning_due(FIRST_SEEN_AT_NS, &counters_with(0, 0)),
        None
    );
    assert_eq!(
        watch.warning_due(FIRST_SEEN_AT_NS + 10 * grace_ns(), &counters_with(0, 0)),
        None
    );

    let first_seen_at_ns = FIRST_SEEN_AT_NS + 10 * grace_ns();
    assert_eq!(
        watch.warning_due(first_seen_at_ns, &counters_with(1, 0)),
        None
    );
    assert_eq!(
        watch.warning_due(first_seen_at_ns + grace_ns() - 1, &counters_with(100, 0)),
        None
    );
    assert!(
        watch
            .warning_due(first_seen_at_ns + grace_ns(), &counters_with(100, 0))
            .is_some()
    );
}

#[test]
fn a_page_that_publishes_audio_is_never_warned_about() {
    let mut watch = watch_over(&[("audio_in_0", PageInputTrackKind::Audio)]);
    assert_eq!(
        watch.warning_due(FIRST_SEEN_AT_NS, &counters_with(1, 1)),
        None
    );

    assert_eq!(
        watch.warning_due(FIRST_SEEN_AT_NS + 10 * grace_ns(), &counters_with(500, 1)),
        None
    );
}

#[test]
fn a_page_with_no_wired_audio_input_is_never_warned_about() {
    let mut watch = watch_over(&[("video_in_0", PageInputTrackKind::Video)]);
    assert_eq!(
        watch.warning_due(FIRST_SEEN_AT_NS, &counters_with(1, 0)),
        None
    );

    assert_eq!(
        watch.warning_due(FIRST_SEEN_AT_NS + 10 * grace_ns(), &counters_with(1, 0)),
        None
    );
}

#[test]
fn inputs_that_carried_only_digital_silence_are_named_as_the_silent_source() {
    let mut watch = watch_over(&[
        ("audio_in_0", PageInputTrackKind::Audio),
        ("audio_in_1", PageInputTrackKind::Audio),
    ]);
    let only_silence_delivered = BrowserPageCounters::default();
    only_silence_delivered
        .page_input_audio_blocks_delivered
        .store(1, Ordering::Relaxed);
    assert_eq!(
        watch.warning_due(FIRST_SEEN_AT_NS, &only_silence_delivered),
        None
    );

    only_silence_delivered
        .page_input_audio_blocks_delivered
        .store(250, Ordering::Relaxed);
    let warning = watch
        .warning_due(FIRST_SEEN_AT_NS + grace_ns(), &only_silence_delivered)
        .expect("a page handed only silence for the whole grace is warned about");
    assert!(warning.contains("(audio_in_0, audio_in_1)"), "{warning}");
    assert!(warning.contains("digital silence"), "{warning}");
    assert!(warning.contains("check the source"), "{warning}");
    assert!(!warning.contains("inputTrack"), "{warning}");
}

#[test]
fn one_block_with_sound_makes_it_the_page_that_is_warned_about() {
    let mut watch = watch_over(&[("audio_in_0", PageInputTrackKind::Audio)]);
    let one_block_with_sound = BrowserPageCounters::default();
    one_block_with_sound
        .page_input_audio_blocks_delivered
        .store(250, Ordering::Relaxed);
    one_block_with_sound
        .page_input_audio_blocks_with_sound_delivered
        .store(1, Ordering::Relaxed);
    assert_eq!(
        watch.warning_due(FIRST_SEEN_AT_NS, &one_block_with_sound),
        None
    );

    let warning = watch
        .warning_due(FIRST_SEEN_AT_NS + grace_ns(), &one_block_with_sound)
        .expect("a page handed sound for the whole grace is warned about");
    assert!(warning.contains("published no audio"), "{warning}");
    assert!(!warning.contains("digital silence"), "{warning}");
}
