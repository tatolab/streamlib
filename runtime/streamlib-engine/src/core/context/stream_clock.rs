// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The clock one stream runs on, and the spec a graph names it by.

use parking_lot::RwLock;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use streamlib_ipc_types::{
    STREAM_CLOCK_BOARD_MODE_REAL, STREAM_CLOCK_BOARD_MODE_SCALED, STREAM_CLOCK_BOARD_MODE_STEPPED,
    StreamClockBoardEntry,
};

use crate::core::media_clock::MediaClock;
use crate::core::{Error, Result};
use crate::iceoryx2::StreamClockBoard;

/// Stream nanoseconds per step a scaled clock takes when its spec names none:
/// one frame at 60 Hz.
pub const DEFAULT_SCALED_STREAM_CLOCK_STEP_NS: u64 = 16_666_667;

fn default_scaled_stream_clock_step_ns() -> u64 {
    DEFAULT_SCALED_STREAM_CLOCK_STEP_NS
}

/// The clock a stream's spec names: the graph document's `clock` key.
#[derive(
    Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum StreamClockSpec {
    /// The machine's monotonic clock.
    #[default]
    Real,
    /// `base + scale × elapsed` on the machine's monotonic clock.
    Scaled {
        /// Stream seconds per machine second; greater than zero.
        scale: f64,
        /// Stream nanoseconds per step when the scale runs behind the step barrier.
        #[serde(default = "default_scaled_stream_clock_step_ns")]
        step_ns: u64,
    },
    /// Advances `step_ns` at a time, once every node finished the step.
    Stepped {
        /// Stream nanoseconds per step; greater than zero.
        step_ns: u64,
        /// The multiple of real time steps are paced to; unpaced when absent.
        #[serde(default)]
        pace: Option<f64>,
        /// The step after which the stream stops itself.
        #[serde(default)]
        stop_after_steps: Option<u64>,
    },
}

impl StreamClockSpec {
    /// Whether this is the machine's own clock.
    pub fn is_real(&self) -> bool {
        matches!(self, Self::Real)
    }

    /// The mode's name, as the graph document spells it.
    pub fn mode_name(&self) -> &'static str {
        match self {
            Self::Real => "real",
            Self::Scaled { .. } => "scaled",
            Self::Stepped { .. } => "stepped",
        }
    }

    /// Refuse a spec with a value no clock can run on, or a mode this runtime
    /// does not run yet, naming `stream_id`.
    pub fn refusal_unless_this_runtime_runs_it(&self, stream_id: &str) -> Result<()> {
        match *self {
            Self::Real => Ok(()),
            Self::Scaled { scale, step_ns } => {
                if !(scale.is_finite() && scale > 0.0) {
                    return Err(Error::GraphError(format!(
                        "the stream `{stream_id}` names a scaled clock with scale {scale}; a \
                         scale is a finite number greater than zero"
                    )));
                }
                if step_ns == 0 {
                    return Err(Error::GraphError(format!(
                        "the stream `{stream_id}` names a scaled clock with a step of 0 ns; a \
                         step is greater than zero"
                    )));
                }
                if scale > 1.0 {
                    return Err(Error::GraphError(format!(
                        "the stream `{stream_id}` names a scaled clock at {scale}×; faster than \
                         real time runs behind the step barrier, which this runtime does not \
                         run yet. Use a scale of at most 1"
                    )));
                }
                Ok(())
            }
            Self::Stepped { .. } => Err(Error::GraphError(format!(
                "the stream `{stream_id}` names a stepped clock, which this runtime does not run \
                 yet; use a real or scaled clock"
            ))),
        }
    }
}

/// Which clock a stream runs on and where it stands, as a node reads it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StreamClockReading {
    /// `real`, `scaled` or `stepped`.
    pub mode: &'static str,
    /// Stream seconds per machine second; 1.0 when real.
    pub scale: f64,
    /// Stream nanoseconds per step, when the clock steps.
    pub step_ns: Option<u64>,
    /// The step open now, when the clock steps.
    pub step_index: Option<u64>,
}

