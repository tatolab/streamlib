// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Read and write views handed back to consumers inside an acquire scope.
//!
//! Both views are short-lived (lifetime-bound to the guard) and expose
//! only what a CPU consumer needs: a byte slice (`&[u8]` or
//! `&mut [u8]`) sized to `width * height * bytes_per_pixel`. Stride is
//! always tightly packed.
//!
//! These are the only [`streamlib_adapter_abi::SurfaceAdapter`] views
//! in-tree that implement [`streamlib_adapter_abi::CpuReadable`] /
//! [`streamlib_adapter_abi::CpuWritable`]. GPU adapters
//! (`streamlib-adapter-vulkan`, `-opengl`, `-skia`) deliberately do
//! not — switching to this adapter is the contractual signal that the
//! caller has opted into a host-side GPU→CPU copy.

use std::marker::PhantomData;

use streamlib_adapter_abi::{CpuReadable, CpuWritable};

/// Read view of an acquired surface — a tightly-packed byte slice with
/// the surface's current pixel content (already copied from GPU at
/// `acquire_read` time).
pub struct CpuReadbackReadView<'g> {
    pub(crate) bytes: &'g [u8],
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bytes_per_pixel: u32,
    pub(crate) _marker: PhantomData<&'g ()>,
}

impl<'g> CpuReadbackReadView<'g> {
    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Bytes per pixel (4 for BGRA8 / RGBA8).
    pub fn bytes_per_pixel(&self) -> u32 {
        self.bytes_per_pixel
    }

    /// Tightly-packed row stride in bytes (`width * bytes_per_pixel`).
    pub fn row_stride(&self) -> u32 {
        self.width * self.bytes_per_pixel
    }

    /// Tightly-packed pixel bytes — `(height, width, bytes_per_pixel)`
    /// in row-major order.
    pub fn bytes(&self) -> &[u8] {
        self.bytes
    }
}

impl CpuReadable for CpuReadbackReadView<'_> {
    fn read_bytes(&self) -> &[u8] {
        self.bytes
    }
}

/// Write view of an acquired surface — a tightly-packed mutable byte
/// slice initialized with the current pixel content. Customer mutations
/// are flushed back to the host's `VkImage` on guard drop via
/// `vkCmdCopyBufferToImage` before the timeline release-value signals.
pub struct CpuReadbackWriteView<'g> {
    pub(crate) bytes: &'g mut [u8],
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bytes_per_pixel: u32,
    pub(crate) _marker: PhantomData<&'g mut ()>,
}

impl<'g> CpuReadbackWriteView<'g> {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn bytes_per_pixel(&self) -> u32 {
        self.bytes_per_pixel
    }

    pub fn row_stride(&self) -> u32 {
        self.width * self.bytes_per_pixel
    }

    pub fn bytes(&self) -> &[u8] {
        self.bytes
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        self.bytes
    }
}

impl CpuReadable for CpuReadbackWriteView<'_> {
    fn read_bytes(&self) -> &[u8] {
        self.bytes
    }
}

impl CpuWritable for CpuReadbackWriteView<'_> {
    fn write_bytes(&mut self) -> &mut [u8] {
        self.bytes
    }
}
