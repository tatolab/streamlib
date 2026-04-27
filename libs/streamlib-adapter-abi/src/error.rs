// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Error taxonomy for surface adapter operations.

use std::time::Duration;

use thiserror::Error;

use crate::surface::SurfaceId;

/// Failure cases an adapter implementation can return from
/// `acquire_read` / `acquire_write` and related operations.
///
/// Variants name the failure precisely so callers can branch (and
/// observers can log) without parsing strings.
#[derive(Debug, Error)]
pub enum AdapterError {
    /// Another acquire holds the write lock and concurrent write is forbidden.
    #[error("write contended on surface {surface_id}: held by {holder}")]
    WriteContended {
        surface_id: SurfaceId,
        /// Identifier of whoever currently holds the write — adapter-defined
        /// (could be a subprocess pid, a worker name, etc.).
        holder: String,
    },

    /// The descriptor doesn't refer to a surface this adapter knows about.
    #[error("surface {surface_id} not found")]
    SurfaceNotFound { surface_id: SurfaceId },

    /// The host-side IPC channel went away before/during the operation.
    #[error("IPC disconnected: {reason}")]
    IpcDisconnected { reason: String },

    /// A wait on the timeline semaphore exceeded the configured timeout.
    #[error("sync timeout after {duration:?}")]
    SyncTimeout { duration: Duration },

    /// The host-side backing for this surface was destroyed (refcount → 0).
    #[error("backing for surface {surface_id} was destroyed")]
    BackingDestroyed { surface_id: SurfaceId },

    /// The surface descriptor's pixel format / layout is not supported
    /// by this adapter — distinct from [`Self::SurfaceNotFound`]
    /// (which is a registry miss). `reason` names the specific limit
    /// hit (e.g. `"bytes_per_pixel != 4"`, `"NV12 multi-plane"`,
    /// `"non-color aspect"`).
    #[error("surface {surface_id}: unsupported format ({reason})")]
    UnsupportedFormat {
        surface_id: SurfaceId,
        reason: String,
    },
}
