// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Telling a page that plays none of its wired audio apart from a broken one.
//!
//! A page hears a wired `audio_in_N` only once it plays the track
//! `window.tatolab.inputTrack` gives it. A page that never does, or whose
//! AudioContext never starts, leaves `audio` silent with nothing in the log, and
//! that silence looks the same as a broken input.

use std::sync::atomic::Ordering;
use std::time::Duration;

use tatolab_browser_tab_protocol::{PageInputTrack, PageInputTrackKind};

use crate::browser_host_tab_client::BrowserPageCounters;

/// How long after the first input block reaches the page it may stay silent
/// before the watch warns.
pub const PAGE_AUDIO_SILENCE_GRACE: Duration = Duration::from_secs(5);

/// Watches one tab for audio inputs that reach the page while the page
/// publishes no audio, and says so once.
#[derive(Debug)]
pub struct PageAudioSilenceWatch {
    wired_audio_input_port_names: Vec<String>,
    first_input_block_reached_the_page_at_ns: Option<i64>,
    settled: bool,
}

impl PageAudioSilenceWatch {
    /// A watch over a page taking `page_input_tracks`; one with no audio track
    /// never warns.
    pub fn of_the_page_input_tracks<'track>(
        page_input_tracks: impl IntoIterator<Item = &'track PageInputTrack>,
    ) -> Self {
        let wired_audio_input_port_names: Vec<String> = page_input_tracks
            .into_iter()
            .filter(|track| track.kind == PageInputTrackKind::Audio)
            .map(|track| track.input_port_name.clone())
            .collect();
        Self {
            settled: wired_audio_input_port_names.is_empty(),
            wired_audio_input_port_names,
            first_input_block_reached_the_page_at_ns: None,
        }
    }

    /// The warning to log at `now_ns`, read from what the page's `counters`
    /// say it was handed and has published: once, when the grace has passed
    /// since an input block was first seen to reach the page and the page has
    /// published no audio. After a warning, or once the page publishes audio,
    /// it is `None` for good.
    pub fn warning_due(&mut self, now_ns: i64, counters: &BrowserPageCounters) -> Option<String> {
        if self.settled {
            return None;
        }
        if counters.audio_blocks_published.load(Ordering::Relaxed) > 0 {
            self.settled = true;
            return None;
        }
        if counters
            .page_input_audio_blocks_delivered
            .load(Ordering::Relaxed)
            == 0
        {
            return None;
        }
        let first_at_ns = *self
            .first_input_block_reached_the_page_at_ns
            .get_or_insert(now_ns);
        let silent_for =
            Duration::from_nanos(u64::try_from(now_ns.saturating_sub(first_at_ns)).unwrap_or(0));
        if silent_for < PAGE_AUDIO_SILENCE_GRACE {
            return None;
        }
        self.settled = true;
        let wired = self.wired_audio_input_port_names.join(", ");
        let first_wired = &self.wired_audio_input_port_names[0];
        Some(format!(
            "the page has published no audio in the {} s since its wired audio inputs \
             ({wired}) began reaching it, so `audio` stays silent; a page plays an input \
             by playing `window.tatolab.inputTrack(\"{first_wired}\")` through an audio \
             element or a started AudioContext",
            silent_for.as_secs()
        ))
    }
}
