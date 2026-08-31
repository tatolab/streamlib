// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The graph's consumer — reports the cadence the ticks actually arrived at.

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ReactiveProcessor;

use crate::sequenced_tick::SequencedTick;

/// The attribute below takes a literal, so this is the spelling every caller
/// shares with it.
pub const TICK_INPUT_PORT_FROM_UPSTREAM: &str = "tick_from_upstream";

/// The shortest window an interval can be read out of: two stamps.
const FEWEST_TICKS_A_CADENCE_REPORT_CAN_READ: u32 = 2;

/// How many ticks the sink gathers before it reports on them. Below
/// [`FEWEST_TICKS_A_CADENCE_REPORT_CAN_READ`] there is no interval to measure,
/// and `setup` refuses rather than reporting nothing forever.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TickCadenceReportingSinkConfig {
    pub ticks_per_cadence_report: u32,
}

impl Default for TickCadenceReportingSinkConfig {
    fn default() -> Self {
        Self {
            ticks_per_cadence_report: 25,
        }
    }
}

#[streamlib::sdk::processor(
    description = "Reports the cadence a window of ticks actually arrived at",
    execution = reactive,
    config = crate::tick_cadence_reporting_sink::TickCadenceReportingSinkConfig,
    input(
        "tick_from_upstream",
        delivery_profile = "ordered",
        description = "Sequenced ticks from upstream"
    ),
)]
pub struct TickCadenceReportingSink {
    ticks_in_the_current_window: Vec<SequencedTick>,
}

impl ReactiveProcessor for TickCadenceReportingSink::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        refuse_a_window_too_short_to_hold_an_interval(self.config.ticks_per_cadence_report)
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        // One wake can carry more than one tick, so drain rather than read
        // once: `ordered` hands them back oldest-first, which is what makes
        // the intervals below mean anything.
        while self.inputs.has_data(TICK_INPUT_PORT_FROM_UPSTREAM) {
            let tick: SequencedTick = self.inputs.read(TICK_INPUT_PORT_FROM_UPSTREAM)?;
            self.ticks_in_the_current_window.push(tick);

            if self.ticks_in_the_current_window.len()
                >= self.config.ticks_per_cadence_report as usize
            {
                self.report_the_window_and_start_another();
            }
        }
        Ok(())
    }
}

impl TickCadenceReportingSink::Processor {
    fn report_the_window_and_start_another(&mut self) {
        if let Some(cadence) = observed_cadence_of(&self.ticks_in_the_current_window) {
            tracing::info!(
                tick_count = cadence.tick_count,
                skipped_tick_count = cadence.skipped_tick_count,
                mean_interval_ms = cadence.mean_interval_ms,
                widest_interval_ms = cadence.widest_interval_ms,
                "the graph's consumer measured the cadence it is being fed at"
            );
        }
        self.ticks_in_the_current_window.clear();
    }
}

/// What one window of ticks says about how it actually arrived.
#[derive(Debug, PartialEq)]
struct ObservedTickCadence {
    tick_count: usize,
    /// Sequence numbers the window never saw. A tick the producer emitted and
    /// this end never got — zero while the link keeps up.
    skipped_tick_count: u64,
    mean_interval_ms: f64,
    widest_interval_ms: f64,
}

/// Refuse a window size no interval can be read out of.
///
/// A window of one satisfies the report threshold on its first tick, measures
/// nothing, and clears — so an unchecked knob leaves the processor running
/// forever and saying nothing. Refusing at `setup` says it once, up front.
fn refuse_a_window_too_short_to_hold_an_interval(ticks_per_cadence_report: u32) -> Result<()> {
    if ticks_per_cadence_report < FEWEST_TICKS_A_CADENCE_REPORT_CAN_READ {
        return Err(Error::Configuration(format!(
            "ticks_per_cadence_report is {ticks_per_cadence_report}, and an interval needs at \
             least {FEWEST_TICKS_A_CADENCE_REPORT_CAN_READ} ticks to be read out of"
        )));
    }
    Ok(())
}

