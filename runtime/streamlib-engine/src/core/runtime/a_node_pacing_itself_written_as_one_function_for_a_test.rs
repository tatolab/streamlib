// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node type for tests that paces itself (`execution = manual`), written as one function: one
//! value input, `gain`, and one computed value built from it, a publishing of the gain that would
//! publish on its own. Its body returns no per-frame function. It records each run of its body,
//! each publishing opened and each one closed, for a test of a value set live reaching a node
//! that runs no frame at its next tick. One node of the type runs at a time.

use std::sync::Once;

use parking_lot::{Condvar, Mutex};

use crate::core::descriptors::NodeClassImportPath;
use crate::core::nodes::NODE_REGISTRY;
use crate::sdk::nodes::{Input, computed};

/// What the node of the type did, since a test last forgot it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WhatTheNodePacingItselfDid {
    /// How many times its body ran.
    pub bodies_run: usize,
    /// How many builds of its publishing started, held or not.
    pub builds_started: usize,
    /// The gain of each publishing opened, in the order opened.
    pub gains_opened: Vec<i32>,
    /// The gain of each publishing closed, in the order closed.
    pub gains_closed: Vec<i32>,
}

static DID: Mutex<WhatTheNodePacingItselfDid> = Mutex::new(WhatTheNodePacingItselfDid {
    bodies_run: 0,
    builds_started: 0,
    gains_opened: Vec::new(),
    gains_closed: Vec::new(),
});

/// Whether each build of the publishing waits until a test lets the builds go.
static BUILDS_HELD: Mutex<bool> = Mutex::new(false);
static BUILDS_LET_GO: Condvar = Condvar::new();

/// The computed value: a publishing of one gain, recording its close.
struct PublishingOfAGain {
    gain: i32,
}

impl PartialEq for PublishingOfAGain {
    fn eq(&self, other: &Self) -> bool {
        self.gain == other.gain
    }
}

impl Drop for PublishingOfAGain {
    fn drop(&mut self) {
        DID.lock().gains_closed.push(self.gain);
    }
}

#[crate::sdk::node(
    execution = manual,
    input("gain", description = "The gain its publishing publishes.", value = i32, default = 1),
)]
fn a_node_pacing_itself(gain: Input<i32>) {
    DID.lock().bodies_run += 1;
    computed(move || {
        let gain = *gain.get();
        DID.lock().builds_started += 1;
        let mut held = BUILDS_HELD.lock();
        while *held {
            BUILDS_LET_GO.wait(&mut held);
        }
        drop(held);
        DID.lock().gains_opened.push(gain);
        Ok::<_, String>(PublishingOfAGain { gain })
    });
}

/// The type of the node pacing itself, registered once per test binary.
pub fn the_node_type_pacing_itself() -> NodeClassImportPath {
    static REGISTERED: Once = Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<ANodePacingItself::Node>());
    ANodePacingItself::Node::node_class_import_path()
}

/// What the node of the type did since a test last forgot it.
pub fn what_the_node_pacing_itself_did() -> WhatTheNodePacingItselfDid {
    DID.lock().clone()
}

/// Forget what the node of the type did, and let its builds go, for a test starting afresh.
pub fn forget_what_the_node_pacing_itself_did() {
    *DID.lock() = WhatTheNodePacingItselfDid::default();
    let_the_builds_go();
}

/// Each build of the node's publishing held until this is dropped, so a test that fails while
/// builds are held lets them go as it unwinds instead of waiting on them forever.
#[must_use = "the builds are let go when this is dropped"]
pub struct ThePublishingBuildsHeld(());

impl Drop for ThePublishingBuildsHeld {
    fn drop(&mut self) {
        let_the_builds_go();
    }
}

/// Hold each build of the node's publishing until what this returns is dropped.
pub fn hold_the_publishing_builds() -> ThePublishingBuildsHeld {
    *BUILDS_HELD.lock() = true;
    ThePublishingBuildsHeld(())
}

fn let_the_builds_go() {
    *BUILDS_HELD.lock() = false;
    BUILDS_LET_GO.notify_all();
}
