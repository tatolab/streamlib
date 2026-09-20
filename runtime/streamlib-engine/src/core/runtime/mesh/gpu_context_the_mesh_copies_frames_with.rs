// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The GPU context the mesh copies a frame's pixels with, once the runtime
//! has one.
//!
//! A runtime joins its mesh in `Runner::new()`, which needs no GPU; the
//! context is built in `Runner::start()`. Every egress and every ingress
//! therefore reads it through this cell rather than being handed one: an
//! egress holds a port for as long as another runtime reads it, and one whose
//! reader arrived before `start()` would otherwise carry no frame for the
//! rest of the run.
//!
//! Cleared when the runtime stops, so a mesh outliving its context — a
//! membership dropped after the runtime's own teardown — never keeps a GPU
//! device alive by holding the last clone of it.

use parking_lot::Mutex;

use crate::core::context::GpuContext;

/// Where the mesh reads this runtime's GPU context, or nothing while it has
/// none.
#[derive(Default)]
pub struct GpuContextTheMeshCopiesFramesWith {
    gpu_context: Mutex<Option<GpuContext>>,
}

impl GpuContextTheMeshCopiesFramesWith {
    /// Hand the mesh the context the runtime has just built.
    pub fn record_the_runtimes_gpu_context(&self, gpu_context: &GpuContext) {
        *self.gpu_context.lock() = Some(gpu_context.clone());
    }

    /// Forget it again, as the runtime stops.
    pub fn forget_the_runtimes_gpu_context(&self) {
        *self.gpu_context.lock() = None;
    }

    /// The context, or `None` while this runtime has none — before `start()`,
    /// and after `stop()`.
    ///
    /// Cloned out from under the lock rather than borrowed: the caller goes on
    /// to submit a GPU copy and wait on it, which is far longer than this lock
    /// may be held.
    pub(super) fn the_gpu_context_or_none(&self) -> Option<GpuContext> {
        self.gpu_context.lock().clone()
    }
}
