// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Test values shared by the scope's test binaries.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A value that counts how many of its instances are alive.
pub(crate) struct Counted {
    pub(crate) value: u32,
    alive: Arc<AtomicUsize>,
}

impl Counted {
    pub(crate) fn new(value: u32, alive: &Arc<AtomicUsize>) -> Self {
        alive.fetch_add(1, Ordering::SeqCst);
        Self {
            value,
            alive: Arc::clone(alive),
        }
    }
}

impl PartialEq for Counted {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.alive.fetch_sub(1, Ordering::SeqCst);
    }
}
