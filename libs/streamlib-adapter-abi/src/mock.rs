// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reference [`SurfaceAdapter`] implementation used by the conformance
//! suite and as a worked example for 3rd-party adapter authors.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::adapter::SurfaceAdapter;
use crate::error::AdapterError;
use crate::guard::{ReadGuard, WriteGuard};
use crate::surface::{StreamlibSurface, SurfaceId};

/// Counters for per-surface acquire/release accounting.
///
/// `read_holders` tracks the number of currently-live `ReadGuard`s.
/// `write_held` tracks whether a `WriteGuard` is live. The mock rejects
/// a write acquire while either is non-zero.
#[derive(Default, Debug)]
struct AccessCounters {
    read_holders: u64,
    write_held: bool,
    /// Total acquire_read calls that succeeded.
    acquires_read: u64,
    /// Total acquire_write calls that succeeded.
    acquires_write: u64,
    /// Total guard drops.
    releases_read: u64,
    releases_write: u64,
}

/// View handed out by the mock for both read and write paths — a
/// per-acquire counter that lets conformance tests confirm the right
/// view was vended.
#[derive(Debug)]
pub struct MockView {
    pub surface_id: SurfaceId,
    pub acquire_serial: u64,
}

/// Reference adapter — pure-Rust, in-memory, no host IPC.
pub struct MockAdapter {
    counters: Mutex<AccessCounters>,
    serial: AtomicU64,
    /// Whether the next read acquire should fail with
    /// [`AdapterError::SurfaceNotFound`]. Used by conformance tests.
    pub fail_with_not_found: AtomicU64,
}

impl Default for MockAdapter {
    fn default() -> Self {
        Self {
            counters: Mutex::new(AccessCounters::default()),
            serial: AtomicU64::new(0),
            fail_with_not_found: AtomicU64::new(0),
        }
    }
}

impl MockAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of acquire/release counters for assertions.
    pub fn snapshot(&self) -> AdapterCountersSnapshot {
        let c = self.counters.lock().expect("mock adapter mutex poisoned");
        AdapterCountersSnapshot {
            acquires_read: c.acquires_read,
            acquires_write: c.acquires_write,
            releases_read: c.releases_read,
            releases_write: c.releases_write,
            read_holders: c.read_holders,
            write_held: c.write_held,
        }
    }
}

/// Counter snapshot exposed for assertions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterCountersSnapshot {
    pub acquires_read: u64,
    pub acquires_write: u64,
    pub releases_read: u64,
    pub releases_write: u64,
    pub read_holders: u64,
    pub write_held: bool,
}

impl SurfaceAdapter for MockAdapter {
    type ReadView<'g> = MockView;
    type WriteView<'g> = MockView;

    fn acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'g, Self>, AdapterError> {
        if self.fail_with_not_found.load(Ordering::Acquire) != 0 {
            return Err(AdapterError::SurfaceNotFound {
                surface_id: surface.id,
            });
        }
        let mut c = self.counters.lock().expect("mock adapter mutex poisoned");
        if c.write_held {
            return Err(AdapterError::WriteContended {
                surface_id: surface.id,
                holder: "mock-writer".to_string(),
            });
        }
        c.read_holders += 1;
        c.acquires_read += 1;
        let serial = self.serial.fetch_add(1, Ordering::AcqRel);
        Ok(ReadGuard::new(
            self,
            surface.id,
            MockView {
                surface_id: surface.id,
                acquire_serial: serial,
            },
        ))
    }

    fn acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'g, Self>, AdapterError> {
        if self.fail_with_not_found.load(Ordering::Acquire) != 0 {
            return Err(AdapterError::SurfaceNotFound {
                surface_id: surface.id,
            });
        }
        let mut c = self.counters.lock().expect("mock adapter mutex poisoned");
        if c.write_held || c.read_holders > 0 {
            return Err(AdapterError::WriteContended {
                surface_id: surface.id,
                holder: if c.write_held {
                    "mock-writer".to_string()
                } else {
                    format!("{} reader(s)", c.read_holders)
                },
            });
        }
        c.write_held = true;
        c.acquires_write += 1;
        let serial = self.serial.fetch_add(1, Ordering::AcqRel);
        Ok(WriteGuard::new(
            self,
            surface.id,
            MockView {
                surface_id: surface.id,
                acquire_serial: serial,
            },
        ))
    }

    fn end_read_access(&self, _surface_id: SurfaceId) {
        let mut c = self.counters.lock().expect("mock adapter mutex poisoned");
        debug_assert!(c.read_holders > 0, "read release without matching acquire");
        c.read_holders = c.read_holders.saturating_sub(1);
        c.releases_read += 1;
    }

    fn end_write_access(&self, _surface_id: SurfaceId) {
        let mut c = self.counters.lock().expect("mock adapter mutex poisoned");
        debug_assert!(c.write_held, "write release without matching acquire");
        c.write_held = false;
        c.releases_write += 1;
    }
}
