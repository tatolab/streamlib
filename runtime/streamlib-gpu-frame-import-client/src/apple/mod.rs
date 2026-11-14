// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The macOS arm: the surface-sharing Mach service, and a pool slot's memory
//! as an IOSurface.

mod gpu_pool_slot_of_a_viewer;
mod surface_share_connection_of_this_importer;

pub use gpu_pool_slot_of_a_viewer::GpuPoolSlotOfAViewer;
pub(crate) use gpu_pool_slot_of_a_viewer::IOSurfaceUseCountClaimOfAViewer;
pub use surface_share_connection_of_this_importer::{
    SurfaceShareConnectionOfThisImporter, SurfaceShareTransferredHandle,
};
