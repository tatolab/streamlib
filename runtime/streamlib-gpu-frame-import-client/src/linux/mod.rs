// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The Linux arm: the surface-sharing Unix socket, and a pool slot's memory
//! as DMA-BUF planes or one OPAQUE_FD allocation, a pixel buffer's or a
//! texture's.

mod gpu_pool_slot_of_a_viewer;
mod surface_share_connection_of_this_importer;

pub use gpu_pool_slot_of_a_viewer::{DmaBufPlaneOfAGpuPoolSlotOfAViewer, GpuPoolSlotOfAViewer};
pub use surface_share_connection_of_this_importer::{
    SurfaceShareConnectionOfThisImporter, SurfaceShareTransferredHandle,
};
