// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The pixel pattern the IOSurface texture test's engine writes and its
//! helper verifies, shared so the two sides cannot drift apart.

/// The byte the engine writes at `index` of the texture's packed rows.
pub fn engine_pattern_byte(index: usize) -> u8 {
    (index.wrapping_mul(29).wrapping_add(3)) as u8
}