/// Stream time on a published clock, at machine monotonic time `machine_now_ns`.
///
/// The one arithmetic the engine and every helper share, so a stamp taken in
/// either process lands on the same stream time.
pub fn stream_time_ns_on_a_published_clock(
    published: &StreamClockBoardEntry,
    machine_now_ns: i64,
) -> i64 {
    match published.mode {
        STREAM_CLOCK_BOARD_MODE_SCALED => {
            let scale = f64::from_bits(published.scale_f64_bits);
            let machine_elapsed_ns = machine_now_ns.saturating_sub(published.anchor_ns);
            published
                .base_ns
                .saturating_add((machine_elapsed_ns as f64 * scale) as i64)
        }
        STREAM_CLOCK_BOARD_MODE_STEPPED => published.base_ns.saturating_add(
            published
                .step_index
                .saturating_mul(published.step_ns)
                .min(i64::MAX as u64) as i64,
        ),
        _ => machine_now_ns,
    }
}

/// The machine monotonic time at which `published` reaches stream time
/// `stream_deadline_ns`; `machine_now_ns` plus the stream time still to go when
/// the clock is not scaled.
pub fn machine_time_ns_a_published_clock_reaches(
    published: &StreamClockBoardEntry,
    stream_deadline_ns: i64,
    machine_now_ns: i64,
) -> i64 {
    match published.mode {
        STREAM_CLOCK_BOARD_MODE_SCALED => {
            let scale = f64::from_bits(published.scale_f64_bits);
            let stream_ns_from_base = stream_deadline_ns.saturating_sub(published.base_ns);
            published
                .anchor_ns
                .saturating_add((stream_ns_from_base as f64 / scale).ceil() as i64)
        }
        _ => machine_now_ns.saturating_add(stream_deadline_ns.saturating_sub(
            stream_time_ns_on_a_published_clock(published, machine_now_ns),
        )),
    }
}

/// The clock one loaded stream runs on, built at its start.
///
/// Stream time is in the machine's monotonic epoch, derived from
/// [`MediaClock`] readings and step counts alone. In a real-time stream it is
/// [`MediaClock::now`] itself.
pub struct StreamClock {
    spec: RwLock<StreamClockSpec>,
    runs_on_the_machine_clock: bool,
    published: RwLock<StreamClockBoardEntry>,
    board_read_by_helpers: Option<StreamClockBoard>,
}

impl std::fmt::Debug for StreamClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamClock")
            .field("spec", &*self.spec.read())
            .field("published", &*self.published.read())
            .field(
                "board_read_by_helpers",
                &self
                    .board_read_by_helpers
                    .as_ref()
                    .map(StreamClockBoard::service_name),
            )
            .finish()
    }
}

impl StreamClock {
    /// The clock `spec` names, started now: stream time begins at the machine's
    /// monotonic now.
    pub fn starting_now(spec: StreamClockSpec) -> Self {
        Self::starting_at(spec, machine_monotonic_now_ns())
    }

    /// The clock `spec` names, started at machine monotonic `machine_now_ns`.
    pub fn starting_at(spec: StreamClockSpec, machine_now_ns: i64) -> Self {
        let published = match spec {
            StreamClockSpec::Real => StreamClockBoardEntry {
                mode: STREAM_CLOCK_BOARD_MODE_REAL,
                scale_f64_bits: 1.0f64.to_bits(),
                base_ns: machine_now_ns,
                anchor_ns: machine_now_ns,
                ..StreamClockBoardEntry::default()
            },
            StreamClockSpec::Scaled { scale, step_ns } => StreamClockBoardEntry {
                mode: STREAM_CLOCK_BOARD_MODE_SCALED,
                step_ns,
                base_ns: machine_now_ns,
                anchor_ns: machine_now_ns,
                scale_f64_bits: scale.to_bits(),
                ..StreamClockBoardEntry::default()
            },
            StreamClockSpec::Stepped { step_ns, pace, .. } => StreamClockBoardEntry {
                mode: STREAM_CLOCK_BOARD_MODE_STEPPED,
                step_ns,
                base_ns: machine_now_ns,
                anchor_ns: machine_now_ns,
                scale_f64_bits: 1.0f64.to_bits(),
                pace_f64_bits: pace.map_or(0, f64::to_bits),
                ..StreamClockBoardEntry::default()
            },
        };
        Self {
            spec: RwLock::new(spec),
            runs_on_the_machine_clock: spec.is_real(),
            published: RwLock::new(published),
            board_read_by_helpers: None,
        }
    }

