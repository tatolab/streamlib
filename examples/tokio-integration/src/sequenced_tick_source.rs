// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The graph's producer — a Rust processor authored in the app itself.

use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::error::Result;
use streamlib::sdk::media_clock::MediaClock;
use streamlib::sdk::processors::ContinuousProcessor;

use crate::sequenced_tick::SequencedTick;

/// The attribute below takes a literal, so this is the spelling every caller
/// shares with it.
pub const TICK_OUTPUT_PORT_TO_DOWNSTREAM: &str = "tick_to_downstream";

#[streamlib::sdk::processor(
    description = "Publishes a sequenced tick on a fixed interval",
    execution = continuous(interval_ms = 40),
    output("tick_to_downstream", description = "Sequenced ticks"),
)]
pub struct SequencedTickSource {
    next_sequence_number: u64,
}

impl ContinuousProcessor for SequencedTickSource::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let tick = SequencedTick {
            sequence_number: self.next_sequence_number,
            emitted_at_monotonic_ns: MediaClock::now().as_nanos() as i64,
        };
        self.next_sequence_number = self.next_sequence_number.wrapping_add(1);
        self.outputs.write(TICK_OUTPUT_PORT_TO_DOWNSTREAM, &tick)
    }
}
