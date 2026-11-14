// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The clock one stream runs on, and the spec a graph names it by.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::{Condvar, Mutex, RwLock};
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
                Ok(())
            }
            Self::Stepped {
                step_ns,
                pace,
                stop_after_steps,
            } => {
                if step_ns == 0 {
                    return Err(Error::GraphError(format!(
                        "the stream `{stream_id}` names a stepped clock with a step of 0 ns; a \
                         step is greater than zero"
                    )));
                }
                if let Some(pace) = pace
                    && !(pace.is_finite() && pace > 0.0)
                {
                    return Err(Error::GraphError(format!(
                        "the stream `{stream_id}` names a stepped clock paced at {pace}; a pace \
                         is a finite number greater than zero, or absent to run unpaced"
                    )));
                }
                if stop_after_steps == Some(0) {
                    return Err(Error::GraphError(format!(
                        "the stream `{stream_id}` names a stepped clock that stops after 0 steps; \
                         a stream runs at least one step"
                    )));
                }
                Ok(())
            }
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

/// Whether a published clock advances one step at a time behind the step
/// barrier: a stepped clock, or a scaled one faster than real time.
pub fn a_published_clock_runs_behind_the_step_barrier(published: &StreamClockBoardEntry) -> bool {
    match published.mode {
        STREAM_CLOCK_BOARD_MODE_STEPPED => true,
        STREAM_CLOCK_BOARD_MODE_SCALED => f64::from_bits(published.scale_f64_bits) > 1.0,
        _ => false,
    }
}

/// Stream time on a published clock, at machine monotonic time `machine_now_ns`.
///
/// The one arithmetic the engine and every helper share, so a stamp taken in
/// either process lands on the same stream time.
pub fn stream_time_ns_on_a_published_clock(
    published: &StreamClockBoardEntry,
    machine_now_ns: i64,
) -> i64 {
    if a_published_clock_runs_behind_the_step_barrier(published) {
        return published
            .base_ns
            .saturating_add(stream_ns_of_steps(published.step_index, published.step_ns));
    }
    match published.mode {
        STREAM_CLOCK_BOARD_MODE_SCALED => {
            let scale = f64::from_bits(published.scale_f64_bits);
            let machine_elapsed_ns = machine_now_ns.saturating_sub(published.anchor_ns);
            published
                .base_ns
                .saturating_add((machine_elapsed_ns as f64 * scale) as i64)
        }
        _ => machine_now_ns,
    }
}

/// The multiple of real time a published clock's steps open at; `None` when
/// they open as fast as the barrier closes them, or the clock does not step.
fn pace_of_a_published_clocks_steps(published: &StreamClockBoardEntry) -> Option<f64> {
    match published.mode {
        STREAM_CLOCK_BOARD_MODE_STEPPED if published.pace_f64_bits != 0 => {
            Some(f64::from_bits(published.pace_f64_bits))
        }
        STREAM_CLOCK_BOARD_MODE_SCALED
            if a_published_clock_runs_behind_the_step_barrier(published) =>
        {
            Some(f64::from_bits(published.scale_f64_bits))
        }
        _ => None,
    }
}

fn stream_ns_of_steps(step_count: u64, step_ns: u64) -> i64 {
    step_count.saturating_mul(step_ns).min(i64::MAX as u64) as i64
}

fn machine_ns_of_steps_paced_at(step_count: u64, step_ns: u64, pace: f64) -> i64 {
    ((step_count as f64) * (step_ns as f64) / pace) as i64
}

/// The anchor that paces step `step_index` to open at `machine_now_ns`.
fn anchor_ns_pacing_the_step_to_open_at(
    machine_now_ns: i64,
    step_index: u64,
    step_ns: u64,
    pace: f64,
) -> i64 {
    machine_now_ns.saturating_sub(machine_ns_of_steps_paced_at(step_index, step_ns, pace))
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
    /// Mirrors [`a_published_clock_runs_behind_the_step_barrier`] on
    /// `published`, read without its lock.
    runs_behind_the_step_barrier: AtomicBool,
    /// Whether a pause holds this clock where it stands: a free-running clock
    /// publishes no advance and a clock behind the step barrier opens no step.
    held_by_pause: AtomicBool,
    step_barrier_position: Mutex<StepBarrierPosition>,
    a_step_opened: Condvar,
    /// How many times a node noted progress on its step, so the step
    /// coordinator wakes for it.
    step_progress_notes: Mutex<u64>,
    a_node_noted_step_progress: Condvar,
}

