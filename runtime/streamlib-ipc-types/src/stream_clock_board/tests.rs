// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn the_stream_clock_board_key_is_one_u32_under_its_pinned_name() {
    assert_eq!(std::mem::size_of::<StreamClockBoardKey>(), 4);
    assert_eq!(std::mem::align_of::<StreamClockBoardKey>(), 4);
    assert_eq!(std::mem::offset_of!(StreamClockBoardKey, entry_index), 0);
    assert_eq!(
        // SAFETY: reads the pinned name; the layout it vouches for is asserted above.
        unsafe { <StreamClockBoardKey as ZeroCopySend>::type_name() },
        "StreamClockBoardKey"
    );
}

#[test]
fn a_stream_clock_board_entry_is_two_u32s_and_six_eight_byte_fields_under_its_pinned_name() {
    assert_eq!(std::mem::size_of::<StreamClockBoardEntry>(), 56);
    assert_eq!(std::mem::align_of::<StreamClockBoardEntry>(), 8);
    assert_eq!(std::mem::offset_of!(StreamClockBoardEntry, mode), 0);
    assert_eq!(
        std::mem::offset_of!(StreamClockBoardEntry, a_step_is_open),
        4
    );
    assert_eq!(std::mem::offset_of!(StreamClockBoardEntry, step_index), 8);
    assert_eq!(std::mem::offset_of!(StreamClockBoardEntry, step_ns), 16);
    assert_eq!(std::mem::offset_of!(StreamClockBoardEntry, base_ns), 24);
    assert_eq!(std::mem::offset_of!(StreamClockBoardEntry, anchor_ns), 32);
    assert_eq!(
        std::mem::offset_of!(StreamClockBoardEntry, scale_f64_bits),
        40
    );
    assert_eq!(
        std::mem::offset_of!(StreamClockBoardEntry, pace_f64_bits),
        48
    );
    assert_eq!(
        // SAFETY: reads the pinned name; the layout it vouches for is asserted above.
        unsafe { <StreamClockBoardEntry as ZeroCopySend>::type_name() },
        "StreamClockBoardEntry"
    );
}
