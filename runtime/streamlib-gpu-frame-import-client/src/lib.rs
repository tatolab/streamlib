// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Imports the frames a runtime's GPU pools publish into another process —
//! a helper process or a viewer — over the runtime's surface-sharing service.
//!
//! [`SurfaceShareConnectionOfThisImporter`] makes every request, dropping the
//! pool slot imports the service retires; [`GpuPoolClaimTablesOfThisImporter`]
//! holds each pooled frame against producer reuse in its pool's claim table;
//! [`PoolSlotImportsByPoolSlot`] keeps one import per pool slot.
//! [`GpuPoolFrameImporterOfAViewer`] is the whole read a viewer makes of an
//! Image frame: claimed, and handed over as its slot's IOSurface on macOS, and
//! on Linux as its DMA-BUF planes or, for memory that crossed as OPAQUE_FD, a
//! CPU read.

#[cfg(target_os = "macos")]
mod apple;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod gpu_pool_claim_tables_of_this_importer;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod gpu_pool_frame_importer_of_a_viewer;
#[cfg(target_os = "linux")]
mod linux;
mod pool_slot_imports_by_pool_slot;

#[cfg(target_os = "macos")]
pub use apple::{
    PixelBufferPoolSlotOfAViewer, SurfaceShareConnectionOfThisImporter,
    SurfaceShareTransferredHandle,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use gpu_pool_claim_tables_of_this_importer::{
    GpuPoolClaimTableOfThisImporter, GpuPoolClaimTablesOfThisImporter,
    GpuPoolFrameClaimAttemptOfThisImporter, GpuPoolSlotFrameClaimOfThisImporter,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use gpu_pool_frame_importer_of_a_viewer::{
    GpuPoolFrameHeldByAViewer, GpuPoolFrameImporterOfAViewer,
};
#[cfg(target_os = "linux")]
pub use linux::{
    PixelBufferPoolSlotOfAViewer, SurfaceShareConnectionOfThisImporter,
    SurfaceShareTransferredHandle,
};
pub use pool_slot_imports_by_pool_slot::{
    PoolSlotImportReservation, PoolSlotImportsByPoolSlot, pool_slot_of_a_published_frame_id,
    pool_slots_a_reply_lists_as_retired,
};

/// How many connections an importer sets aside after a failed or timed-out
/// request before it stops asking the service anything: each one stays open,
/// and a service that has failed this often has stopped answering.
pub const SURFACE_SHARE_CONNECTIONS_SET_ASIDE_AT_MOST: usize = 4;

/// How long the surface-share channel may keep an importer waiting: every
/// request on Linux, the connect handshake on macOS. A Mach request carries
/// no receive timeout; the service's death is what wakes one still pending.
///
/// The service answers from in-memory state and duplicated handles, never GPU
/// work, so this bounds only a service that has stopped answering — and with
/// it the time every other thread waits on the connection's lock.
pub const SURFACE_SHARE_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
