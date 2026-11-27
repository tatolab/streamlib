// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node type for tests written as one function: one value input, `gain`, and one computed
//! value built from it, which each frame reads. It records each run of its body, the `gain`
//! each build read, the value the latest frame read and each built value dropped, for a test of
//! a value set live reaching a node that is never rebuilt. One node of the type runs at a time.

use std::convert::Infallible;
use std::sync::Once;

use parking_lot::{Condvar, Mutex};

use crate::core::context::RuntimeContextLimitedAccess;
use crate::core::descriptors::NodeClassImportPath;
use crate::core::error::Result;
use crate::core::nodes::NODE_REGISTRY;
use crate::sdk::nodes::{Input, computed};

/// What the node of the type ran, since a test last forgot it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WhatTheNodeFunctionRan {
    /// How many times its body ran.
    pub bodies_run: usize,
    /// How many builds of its computed value started, held or not.
    pub builds_started: usize,
    /// The `gain` each build of its computed value read, in the order built.
    pub gains_built_from: Vec<i32>,
    /// How many frames ran.
    pub frames_run: usize,
    /// The gain of the computed value the latest frame read; `None` before any frame.
    pub gain_the_latest_frame_read: Option<i32>,
    /// The gain of each built value dropped, in the order dropped.
    pub gains_dropped: Vec<i32>,
}

static RAN: Mutex<WhatTheNodeFunctionRan> = Mutex::new(WhatTheNodeFunctionRan {
    bodies_run: 0,
    builds_started: 0,
    gains_built_from: Vec::new(),
    frames_run: 0,
    gain_the_latest_frame_read: None,
    gains_dropped: Vec::new(),
});

/// Whether each build of the computed value waits until a test lets the builds go.
static BUILDS_HELD: Mutex<bool> = Mutex::new(false);
static BUILDS_LET_GO: Condvar = Condvar::new();

/// The computed value: the gain it was built from, recording its drop.
struct BuiltFromAGain {
    gain: i32,
}

impl PartialEq for BuiltFromAGain {
    fn eq(&self, other: &Self) -> bool {
        self.gain == other.gain
    }
}

impl Drop for BuiltFromAGain {
    fn drop(&mut self) {
        RAN.lock().gains_dropped.push(self.gain);
    }
}

#[crate::sdk::node(
    execution = continuous(interval_ms = 5),
    input("gain", description = "The gain its computed value is built from.", value = i32, default = 1),
)]
fn a_node_written_as_one_function(
    gain: Input<i32>,
) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
    RAN.lock().bodies_run += 1;
    let built = computed(move || {
        let gain = *gain.get();
        RAN.lock().builds_started += 1;
        let mut held = BUILDS_HELD.lock();
        while *held {
            BUILDS_LET_GO.wait(&mut held);
        }
        drop(held);
        RAN.lock().gains_built_from.push(gain);
        Ok::<_, Infallible>(BuiltFromAGain { gain })
    });
    move |_ctx| {
        // Counted before the read, so a frame run before the first build is counted too.
        RAN.lock().frames_run += 1;
        let gain = built.get().gain;
        RAN.lock().gain_the_latest_frame_read = Some(gain);
        Ok(())
    }
}

/// The type of the node written as one function, registered once per test binary.
pub fn the_node_type_written_as_one_function() -> NodeClassImportPath {
    static REGISTERED: Once = Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<ANodeWrittenAsOneFunction::Node>());
    ANodeWrittenAsOneFunction::Node::node_class_import_path()
}

/// What the node of the type ran since a test last forgot it.
pub fn what_the_node_function_ran() -> WhatTheNodeFunctionRan {
    RAN.lock().clone()
}

/// Forget what the node of the type ran, and let its builds go, for a test starting afresh.
pub fn forget_what_the_node_function_ran() {
    *RAN.lock() = WhatTheNodeFunctionRan::default();
    let_the_builds_go();
}

/// Each build of the node's computed value held until this is dropped, so a test that fails
/// while builds are held lets them go as it unwinds instead of waiting on them forever.
#[must_use = "the builds are let go when this is dropped"]
pub struct TheBuildsHeld(());

impl Drop for TheBuildsHeld {
    fn drop(&mut self) {
        let_the_builds_go();
    }
}

/// Hold each build of the node's computed value until what this returns is dropped.
pub fn hold_the_builds() -> TheBuildsHeld {
    *BUILDS_HELD.lock() = true;
    TheBuildsHeld(())
}

fn let_the_builds_go() {
    *BUILDS_HELD.lock() = false;
    BUILDS_LET_GO.notify_all();
}
