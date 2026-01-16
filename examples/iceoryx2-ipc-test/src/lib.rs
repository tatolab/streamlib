// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared constants for iceoryx2 IPC test

pub const WIDTH: usize = 1920;
pub const HEIGHT: usize = 1080;
pub const CHANNELS: usize = 3; // RGB
pub const BUFFER_SIZE: usize = WIDTH * HEIGHT * CHANNELS; // ~6.2MB

/// Message containing only the IOSurface ID - no pixel data copied
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct IOSurfaceMessage {
    /// The IOSurface ID (from IOSurfaceGetID) - this is all we need to share GPU textures
    pub surface_id: u32,
    /// Frame number for tracking
    pub frame_number: u64,
    /// Width of the surface
    pub width: u32,
    /// Height of the surface
    pub height: u32,
    /// Timestamp when sent (nanos since epoch)
    pub timestamp_ns: u64,
}