/// Where a clock stands behind the step barrier, as its runners wait on it.
#[derive(Debug, Clone, Copy, Default)]
struct StepBarrierPosition {
    /// How many steps have opened; the next to open is this one.
    steps_opened: u64,
    /// The step open now; `None` before one opens and while the clock runs
    /// free of the barrier.
    step_open_now: Option<u64>,
}

impl StepBarrierPosition {
    fn holds_a_step_opened_after(&self, after: Option<u64>) -> Option<u64> {
        self.step_open_now
            .filter(|step_open_now| after.is_none_or(|after| *step_open_now > after))
    }
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
            runs_behind_the_step_barrier: AtomicBool::new(
                a_published_clock_runs_behind_the_step_barrier(&published),
            ),
            held_by_pause: AtomicBool::new(false),
            published: RwLock::new(published),
            board_read_by_helpers: None,
            step_barrier_position: Mutex::new(StepBarrierPosition::default()),
            a_step_opened: Condvar::new(),
            step_progress_notes: Mutex::new(0),
            a_node_noted_step_progress: Condvar::new(),
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
    /// Crossing 1× moves the clock behind the step barrier or out of it: above
    /// 1× it runs in steps of its `step_ns`, paced at the scale, and the next
    /// step waits for the stream's step coordinator to open it.
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
        self.rescale_unless_a_node_holds_it_under_real_time(
            stream_id,
            scale,
            machine_now_ns,
            || None,
        )
    }

    /// [`Self::rescale_at`], refused above 1× naming the node that
    /// `a_node_that_paces_its_own_callbacks` names, read while no node is
    /// admitted through [`Self::admit_unless_it_steps`].
    pub fn rescale_unless_a_node_holds_it_under_real_time(
        &self,
        stream_id: &str,
        scale: f64,
        machine_now_ns: i64,
        a_node_that_paces_its_own_callbacks: impl FnOnce() -> Option<String>,
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
        if scale > 1.0
            && let Some(node_that_paces_its_own_callbacks) = a_node_that_paces_its_own_callbacks()
        {
            return Err(Error::GraphError(format!(
                "the stream `{stream_id}` cannot run its clock at {scale}×: above 1× the clock \
                 runs in steps every node runs on, and the node \
                 `{node_that_paces_its_own_callbacks}` paces itself (execution = manual)"
            )));
        }
        let rescaled = {
            let mut published = self.published.write();
            let mut step_barrier_position = self.step_barrier_position.lock();
            let stream_now_ns = stream_time_ns_on_a_published_clock(&published, machine_now_ns);
            let goes_behind_the_step_barrier = scale > 1.0;
            let rescaled = match (
                a_published_clock_runs_behind_the_step_barrier(&published),
                goes_behind_the_step_barrier,
            ) {
                (false, false) => StreamClockBoardEntry {
                    base_ns: stream_now_ns,
                    anchor_ns: machine_now_ns,
                    scale_f64_bits: scale.to_bits(),
                    ..*published
                },
                (false, true) => {
                    let next_step = step_barrier_position.steps_opened;
                    StreamClockBoardEntry {
                        base_ns: stream_now_ns
                            .saturating_sub(stream_ns_of_steps(next_step, step_ns)),
                        anchor_ns: anchor_ns_pacing_the_step_to_open_at(
                            machine_now_ns,
                            next_step,
                            step_ns,
                            scale,
                        ),
                        step_index: next_step,
                        a_step_is_open: 0,
                        scale_f64_bits: scale.to_bits(),
                        ..*published
                    }
                }
                (true, true) => StreamClockBoardEntry {
                    anchor_ns: anchor_ns_pacing_the_step_to_open_at(
                        machine_now_ns,
                        published.step_index,
                        step_ns,
                        scale,
                    ),
                    scale_f64_bits: scale.to_bits(),
                    ..*published
                },
                (true, false) => {
                    step_barrier_position.step_open_now = None;
                    StreamClockBoardEntry {
                        base_ns: stream_now_ns,
                        anchor_ns: machine_now_ns,
                        a_step_is_open: 0,
                        scale_f64_bits: scale.to_bits(),
                        ..*published
                    }
                }
            };
            let rescaled_as_published = if self.is_held_by_pause() && !goes_behind_the_step_barrier
            {
                StreamClockBoardEntry {
                    scale_f64_bits: 0.0f64.to_bits(),
                    ..rescaled
                }
            } else {
                rescaled
            };
            if let Some(board) = &self.board_read_by_helpers {
                board.publish(rescaled_as_published);
            }
            *published = rescaled_as_published;
            self.runs_behind_the_step_barrier
                .store(goes_behind_the_step_barrier, Ordering::Release);
            *spec = rescaled_spec;
            rescaled
        };
        self.a_step_opened.notify_all();
        self.note_step_progress();
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

    /// Which clock this is and where it stands; a held scaled clock reads at
    /// the scale it carries on at.
    pub fn reading(&self) -> StreamClockReading {
        let mut reading = stream_clock_reading_of_a_published_clock(&self.published.read());
        if let StreamClockSpec::Scaled { scale, .. } = self.spec() {
            reading.scale = scale;
        }
        reading
    }

    /// Whether a pause holds this clock where it stands.
    pub fn is_held_by_pause(&self) -> bool {
        self.held_by_pause.load(Ordering::Acquire)
    }

    /// Hold this clock where it stands for a pause: stream time stops, the
    /// stream's helpers read it stopped, and no step opens until
    /// [`Self::carry_on_from_where_the_pause_held_it`]. A real-time clock is
    /// the machine's own and runs on.
    pub fn hold_where_it_stands_for_a_pause(&self) {
        self.hold_where_it_stands_for_a_pause_at(machine_monotonic_now_ns());
    }

    /// [`Self::hold_where_it_stands_for_a_pause`] at machine monotonic `machine_now_ns`.
    pub fn hold_where_it_stands_for_a_pause_at(&self, machine_now_ns: i64) {
        if self.runs_on_the_machine_clock {
            return;
        }
        let _no_rescale_meanwhile = self.spec.read();
        let mut published = self.published.write();
        if self.held_by_pause.swap(true, Ordering::AcqRel) {
            return;
        }
        if a_published_clock_runs_behind_the_step_barrier(&published) {
            return;
        }
        let held = StreamClockBoardEntry {
            base_ns: stream_time_ns_on_a_published_clock(&published, machine_now_ns),
            anchor_ns: machine_now_ns,
            scale_f64_bits: 0.0f64.to_bits(),
            ..*published
        };
        if let Some(board) = &self.board_read_by_helpers {
            board.publish(held);
        }
        *published = held;
    }

    /// Carry this clock on from the stream time a pause held it at: a
    /// free-running clock advances from now at its scale, and the next step
    /// of a paced clock behind the step barrier opens no earlier than its pace
    /// from now.
    pub fn carry_on_from_where_the_pause_held_it(&self) {
        self.carry_on_from_where_the_pause_held_it_at(machine_monotonic_now_ns());
    }

    /// [`Self::carry_on_from_where_the_pause_held_it`] at machine monotonic `machine_now_ns`.
    pub fn carry_on_from_where_the_pause_held_it_at(&self, machine_now_ns: i64) {
        let spec = self.spec.read();
        {
            let mut published = self.published.write();
            if !self.held_by_pause.swap(false, Ordering::AcqRel) {
                return;
            }
            let carried_on = if a_published_clock_runs_behind_the_step_barrier(&published) {
                match pace_of_a_published_clocks_steps(&published) {
                    Some(pace) => {
                        let next_step = if published.a_step_is_open != 0 {
                            published.step_index.saturating_add(1)
                        } else {
                            published.step_index
                        };
                        StreamClockBoardEntry {
                            anchor_ns: anchor_ns_pacing_the_step_to_open_at(
                                machine_now_ns,
                                next_step,
                                published.step_ns,
                                pace,
                            ),
                            ..*published
                        }
                    }
                    None => *published,
                }
            } else {
                let scale = match *spec {
                    StreamClockSpec::Scaled { scale, .. } => scale,
                    _ => 1.0,
                };
                StreamClockBoardEntry {
                    anchor_ns: machine_now_ns,
                    scale_f64_bits: scale.to_bits(),
                    ..*published
                }
            };
            if let Some(board) = &self.board_read_by_helpers {
                board.publish(carried_on);
            }
            *published = carried_on;
        }
        drop(spec);
        self.a_step_opened.notify_all();
        self.note_step_progress();
    }

    /// The clock as its board carries it to the stream's helpers.
    pub fn published(&self) -> StreamClockBoardEntry {
        *self.published.read()
    }

    /// Whether this clock advances one step at a time behind the step
    /// barrier now: a stepped clock, or a scaled one faster than real time.
    pub fn runs_behind_the_step_barrier(&self) -> bool {
        self.runs_behind_the_step_barrier.load(Ordering::Acquire)
    }

    /// Open the step a clock behind the step barrier waits on: stream time
    /// moves to it, the stream's helpers read it off the board, and every
    /// runner waiting on a step wakes. Its steps from here on are paced from
    /// now. `None`, opening nothing, when the clock runs free of the barrier
    /// or a step is open already.
    pub fn open_the_waiting_step(&self) -> Option<u64> {
        self.open_the_waiting_step_at(machine_monotonic_now_ns())
    }

    /// [`Self::open_the_waiting_step`] at machine monotonic `machine_now_ns`.
    pub fn open_the_waiting_step_at(&self, machine_now_ns: i64) -> Option<u64> {
        let opened = {
            let mut published = self.published.write();
            if !a_published_clock_runs_behind_the_step_barrier(&published)
                || published.a_step_is_open != 0
                || self.is_held_by_pause()
            {
                return None;
            }
            let waiting_step = published.step_index;
            if let Some(pace) = pace_of_a_published_clocks_steps(&published) {
                published.anchor_ns = anchor_ns_pacing_the_step_to_open_at(
                    machine_now_ns,
                    waiting_step,
                    published.step_ns,
                    pace,
                );
            }
            published.a_step_is_open = 1;
            self.publish_and_note_the_open_step(&published);
            waiting_step
        };
        self.a_step_opened.notify_all();
        Some(opened)
    }

    /// Open the step after `step_index`, when `step_index` is the step open
    /// behind the barrier now; `false`, opening nothing, otherwise.
    ///
    /// A scaled clock that opens it later than its pace allows paces the
    /// steps after it from now, so it slows while the graph cannot keep up
    /// rather than catching up in a burst; a stepped clock keeps its pace from
    /// its first step.
    pub fn open_the_step_after(&self, step_index: u64) -> bool {
        self.open_the_step_after_at(step_index, machine_monotonic_now_ns())
    }

    /// [`Self::open_the_step_after`] at machine monotonic `machine_now_ns`.
    pub fn open_the_step_after_at(&self, step_index: u64, machine_now_ns: i64) -> bool {
        {
            let mut published = self.published.write();
            if !a_published_clock_runs_behind_the_step_barrier(&published)
                || published.a_step_is_open == 0
                || published.step_index != step_index
                || self.is_held_by_pause()
            {
                return false;
            }
            let next_step = step_index.saturating_add(1);
            if published.mode == STREAM_CLOCK_BOARD_MODE_SCALED {
                let scale = f64::from_bits(published.scale_f64_bits);
                published.anchor_ns =
                    published
                        .anchor_ns
                        .max(anchor_ns_pacing_the_step_to_open_at(
                            machine_now_ns,
                            next_step,
                            published.step_ns,
                            scale,
                        ));
            }
            published.step_index = next_step;
            self.publish_and_note_the_open_step(&published);
        }
        self.a_step_opened.notify_all();
        true
    }

    fn publish_and_note_the_open_step(&self, published: &StreamClockBoardEntry) {
        if let Some(board) = &self.board_read_by_helpers {
            board.publish(*published);
        }
        let mut step_barrier_position = self.step_barrier_position.lock();
        step_barrier_position.steps_opened = published.step_index.saturating_add(1);
        step_barrier_position.step_open_now = Some(published.step_index);
    }

    /// The step open behind the barrier now; `None` while none is, or the
    /// clock runs free of the barrier.
    pub fn step_open_behind_the_barrier(&self) -> Option<u64> {
        let published = self.published.read();
        (a_published_clock_runs_behind_the_step_barrier(&published)
            && published.a_step_is_open != 0)
            .then_some(published.step_index)
    }

    /// The machine monotonic time at which step `step_index` opens, no
    /// earlier, as the clock is paced now; `None` when its steps are unpaced
    /// or it runs free of the barrier.
    pub fn machine_time_ns_the_step_opens(&self, step_index: u64) -> Option<i64> {
        let published = self.published.read();
        let pace = pace_of_a_published_clocks_steps(&published)?;
        Some(
            published
                .anchor_ns
                .saturating_add(machine_ns_of_steps_paced_at(
                    step_index,
                    published.step_ns,
                    pace,
                )),
        )
    }

    /// The step open once one later than `after` is, waiting up to
    /// `longest_wait` for it; `None` when none opened in that time, or the
    /// clock runs free of the barrier. With `after` absent, any open step will
    /// do.
    pub fn wait_for_a_step_opened_after(
        &self,
        after: Option<u64>,
        longest_wait: Duration,
    ) -> Option<u64> {
        let mut step_barrier_position = self.step_barrier_position.lock();
        if step_barrier_position
            .holds_a_step_opened_after(after)
            .is_none()
            && self.runs_behind_the_step_barrier()
        {
            let _ = self.a_step_opened.wait_while_for(
                &mut step_barrier_position,
                |step_barrier_position| {
                    step_barrier_position
                        .holds_a_step_opened_after(after)
                        .is_none()
                        && self.runs_behind_the_step_barrier()
                },
                longest_wait,
            );
        }
        step_barrier_position.holds_a_step_opened_after(after)
    }

    /// Whether this clock runs behind the step barrier, waiting up to
    /// `longest_wait` for a rescale to move it there.
    pub fn wait_until_it_runs_behind_the_step_barrier(&self, longest_wait: Duration) -> bool {
        let mut step_barrier_position = self.step_barrier_position.lock();
        if !self.runs_behind_the_step_barrier() {
            let _ = self.a_step_opened.wait_while_for(
                &mut step_barrier_position,
                |_| !self.runs_behind_the_step_barrier(),
                longest_wait,
            );
        }
        self.runs_behind_the_step_barrier()
    }

    /// Run `admit` unless this clock runs behind the step barrier, so no
    /// rescale crosses above 1× between the check and `admit`; `false` when
    /// it does, admitting nothing.
    pub fn admit_unless_it_steps(&self, admit: impl FnOnce()) -> bool {
        let _no_rescale_meanwhile = self.spec.read();
        if self.runs_behind_the_step_barrier() {
            return false;
        }
        admit();
        true
    }

    /// Wake the step coordinator: a node's callback started or finished, or it
    /// finished a step.
    pub fn note_step_progress(&self) {
        *self.step_progress_notes.lock() += 1;
        self.a_node_noted_step_progress.notify_all();
    }

    /// How many step-progress notes there have been, once there are more than
    /// `notes_seen` or `longest_wait` passed.
    pub fn wait_for_step_progress_after(&self, notes_seen: u64, longest_wait: Duration) -> u64 {
        let mut step_progress_notes = self.step_progress_notes.lock();
        if *step_progress_notes <= notes_seen {
            let _ = self.a_node_noted_step_progress.wait_while_for(
                &mut step_progress_notes,
                |step_progress_notes| *step_progress_notes <= notes_seen,
                longest_wait,
            );
        }
        *step_progress_notes
    }
}

/// Which clock `published` is and where it stands.
pub fn stream_clock_reading_of_a_published_clock(
    published: &StreamClockBoardEntry,
) -> StreamClockReading {
    match published.mode {
        STREAM_CLOCK_BOARD_MODE_SCALED => {
            let steps = a_published_clock_runs_behind_the_step_barrier(published);
            StreamClockReading {
                mode: "scaled",
                scale: f64::from_bits(published.scale_f64_bits),
                step_ns: steps.then_some(published.step_ns),
                step_index: (steps && published.a_step_is_open != 0)
                    .then_some(published.step_index),
            }
        }
        STREAM_CLOCK_BOARD_MODE_STEPPED => StreamClockReading {
            mode: "stepped",
            scale: 1.0,
            step_ns: Some(published.step_ns),
            step_index: (published.a_step_is_open != 0).then_some(published.step_index),
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
mod tests;
