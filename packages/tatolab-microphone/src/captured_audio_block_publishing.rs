// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The thread that drains the hand-off ring and publishes each captured block
//! on the node's `audio` port through the module host table, with no Python
//! attached.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tatolab_module_sdk::{
    ModuleHost, ModuleHostAudioCaptureStreamLiveness, ModuleHostAudioStreamFormat,
    ModuleHostLogLevel, ModuleHostOutputPort,
};

use crate::captured_audio_block_hand_off_ring::{
    CapturedAudioBlockAwaitingPublish, CapturedAudioBlockHandOffRing,
    NextCapturedAudioBlockToPublish,
};
use crate::channels_block_of_a_captured_block::channels_block_of_a_captured_block;

/// The target this module's records carry in the node's log.
pub(crate) const MICROPHONE_LOG_TARGET: &str = "tatolab.microphone";

/// How often the thread comes up for air to notice a stop or a dead device
/// while no block is arriving.
const PUBLISH_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Drops between warnings once the first has been logged.
const DROPPED_BLOCKS_BETWEEN_WARNINGS: u64 = 300;

/// Failed writes between reports once the first has been logged.
const FAILED_WRITES_BETWEEN_REPORTS: u64 = 300;

/// What the publishing thread reads and the node reports on.
pub(crate) struct CapturedAudioBlockPublishing {
    pub(crate) module_host: ModuleHost,
    pub(crate) audio_output_port: ModuleHostOutputPort,
    pub(crate) capture_stream_liveness: ModuleHostAudioCaptureStreamLiveness,
    pub(crate) stream_format: ModuleHostAudioStreamFormat,
    pub(crate) hand_off_ring: Arc<CapturedAudioBlockHandOffRing>,
    pub(crate) is_publishing: Arc<AtomicBool>,
    pub(crate) published_block_count: Arc<AtomicU64>,
}

impl CapturedAudioBlockPublishing {
    /// Publish until the node stops, the hand-off ends or the device does,
    /// then publish whatever was already captured.
    pub(crate) fn publish_until_stopped(self) {
        let mut dropped_blocks_last_reported = 0u64;
        let mut consecutive_failed_writes = 0u64;
        while self.is_publishing.load(Ordering::Acquire) {
            if let Some(failure) = self.failure_that_ended_the_capture() {
                self.module_host.log(
                    ModuleHostLogLevel::Error,
                    MICROPHONE_LOG_TARGET,
                    &format!(
                        "MicrophoneSource: the capture device stopped serving this node, so no \
                         further blocks will be captured; what was captured is published and the \
                         run's audio ends there: {failure}"
                    ),
                );
                break;
            }
            match self
                .hand_off_ring
                .wait_for_next_block_to_publish(PUBLISH_WAIT_POLL_INTERVAL)
            {
                NextCapturedAudioBlockToPublish::Block(captured) => {
                    self.publish_one(captured, &mut consecutive_failed_writes);
                    self.warn_about_any_new_drops(&mut dropped_blocks_last_reported);
                }
                NextCapturedAudioBlockToPublish::WaitTimedOut => {}
                NextCapturedAudioBlockToPublish::HandOffEnded => break,
            }
        }
        // A stop is not a discard: what the device captured before it is
        // already accounted for in the timestamps.
        while let NextCapturedAudioBlockToPublish::Block(captured) = self
            .hand_off_ring
            .wait_for_next_block_to_publish(Duration::ZERO)
        {
            self.publish_one(captured, &mut consecutive_failed_writes);
        }
        self.warn_about_any_new_drops(&mut dropped_blocks_last_reported);
    }

    fn failure_that_ended_the_capture(&self) -> Option<String> {
        match self.capture_stream_liveness.failure_that_ended_the_stream() {
            Ok(failure) => failure,
            Err(refusal) => Some(refusal.to_string()),
        }
    }

    fn publish_one(
        &self,
        captured: CapturedAudioBlockAwaitingPublish,
        consecutive_failed_writes: &mut u64,
    ) {
        let first_sample_stream_timestamp_ns = self
            .module_host
            .stream_time_ns_of_a_machine_instant(captured.first_sample_timestamp_ns);
        let captured_channels_block = channels_block_of_a_captured_block(
            &captured.interleaved_sample_bytes,
            captured.sample_count,
            captured.start_sample,
            self.stream_format,
        );
        // The device stamped the block; a write stamped "now" would name the
        // instant of publication, not of capture.
        let written = self
            .audio_output_port
            .write_described(
                &captured_channels_block.planar_payload,
                Some(first_sample_stream_timestamp_ns),
                true,
                &captured_channels_block.block.payload_descriptor(),
            )
            .map_err(|write_failure| write_failure.to_string());
        match written {
            Ok(()) => {
                *consecutive_failed_writes = 0;
                self.published_block_count.fetch_add(1, Ordering::Relaxed);
            }
            Err(failure) => {
                *consecutive_failed_writes += 1;
                if (*consecutive_failed_writes - 1).is_multiple_of(FAILED_WRITES_BETWEEN_REPORTS) {
                    self.module_host.log(
                        ModuleHostLogLevel::Error,
                        MICROPHONE_LOG_TARGET,
                        &format!(
                            "MicrophoneSource: failed to write an audio block \
                             ({consecutive_failed_writes} in a row): {failure}"
                        ),
                    );
                }
            }
        }
    }

    fn warn_about_any_new_drops(&self, dropped_blocks_last_reported: &mut u64) {
        let dropped_block_count = self.hand_off_ring.dropped_block_count();
        let worth_reporting = (*dropped_blocks_last_reported == 0 && dropped_block_count > 0)
            || dropped_block_count
                >= dropped_blocks_last_reported.saturating_add(DROPPED_BLOCKS_BETWEEN_WARNINGS);
        if !worth_reporting {
            return;
        }
        *dropped_blocks_last_reported = dropped_block_count;
        self.module_host.log(
            ModuleHostLogLevel::Warn,
            MICROPHONE_LOG_TARGET,
            &format!(
                "MicrophoneSource: {dropped_block_count} block(s) dropped at the device edge — \
                 the publisher is not keeping up. The gap is derivable from the timestamps and \
                 sample counts of the blocks either side of it."
            ),
        );
    }
}