/// Read the cadence out of one window of ticks, in the order they arrived.
///
/// `None` below two ticks: an interval needs two stamps.
fn observed_cadence_of(ticks: &[SequencedTick]) -> Option<ObservedTickCadence> {
    if ticks.len() < FEWEST_TICKS_A_CADENCE_REPORT_CAN_READ as usize {
        return None;
    }
    let (first, last) = (ticks.first()?, ticks.last()?);

    let widest_interval_ns = ticks
        .windows(2)
        .map(|pair| pair[1].emitted_at_monotonic_ns - pair[0].emitted_at_monotonic_ns)
        .max()
        .unwrap_or(0);
    let interval_count = (ticks.len() - 1) as u64;
    let span_ns = last.emitted_at_monotonic_ns - first.emitted_at_monotonic_ns;

    Some(ObservedTickCadence {
        tick_count: ticks.len(),
        skipped_tick_count: last
            .sequence_number
            .saturating_sub(first.sequence_number)
            .saturating_sub(interval_count),
        mean_interval_ms: nanoseconds_as_milliseconds(span_ns) / interval_count as f64,
        widest_interval_ms: nanoseconds_as_milliseconds(widest_interval_ns),
    })
}

fn nanoseconds_as_milliseconds(nanoseconds: i64) -> f64 {
    nanoseconds as f64 / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window arriving on an exact cadence, with nothing missing.
    fn evenly_spaced_ticks(count: u64, interval_ms: i64) -> Vec<SequencedTick> {
        (0..count)
            .map(|sequence_number| SequencedTick {
                sequence_number,
                emitted_at_monotonic_ns: sequence_number as i64 * interval_ms * 1_000_000,
            })
            .collect()
    }

    #[test]
    fn an_even_window_reports_the_interval_it_arrived_on() {
        let cadence = observed_cadence_of(&evenly_spaced_ticks(5, 40))
            .expect("five ticks are enough for an interval");

        assert_eq!(cadence.tick_count, 5);
        assert_eq!(cadence.skipped_tick_count, 0);
        assert_eq!(cadence.mean_interval_ms, 40.0);
        assert_eq!(cadence.widest_interval_ms, 40.0);
    }

    /// The mean says the run was on cadence; only the widest interval shows
    /// the stall. Reporting both is the point.
    #[test]
    fn one_long_gap_shows_in_the_widest_interval_and_not_in_the_mean() {
        let mut ticks = evenly_spaced_ticks(5, 40);
        for tick in &mut ticks[3..] {
            tick.emitted_at_monotonic_ns += 200 * 1_000_000;
        }

        let cadence = observed_cadence_of(&ticks).expect("five ticks are enough for an interval");

        assert_eq!(cadence.widest_interval_ms, 240.0);
        assert_eq!(cadence.mean_interval_ms, 90.0);
    }

    /// A gap in the sequence is loss between the two ends, not slowness:
    /// what arrived is still evenly spaced.
    #[test]
    fn a_gap_in_the_sequence_counts_as_skipped_ticks() {
        let ticks = vec![
            SequencedTick {
                sequence_number: 10,
                emitted_at_monotonic_ns: 0,
            },
            SequencedTick {
                sequence_number: 13,
                emitted_at_monotonic_ns: 40 * 1_000_000,
            },
            SequencedTick {
                sequence_number: 14,
                emitted_at_monotonic_ns: 80 * 1_000_000,
            },
        ];

        let cadence = observed_cadence_of(&ticks).expect("three ticks are enough for an interval");

        assert_eq!(cadence.skipped_tick_count, 2);
        assert_eq!(cadence.tick_count, 3);
    }

    #[test]
    fn a_window_too_short_to_hold_an_interval_reports_nothing() {
        assert!(observed_cadence_of(&[]).is_none());
        assert!(observed_cadence_of(&evenly_spaced_ticks(1, 40)).is_none());
    }

    /// The knob the README invites a reader to turn. Set below two it would
    /// otherwise run forever reporting nothing, with no diagnostic at all.
    #[test]
    fn a_window_size_that_could_never_report_is_refused_at_setup() {
        for unreportable in [0, 1] {
            let refusal = refuse_a_window_too_short_to_hold_an_interval(unreportable)
                .expect_err("a window this short can never report");
            assert!(
                refusal.to_string().contains("ticks_per_cadence_report"),
                "the refusal should name the knob, got {refusal}"
            );
        }

        assert!(refuse_a_window_too_short_to_hold_an_interval(2).is_ok());
        assert!(
            refuse_a_window_too_short_to_hold_an_interval(
                TickCadenceReportingSinkConfig::default().ticks_per_cadence_report
            )
            .is_ok(),
            "the default must not be a size the sink refuses"
        );
    }
}
