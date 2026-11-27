// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::atomic::Ordering;

use tatolab_browser_tab_protocol::{BrowserHostTabLogLevel, TabProtocolSocketFailure};
use tatolab_module_sdk::ModuleHostLogLevel;

use crate::browser_host_tab_client::{
    BrowserPageConfig, BrowserPageCounters, module_log_level_of, refuse_a_config_cef_cannot_render,
};
use crate::browser_page_refused::BrowserPageRefused;

fn config() -> BrowserPageConfig {
    BrowserPageConfig {
        url: "https://example.com".to_string(),
        width: 1920,
        height: 1080,
        frame_rate: 60,
        transparent_background: true,
        audio_sample_rate: 48_000,
        output_pool_rotation_depth: 4,
    }
}

#[test]
fn a_message_that_did_not_cross_is_refused_by_why() {
    assert!(matches!(
        BrowserPageRefused::of_the_tab_protocol(
            TabProtocolSocketFailure::Encoding("too large".into()),
            "the browser host did not answer the open",
        ),
        BrowserPageRefused::TabProtocolBroken(_)
    ));
    assert!(matches!(
        BrowserPageRefused::of_the_tab_protocol(
            TabProtocolSocketFailure::MissingDescriptor,
            "the browser host did not answer the open",
        ),
        BrowserPageRefused::TabProtocolBroken(_)
    ));
    assert_eq!(
        BrowserPageRefused::of_the_tab_protocol(
            TabProtocolSocketFailure::Ended,
            "the browser host did not answer the open",
        ),
        BrowserPageRefused::BrowserHostUnreachable(
            "the browser host did not answer the open: the connection ended".into()
        )
    );
}

#[test]
fn a_config_cef_can_render_passes() {
    refuse_a_config_cef_cannot_render(&config()).expect("1080p60 renders");
}

#[test]
fn what_cef_cannot_render_is_refused_by_name() {
    for (broken, named) in [
        (
            BrowserPageConfig {
                url: " ".into(),
                ..config()
            },
            "needs a url",
        ),
        (
            BrowserPageConfig {
                width: 0,
                ..config()
            },
            "0x1080",
        ),
        (
            BrowserPageConfig {
                frame_rate: 120,
                ..config()
            },
            "not 120",
        ),
        (
            BrowserPageConfig {
                audio_sample_rate: 4_000,
                ..config()
            },
            "not 4000",
        ),
        (
            BrowserPageConfig {
                output_pool_rotation_depth: 0,
                ..config()
            },
            "at least 1",
        ),
    ] {
        let refusal = refuse_a_config_cef_cannot_render(&broken).expect_err(named);
        assert!(refusal.contains(named), "{refusal}");
    }
}

#[test]
fn an_input_audio_block_with_sound_is_counted_as_delivered_and_as_sounding() {
    let counters = BrowserPageCounters::default();
    counters.count_an_input_audio_block_delivered(false);
    counters.count_an_input_audio_block_delivered(true);
    counters.count_an_input_audio_block_delivered(false);
    let delivered = &counters.page_input_audio_blocks_delivered;
    let with_sound = &counters.page_input_audio_blocks_with_sound_delivered;
    assert_eq!(delivered.load(Ordering::Relaxed), 3);
    assert_eq!(with_sound.load(Ordering::Relaxed), 1);
}

#[test]
fn every_host_log_level_keeps_its_loudness() {
    assert_eq!(
        [
            BrowserHostTabLogLevel::Error,
            BrowserHostTabLogLevel::Warn,
            BrowserHostTabLogLevel::Info,
            BrowserHostTabLogLevel::Debug,
        ]
        .map(module_log_level_of),
        [
            ModuleHostLogLevel::Error,
            ModuleHostLogLevel::Warn,
            ModuleHostLogLevel::Info,
            ModuleHostLogLevel::Debug,
        ]
    );
}
