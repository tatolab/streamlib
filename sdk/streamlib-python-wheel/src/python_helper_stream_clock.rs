// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The clock of the stream this helper's node runs in, as `ctx.time`, the
//! implicit write stamp and `ctx.stream_clock` read it.
//!
//! A helper process hosts one node of one stream, so the clock it adopts at
//! `setup` is the process's own. Before it adopts one, and in a real-time
//! stream, stream time is the machine's monotonic clock.

use std::sync::OnceLock;

use pyo3::prelude::*;
use streamlib::sdk::context::{
    StreamClockReading, machine_time_ns_a_published_clock_reaches,
    stream_clock_reading_of_a_published_clock, stream_time_ns_on_a_published_clock,
};
use streamlib::sdk::execution::processor_scheduling_loop::ClockAProcessorSchedulingLoopPacesOn;
use streamlib::sdk::iceoryx2::StreamClockBoardReader;

use crate::python_logging::monotonic_clock_now_ns;

/// The board this helper reads its stream's clock off; unset in a real-time
/// stream.
static STREAM_CLOCK_BOARD_THIS_HELPER_READS: OnceLock<StreamClockBoardReader> = OnceLock::new();

/// Read this helper's stream clock off `board` from now on.
pub(crate) fn adopt_the_stream_clock_board(board: StreamClockBoardReader) -> Result<(), String> {
    STREAM_CLOCK_BOARD_THIS_HELPER_READS
        .set(board)
        .map_err(|board| {
            format!(
                "this helper already reads its stream's clock off a board, so the board {:?} \
                 cannot be adopted as a second",
                board.service_name()
            )
        })
}

/// Stream time now, in nanoseconds.
pub(crate) fn helper_stream_clock_now_ns() -> i64 {
    helper_stream_time_ns_of_a_machine_instant(monotonic_clock_now_ns() as i64)
}

/// Stream time at `machine_instant_ns`, a reading of the machine's monotonic
/// clock, as the clock runs now.
pub(crate) fn helper_stream_time_ns_of_a_machine_instant(machine_instant_ns: i64) -> i64 {
    match STREAM_CLOCK_BOARD_THIS_HELPER_READS.get() {
        Some(board) => stream_time_ns_on_a_published_clock(&board.read(), machine_instant_ns),
        None => machine_instant_ns,
    }
}

/// Machine nanoseconds until this helper's stream clock reaches stream time
/// `stream_deadline_ns`, as the clock runs now; zero or less once it has.
fn machine_nanoseconds_until_this_helpers_stream_clock_reaches(stream_deadline_ns: i64) -> i64 {
    let machine_now_ns = monotonic_clock_now_ns() as i64;
    let machine_time_ns_reaching_the_deadline = match STREAM_CLOCK_BOARD_THIS_HELPER_READS.get() {
        Some(board) => machine_time_ns_a_published_clock_reaches(
            &board.read(),
            stream_deadline_ns,
            machine_now_ns,
        ),
        None => stream_deadline_ns,
    };
    machine_time_ns_reaching_the_deadline.saturating_sub(machine_now_ns)
}

/// This helper's stream clock, as its scheduling loop paces on it.
pub(crate) struct HelperProcessStreamClock;

impl ClockAProcessorSchedulingLoopPacesOn for HelperProcessStreamClock {
    fn now_ns(&self) -> i64 {
        helper_stream_clock_now_ns()
    }

    fn machine_ns_until(&self, deadline_ns: i64) -> i64 {
        machine_nanoseconds_until_this_helpers_stream_clock_reaches(deadline_ns)
    }

    fn runs_behind_the_step_barrier(&self) -> bool {
        helper_stream_clock_reading().step_ns.is_some()
    }
}

/// The step this helper's stream clock has open; `None` when it runs free of
/// the step barrier.
pub(crate) fn helper_stream_clock_open_step_index() -> Option<u64> {
    helper_stream_clock_reading().step_index
}

/// Which clock this helper's stream runs on and where it stands.
fn helper_stream_clock_reading() -> StreamClockReading {
    match STREAM_CLOCK_BOARD_THIS_HELPER_READS.get() {
        Some(board) => stream_clock_reading_of_a_published_clock(&board.read()),
        None => StreamClockReading {
            mode: "real",
            scale: 1.0,
            step_ns: None,
            step_index: None,
        },
    }
}

/// Which clock the node's stream runs on and where it stands, read when asked.
#[pyclass(name = "StreamClockReading", module = "tatolab.stream", frozen)]
pub(crate) struct PythonStreamClockReading {
    reading: StreamClockReading,
}

impl PythonStreamClockReading {
    /// The helper's stream clock as it stands now.
    pub(crate) fn now() -> Self {
        Self {
            reading: helper_stream_clock_reading(),
        }
    }
}

#[pymethods]
impl PythonStreamClockReading {
    /// `"real"`, `"scaled"` or `"stepped"`.
    #[getter]
    fn mode(&self) -> &'static str {
        self.reading.mode
    }

    /// Stream seconds per machine second; 1.0 when real.
    #[getter]
    fn scale(&self) -> f64 {
        self.reading.scale
    }

    /// Stream nanoseconds per step, or `None` when the clock does not step.
    #[getter]
    fn step_ns(&self) -> Option<u64> {
        self.reading.step_ns
    }

    /// The step open now, or `None` when the clock does not step.
    #[getter]
    fn step_index(&self) -> Option<u64> {
        self.reading.step_index
    }
}
