// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The Linux arm: the surface-sharing Unix socket, and a pool slot's memory
//! as DMA-BUF planes or one OPAQUE_FD allocation.

mod pixel_buffer_pool_slot_of_a_viewer;
mod surface_share_connection_of_this_importer;

pub use pixel_buffer_pool_slot_of_a_viewer::PixelBufferPoolSlotOfAViewer;
pub use surface_share_connection_of_this_importer::{
    SurfaceShareConnectionOfThisImporter, SurfaceShareTransferredHandle,
};