    /// This clock, published on `board` for the stream's helpers to read.
    pub fn published_on(self, board: StreamClockBoard) -> Self {
        board.publish(*self.published.read());
        Self {
            board_read_by_helpers: Some(board),
            ..self
        }
    }

    /// The service name of the board the stream's helpers read this clock off;
    /// `None` for a real-time stream, whose helpers read the machine clock.
    pub fn board_service_name_for_helpers(&self) -> Option<&str> {
        self.board_read_by_helpers
            .as_ref()
            .map(StreamClockBoard::service_name)
    }

    /// The spec this clock runs.
    pub fn spec(&self) -> StreamClockSpec {
        *self.spec.read()
    }

    /// Run a scaled clock at `scale` from now on, publishing it to the
    /// stream's helpers; stream time carries on from where it stands.
    ///
    /// Refuses a real or stepped clock and a scale this runtime does not run,
    /// naming `stream_id`.
    pub fn rescale(&self, stream_id: &str, scale: f64) -> Result<StreamClockReading> {
        self.rescale_at(stream_id, scale, machine_monotonic_now_ns())
    }

    /// [`Self::rescale`] at machine monotonic `machine_now_ns`.
    pub fn rescale_at(
        &self,
        stream_id: &str,
        scale: f64,
        machine_now_ns: i64,
    ) -> Result<StreamClockReading> {
        let mut spec = self.spec.write();
        let StreamClockSpec::Scaled { step_ns, .. } = *spec else {
            return Err(Error::GraphError(format!(
                "the stream `{stream_id}` runs on a {} clock; only a scaled clock's scale \
                 changes",
                spec.mode_name()
            )));
        };
        let rescaled_spec = StreamClockSpec::Scaled { scale, step_ns };
        rescaled_spec.refusal_unless_this_runtime_runs_it(stream_id)?;
        let mut published = self.published.write();
        let rescaled = StreamClockBoardEntry {
            base_ns: stream_time_ns_on_a_published_clock(&published, machine_now_ns),
            anchor_ns: machine_now_ns,
            scale_f64_bits: scale.to_bits(),
            ..*published
        };
        if let Some(board) = &self.board_read_by_helpers {
            board.publish(rescaled);
        }
        *published = rescaled;
        *spec = rescaled_spec;
        Ok(stream_clock_reading_of_a_published_clock(&rescaled))
    }

    /// The machine monotonic time at which this clock reaches stream time
    /// `stream_deadline_ns`, as it runs now; at or before `machine_now_ns`
    /// when it already has.
    pub fn machine_time_ns_reaching(&self, stream_deadline_ns: i64, machine_now_ns: i64) -> i64 {
        machine_time_ns_a_published_clock_reaches(
            &self.published.read(),
            stream_deadline_ns,
            machine_now_ns,
        )
    }

    /// Stream time now, in nanoseconds.
    #[inline]
    pub fn now_ns(&self) -> i64 {
        if self.runs_on_the_machine_clock {
            return machine_monotonic_now_ns();
        }
        stream_time_ns_on_a_published_clock(&self.published.read(), machine_monotonic_now_ns())
    }

    /// Stream time at machine monotonic `machine_now_ns`.
    pub fn stream_time_ns_at(&self, machine_now_ns: i64) -> i64 {
        stream_time_ns_on_a_published_clock(&self.published.read(), machine_now_ns)
    }

    /// Which clock this is and where it stands.
    pub fn reading(&self) -> StreamClockReading {
        stream_clock_reading_of_a_published_clock(&self.published.read())
    }

