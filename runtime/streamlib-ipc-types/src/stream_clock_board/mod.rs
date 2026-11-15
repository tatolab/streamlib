// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use iceoryx2::prelude::*;

/// The key of the one entry on a stream's clock blackboard.
///
/// Pinned under its own iceoryx2 type name: the engine creates the board and
/// every helper of the stream opens it, and both must present the same key type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, ZeroCopySend)]
#[type_name("StreamClockBoardKey")]
#[repr(C)]
pub struct StreamClockBoardKey {
    entry_index: u32,
}

impl StreamClockBoardKey {
    /// The key of the board's one entry, the stream clock.
    pub const STREAM_CLOCK: Self = Self { entry_index: 0 };
}

/// [`StreamClockBoardEntry::mode`] for a stream on the machine's monotonic clock.
pub const STREAM_CLOCK_BOARD_MODE_REAL: u32 = 0;

/// [`StreamClockBoardEntry::mode`] for a stream whose time runs at a scale of
/// the machine's monotonic clock.
pub const STREAM_CLOCK_BOARD_MODE_SCALED: u32 = 1;

/// [`StreamClockBoardEntry::mode`] for a stream whose time advances in steps.
pub const STREAM_CLOCK_BOARD_MODE_STEPPED: u32 = 2;

/// A stream's clock as its engine last wrote it, copied whole by every reader.
///
/// Stream time is `base_ns + scale × (machine monotonic now − anchor_ns)` when
/// scaled, and `base_ns + step_index × step_ns` when stepped; both are in the
/// machine's monotonic epoch. The two `f64` fields travel as their IEEE-754 bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ZeroCopySend)]
#[type_name("StreamClockBoardEntry")]
#[repr(C)]
pub struct StreamClockBoardEntry {
    /// One of the `STREAM_CLOCK_BOARD_MODE_*` values.
    pub mode: u32,
    /// One once a stepped clock opened its first step, zero before and unless stepped.
    pub a_step_is_open: u32,
    /// The step open now; zero unless a step is open.
    pub step_index: u64,
    /// Stream nanoseconds per step; zero unless the clock steps.
    pub step_ns: u64,
    /// Stream time at the anchor, in nanoseconds.
    pub base_ns: i64,
    /// The machine monotonic reading `base_ns` was taken at, in nanoseconds.
    pub anchor_ns: i64,
    /// Stream seconds per machine second, as `f64::to_bits`.
    pub scale_f64_bits: u64,
    /// Steps paced to this multiple of real time, as `f64::to_bits`; zero when unpaced.
    pub pace_f64_bits: u64,
}

#[cfg(test)]
mod tests;
