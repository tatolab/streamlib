// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The thread that reads each block off the node's lent `audio` port through
//! the module host table and queues its samples for the device, with no Python
//! attached.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tatolab_module_sdk::{
    ModuleHost, ModuleHostAudioPlaybackStreamLiveness, ModuleHostAudioStreamFormat,
    ModuleHostInputPort, ModuleHostLogLevel,
};

use crate::audio_samples_awaiting_playback_ring::{
    AudioSamplesAwaitingPlaybackRing, AudioSamplesHandOffOutcome,
};
use crate::channels_block_to_play::interleaved_sample_bytes_to_play;

/// The target this module's records carry in the node's log.
pub(crate) const SPEAKER_LOG_TARGET: &str = "tatolab.speaker";

/// How long the thread waits for a block before it comes up to notice a stop,
/// a dead device or new underruns. Short against a device period, so a stop is
/// prompt; a block arriving wakes it at once.
const DRAIN_IDLE_WAIT: Duration = Duration::from_millis(5);

/// How long the thread sits inside a wait for ring room before it comes up for
/// air. Stopping ends playback, which releases it for good.
const ROOM_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Failed reads or refused blocks between reports once the first is logged.
const FAILURES_BETWEEN_REPORTS: u64 = 300;

/// What the drain thread reads and the node reports on.
pub(crate) struct AudioBlockDraining {
    pub(crate) module_host: ModuleHost,
    pub(crate) audio_input_port: ModuleHostInputPort,
    pub(crate) playback_stream_liveness: ModuleHostAudioPlaybackStreamLiveness,
    pub(crate) playback_stream_format: ModuleHostAudioStreamFormat,
    pub(crate) samples_awaiting_playback: Arc<AudioSamplesAwaitingPlaybackRing>,
    pub(crate) silence_bytes_between_underrun_warnings: u64,
    pub(crate) is_draining: Arc<AtomicBool>,
    pub(crate) played_block_count: Arc<AtomicU64>,
    pub(crate) refused_block_count: Arc<AtomicU64>,
}

impl AudioBlockDraining {
    /// Drain until the node stops, playback ends or the device does.
    pub(crate) fn drain_until_stopped(self) {
        let mut underrun_bytes_last_reported = 0u64;
        let mut consecutive_failed_reads = 0u64;
        while self.is_draining.load(Ordering::Acquire) {
            // Asked ahead of the underrun check: a device that stopped
            // underruns for the rest of the run, and that line would say the
            // least.
            if let Some(failure) = self.failure_that_ended_the_playback() {
                self.module_host.log(
                    ModuleHostLogLevel::Error,
                    SPEAKER_LOG_TARGET,
                    &format!(
                        "SpeakerSink: the playback device stopped serving this node, so nothing \
                         further will be played. Blocks still arriving on `audio` are dropped by \
                         the link's own ring: {failure}"
                    ),
                );
                break;
            }
            self.warn_about_any_new_underruns(&mut underrun_bytes_last_reported);

            match self.audio_input_port.read() {
                Ok(Some(message)) => {
                    consecutive_failed_reads = 0;
                    let to_play = message
                        .payload_descriptor()
                        .map_err(|refusal| refusal.to_string())
                        .and_then(|payload_descriptor| {
                            interleaved_sample_bytes_to_play(
                                &payload_descriptor,
                                message.payload(),
                                self.playback_stream_format,
                            )
                        });
                    match to_play {
                        Ok(interleaved_sample_bytes) => {
                            drop(message);
                            // The wait is this speaker pacing itself against
                            // its own ring. A producer outrunning it loses
                            // blocks at the port, counted against its link.
                            if self.samples_awaiting_playback.hand_off_for_playback(
                                &interleaved_sample_bytes,
                                ROOM_WAIT_POLL_INTERVAL,
                            ) == AudioSamplesHandOffOutcome::PlaybackEnded
                            {
                                break;
                            }
                            self.played_block_count.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(refusal) => self.report_a_refused_block(&refusal),
                    }
                }
                Ok(None) => {
                    if let Err(wait_refusal) =
                        self.module_host.wait_for_any_input_port(DRAIN_IDLE_WAIT)
                    {
                        self.report_a_failed_read(&mut consecutive_failed_reads, &wait_refusal);
                        std::thread::sleep(DRAIN_IDLE_WAIT);
                    }
                }
                Err(read_refusal) => {
                    self.report_a_failed_read(&mut consecutive_failed_reads, &read_refusal);
                    std::thread::sleep(DRAIN_IDLE_WAIT);
                }
            }
        }
    }

    fn failure_that_ended_the_playback(&self) -> Option<String> {
        match self
            .playback_stream_liveness
            .failure_that_ended_the_stream()
        {
            Ok(failure) => failure,
            Err(refusal) => Some(refusal.to_string()),
        }
    }

    fn report_a_refused_block(&self, refusal: &str) {
        let refused_block_count = self.refused_block_count.fetch_add(1, Ordering::Relaxed) + 1;
        if (refused_block_count - 1).is_multiple_of(FAILURES_BETWEEN_REPORTS) {
            self.module_host.log(
                ModuleHostLogLevel::Error,
                SPEAKER_LOG_TARGET,
                &format!(
                    "SpeakerSink: a block on `audio` was not played ({refused_block_count} so \
                     far): {refusal}"
                ),
            );
        }
    }

    fn report_a_failed_read(
        &self,
        consecutive_failed_reads: &mut u64,
        refusal: &impl std::fmt::Display,
    ) {
        *consecutive_failed_reads += 1;
        if (*consecutive_failed_reads - 1).is_multiple_of(FAILURES_BETWEEN_REPORTS) {
            self.module_host.log(
                ModuleHostLogLevel::Error,
                SPEAKER_LOG_TARGET,
                &format!(
                    "SpeakerSink: failed to read `audio` ({consecutive_failed_reads} in a row): \
                     {refusal}"
                ),
            );
        }
    }

    fn warn_about_any_new_underruns(&self, underrun_bytes_last_reported: &mut u64) {
        let underrun_byte_count = self.samples_awaiting_playback.underrun_byte_count();
        let worth_reporting = (*underrun_bytes_last_reported == 0 && underrun_byte_count > 0)
            || underrun_byte_count
                >= underrun_bytes_last_reported
                    .saturating_add(self.silence_bytes_between_underrun_warnings);
        if !worth_reporting {
            return;
        }
        *underrun_bytes_last_reported = underrun_byte_count;
        self.module_host.log(
            ModuleHostLogLevel::Warn,
            SPEAKER_LOG_TARGET,
            &format!(
                "SpeakerSink: the device was given {underrun_byte_count} byte(s) of silence so \
                 far because the graph had no samples ready; none is ever filled uncounted."
            ),
        );
    }
}
