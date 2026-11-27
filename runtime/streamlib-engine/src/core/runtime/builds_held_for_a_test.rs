// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A gate a test node type's builds wait at while a test holds them, so a test can watch what a
//! node does while a build is in flight.

use parking_lot::{Condvar, Mutex};

/// Whether the builds that wait at it are held, and the wake of those waiting once let go.
pub struct BuildsHeldGate {
    held: Mutex<bool>,
    let_go: Condvar,
}

impl Default for BuildsHeldGate {
    fn default() -> Self {
        Self::new()
    }
}

impl BuildsHeldGate {
    /// A gate holding no build, until a test holds them.
    pub const fn new() -> Self {
        Self {
            held: Mutex::new(false),
            let_go: Condvar::new(),
        }
    }

    /// Wait, in a build, for as long as the builds are held.
    pub fn wait_while_held(&self) {
        let mut held = self.held.lock();
        while *held {
            self.let_go.wait(&mut held);
        }
    }

    /// Hold each build that waits at the gate until what this returns is dropped, so a test that
    /// fails while builds are held lets them go as it unwinds instead of waiting on them forever.
    pub fn hold(&'static self) -> TheBuildsHeld {
        *self.held.lock() = true;
        TheBuildsHeld(self)
    }

    /// Let every build waiting at the gate go.
    pub fn let_go(&self) {
        *self.held.lock() = false;
        self.let_go.notify_all();
    }
}

/// The builds of one gate held, until this is dropped.
#[must_use = "the builds are let go when this is dropped"]
pub struct TheBuildsHeld(&'static BuildsHeldGate);

impl Drop for TheBuildsHeld {
    fn drop(&mut self) {
        self.0.let_go();
    }
}