    /// The clock as its board carries it to the stream's helpers.
    pub fn published(&self) -> StreamClockBoardEntry {
        *self.published.read()
    }
}

/// Which clock `published` is and where it stands.
pub fn stream_clock_reading_of_a_published_clock(
    published: &StreamClockBoardEntry,
) -> StreamClockReading {
    match published.mode {
        STREAM_CLOCK_BOARD_MODE_SCALED => StreamClockReading {
            mode: "scaled",
            scale: f64::from_bits(published.scale_f64_bits),
            step_ns: None,
            step_index: None,
        },
        STREAM_CLOCK_BOARD_MODE_STEPPED => StreamClockReading {
            mode: "stepped",
            scale: 1.0,
            step_ns: Some(published.step_ns),
            step_index: Some(published.step_index),
        },
        _ => StreamClockReading {
            mode: "real",
            scale: 1.0,
            step_ns: None,
            step_index: None,
        },
    }
}

fn machine_monotonic_now_ns() -> i64 {
    MediaClock::now().as_nanos() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_clock_reads_the_machine_clock_itself() {
        let clock = StreamClock::starting_now(StreamClockSpec::Real);
        let before = machine_monotonic_now_ns();
        let stream_now = clock.now_ns();
        let after = machine_monotonic_now_ns();
        assert!(before <= stream_now && stream_now <= after);
        assert_eq!(clock.reading().mode, "real");
        assert_eq!(clock.reading().scale, 1.0);
    }

    #[test]
    fn a_scaled_clock_starts_at_the_machine_clock_and_runs_at_its_scale() {
        let started_at = 1_000_000_000;
        let clock = StreamClock::starting_at(
            StreamClockSpec::Scaled {
                scale: 0.5,
                step_ns: DEFAULT_SCALED_STREAM_CLOCK_STEP_NS,
            },
            started_at,
        );
        assert_eq!(clock.stream_time_ns_at(started_at), started_at);
        assert_eq!(
            clock.stream_time_ns_at(started_at + 2_000_000),
            started_at + 1_000_000
        );
        assert_eq!(clock.reading().mode, "scaled");
        assert_eq!(clock.reading().scale, 0.5);
    }

    #[test]
    fn a_published_clock_reads_the_same_in_any_process() {
        let started_at = 5_000;
        let clock = StreamClock::starting_at(
            StreamClockSpec::Scaled {
                scale: 0.25,
                step_ns: 10,
            },
            started_at,
        );
        let published = clock.published();
        assert_eq!(
            stream_time_ns_on_a_published_clock(&published, started_at + 4_000),
            clock.stream_time_ns_at(started_at + 4_000)
        );
        assert_eq!(
            stream_clock_reading_of_a_published_clock(&published),
            clock.reading()
        );
    }

    #[test]
    fn a_stepped_clock_reads_its_open_steps_time() {
        let mut published = StreamClock::starting_at(
            StreamClockSpec::Stepped {
                step_ns: 100,
                pace: None,
                stop_after_steps: None,
            },
            7_000,
        )
        .published();
        published.step_index = 3;
        assert_eq!(
            stream_time_ns_on_a_published_clock(&published, i64::MAX),
            7_300
        );
    }

    #[test]
    fn the_spec_reads_and_writes_the_graph_documents_clock_key() {
        let read = |json: serde_json::Value| serde_json::from_value::<StreamClockSpec>(json);
        assert_eq!(
            read(serde_json::json!({"mode": "real"})).unwrap(),
            StreamClockSpec::Real
        );
        assert_eq!(
            read(serde_json::json!({"mode": "scaled", "scale": 0.5})).unwrap(),
            StreamClockSpec::Scaled {
                scale: 0.5,
                step_ns: DEFAULT_SCALED_STREAM_CLOCK_STEP_NS
            }
        );
        assert_eq!(
            read(serde_json::json!({"mode": "stepped", "step_ns": 10})).unwrap(),
            StreamClockSpec::Stepped {
                step_ns: 10,
                pace: None,
                stop_after_steps: None
            }
        );
        assert!(read(serde_json::json!({"mode": "scaled", "scale": 0.5, "speed": 2})).is_err());
        assert!(read(serde_json::json!({"mode": "warped"})).is_err());
        assert_eq!(
            serde_json::to_value(StreamClockSpec::Scaled {
                scale: 0.5,
                step_ns: 20
            })
            .unwrap(),
            serde_json::json!({"mode": "scaled", "scale": 0.5, "step_ns": 20})
        );
    }

    #[test]
    fn a_spec_this_runtime_cannot_run_is_refused_naming_the_stream() {
        let refusal = |spec: StreamClockSpec| {
            spec.refusal_unless_this_runtime_runs_it("rig")
                .unwrap_err()
                .to_string()
        };
        assert!(
            refusal(StreamClockSpec::Scaled {
                scale: 0.0,
                step_ns: 1
            })
            .contains("`rig`")
        );
        assert!(
            refusal(StreamClockSpec::Scaled {
                scale: f64::NAN,
                step_ns: 1
            })
            .contains("finite")
        );
        assert!(
            refusal(StreamClockSpec::Scaled {
                scale: 2.0,
                step_ns: 1
            })
            .contains("step barrier")
        );
        assert!(
            refusal(StreamClockSpec::Stepped {
                step_ns: 1,
                pace: None,
                stop_after_steps: None
            })
            .contains("stepped")
        );
        StreamClockSpec::Real
            .refusal_unless_this_runtime_runs_it("rig")
            .unwrap();
        StreamClockSpec::Scaled {
            scale: 1.0,
            step_ns: 1,
        }
        .refusal_unless_this_runtime_runs_it("rig")
        .unwrap();
    }

    fn a_half_speed_clock_started_at(machine_now_ns: i64) -> StreamClock {
        StreamClock::starting_at(
            StreamClockSpec::Scaled {
                scale: 0.5,
                step_ns: DEFAULT_SCALED_STREAM_CLOCK_STEP_NS,
            },
            machine_now_ns,
        )
    }

    #[test]
    fn a_rescale_carries_stream_time_on_from_where_it_stands() {
        let clock = a_half_speed_clock_started_at(1_000);
        clock
            .rescale_at("warped", 0.25, 5_000)
            .expect("a scaled clock rescales");

        assert_eq!(clock.stream_time_ns_at(5_000), 3_000);
        assert_eq!(clock.stream_time_ns_at(9_000), 4_000);
        assert_eq!(clock.reading().scale, 0.25);
        assert_eq!(
            clock.spec(),
            StreamClockSpec::Scaled {
                scale: 0.25,
                step_ns: DEFAULT_SCALED_STREAM_CLOCK_STEP_NS
            }
        );
    }

    #[test]
    fn a_rescale_is_refused_on_a_clock_that_does_not_scale_or_past_what_runs() {
        let real = StreamClock::starting_at(StreamClockSpec::Real, 0);
        let refusal = real.rescale_at("on-the-wall", 0.5, 10).unwrap_err();
        assert!(
            refusal
                .to_string()
                .contains("`on-the-wall` runs on a real clock")
        );

        let scaled = a_half_speed_clock_started_at(0);
        for refused_scale in [0.0, -1.0, f64::NAN, 2.0] {
            assert!(scaled.rescale_at("warped", refused_scale, 10).is_err());
        }
        assert_eq!(scaled.reading().scale, 0.5);
        assert_eq!(scaled.stream_time_ns_at(10), 5);
    }

    #[test]
    fn a_stream_deadline_converts_to_the_machine_time_the_clock_reaches_it() {
        let clock = a_half_speed_clock_started_at(1_000);
        assert_eq!(clock.machine_time_ns_reaching(1_500, 1_000), 2_000);
        clock
            .rescale_at("warped", 0.25, 2_000)
            .expect("a scaled clock rescales");
        assert_eq!(clock.machine_time_ns_reaching(1_750, 2_000), 3_000);

        let real = StreamClock::starting_at(StreamClockSpec::Real, 0);
        assert_eq!(real.machine_time_ns_reaching(700, 500), 700);
    }
}
