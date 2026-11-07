// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The macOS arm: the surface-sharing Mach service, and a pool slot's memory
//! as an IOSurface.

mod pixel_buffer_pool_slot_of_a_viewer;
mod surface_share_connection_of_this_importer;

pub(crate) use pixel_buffer_pool_slot_of_a_viewer::IOSurfaceUseCountClaimOfAViewer;
pub use pixel_buffer_pool_slot_of_a_viewer::PixelBufferPoolSlotOfAViewer;
pub use surface_share_connection_of_this_importer::{
    SurfaceShareConnectionOfThisImporter, SurfaceShareTransferredHandle,
};
