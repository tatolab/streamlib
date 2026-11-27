// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node written as one function whose computed value panics while `panic_in_its_build` is
//! set, so an integration test can confirm a panic in a build is reported on the node and the
//! frames run on the value built before it. It records the level each frame reads; one node of
//! the type runs at a time.

use std::sync::{Mutex, MutexGuard, PoisonError};

use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::error::Result;
use streamlib::sdk::nodes::{Input, computed};

/// The level each frame of the node read, in order, since a test last forgot it.
static LEVELS_THE_FRAMES_READ: Mutex<Vec<i32>> = Mutex::new(Vec::new());

fn levels_the_frames_read() -> MutexGuard<'static, Vec<i32>> {
    LEVELS_THE_FRAMES_READ
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// The level each frame of the [`PanickingLifecycle`] node read, in order.
pub fn levels_the_frames_of_the_panicking_node_read() -> Vec<i32> {
    levels_the_frames_read().clone()
}

/// Forget the levels the frames of the [`PanickingLifecycle`] node read, for a test starting
/// afresh.
pub fn forget_the_levels_the_frames_of_the_panicking_node_read() {
    levels_the_frames_read().clear();
}

/// The computed value: the level it was built from.
#[derive(Debug, PartialEq, Eq)]
struct LevelBuilt(i32);

#[streamlib::sdk::node(
    execution = continuous(interval_ms = 5),
    input("level", description = "The level its computed value is built from.", value = i32, default = 0),
    input("panic_in_its_build", description = "Whether the build of its computed value panics.", value = bool, default = false),
)]
#[meta(
    description = "Panic-injection fixture. Its computed value panics while panic_in_its_build is set; the scope reports the panic on the node, and the frames run on the value built before it."
)]
pub fn panicking_lifecycle(
    level: Input<i32>,
    panic_in_its_build: Input<bool>,
) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
    let level_built = computed(move |_build_ctx| {
        let level = *level.get();
        if *panic_in_its_build.get() {
            panic!("PanickingLifecycle: injected panic in the build of level {level}");
        }
        Ok::<_, String>(LevelBuilt(level))
    });
    move |_ctx| {
        levels_the_frames_read().push(level_built.get().0);
        Ok(())
    }
}
