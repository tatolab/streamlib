// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Read and write views handed back to consumers inside an acquire scope.
//!
//! Both views expose the surface as a sequence of planes — single-plane
//! formats (BGRA/RGBA) report one plane, multi-plane formats (NV12)
//! report one plane per logical plane (Y + UV for NV12). Each plane is a
//! tightly-packed byte slice (`width * bytes_per_pixel` per row).
//!
//! These are the only [`streamlib_adapter_abi::SurfaceAdapter`] views
//! in-tree that implement [`streamlib_adapter_abi::CpuReadable`] /
//! [`streamlib_adapter_abi::CpuWritable`]. GPU adapters
//! (`streamlib-adapter-vulkan`, `-opengl`, `-skia`) deliberately do
//! not — switching to this adapter is the contractual signal that the
//! caller has opted into a host-side GPU→CPU copy.

use std::marker::PhantomData;

use streamlib_adapter_abi::{CpuReadable, CpuWritable, SurfaceFormat};

/// Read-only view of one plane of an acquired surface — tightly-packed
/// pixel content, dimensions in plane texels (NV12 UV: half width × half
/// height), and bytes-per-texel.
pub struct CpuReadbackPlaneView<'g> {
    pub(crate) bytes: &'g [u8],
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bytes_per_pixel: u32,
    pub(crate) _marker: PhantomData<&'g ()>,
}

impl<'g> CpuReadbackPlaneView<'g> {
    /// Plane width in texels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Plane height in texels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Bytes per texel of this plane (BGRA: 4, NV12 Y: 1, NV12 UV: 2).
    pub fn bytes_per_pixel(&self) -> u32 {
        self.bytes_per_pixel
    }

    /// Tightly-packed row stride in bytes (`width * bytes_per_pixel`).
    pub fn row_stride(&self) -> u32 {
        self.width * self.bytes_per_pixel
    }

    /// Tightly-packed pixel bytes, row-major, no padding.
    pub fn bytes(&self) -> &[u8] {
        self.bytes
    }
}

/// Mutable view of one plane of an acquired surface. Returned only from
/// a [`CpuReadbackWriteView`] inside a write guard scope.
pub struct CpuReadbackPlaneViewMut<'g> {
    pub(crate) bytes: &'g mut [u8],
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bytes_per_pixel: u32,
    pub(crate) _marker: PhantomData<&'g mut ()>,
}

impl<'g> CpuReadbackPlaneViewMut<'g> {
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

/// Read view of an acquired surface — surface-level metadata plus the
/// list of planes (already copied from GPU at `acquire_read` time).
///
/// For BGRA/RGBA the view reports a single plane; for NV12 it reports
/// two (Y at index 0, UV at index 1). [`Self::plane_count`] reflects the
/// surface's [`SurfaceFormat`].
pub struct CpuReadbackReadView<'g> {
    pub(crate) format: SurfaceFormat,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) planes: Vec<CpuReadbackPlaneView<'g>>,
}

impl<'g> CpuReadbackReadView<'g> {
    /// Surface pixel format.
    pub fn format(&self) -> SurfaceFormat {
        self.format
    }

    /// Surface width in pixels (= plane 0 width).
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Surface height in pixels (= plane 0 height).
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Number of planes in this surface (1 for BGRA/RGBA, 2 for NV12).
    pub fn plane_count(&self) -> u32 {
        self.planes.len() as u32
    }

    /// Borrow plane `index`. Panics if `index >= plane_count()`.
    pub fn plane(&self, index: u32) -> &CpuReadbackPlaneView<'g> {
        &self.planes[index as usize]
    }

    /// All planes in declaration order (plane 0 first). For NV12: `[Y, UV]`.
    pub fn planes(&self) -> &[CpuReadbackPlaneView<'g>] {
        &self.planes
    }
}

impl CpuReadable for CpuReadbackReadView<'_> {
    /// Returns the **primary plane**'s bytes (plane 0). For single-plane
    /// formats this is the entire image; for multi-plane formats (NV12)
    /// it is the Y/luma plane. Use [`Self::plane`] to reach chroma planes.
    fn read_bytes(&self) -> &[u8] {
        self.planes[0].bytes
    }
}

/// Write view of an acquired surface. Edits to any plane's bytes are
/// flushed back to the host's `VkImage` (per-plane
/// `vkCmdCopyBufferToImage`) on guard drop, before the timeline release-
/// value signals.
pub struct CpuReadbackWriteView<'g> {
    pub(crate) format: SurfaceFormat,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) planes: Vec<CpuReadbackPlaneViewMut<'g>>,
}

impl<'g> CpuReadbackWriteView<'g> {
    pub fn format(&self) -> SurfaceFormat {
        self.format
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn plane_count(&self) -> u32 {
        self.planes.len() as u32
    }

    /// Borrow plane `index` immutably. Panics if `index >= plane_count()`.
    pub fn plane(&self, index: u32) -> &CpuReadbackPlaneViewMut<'g> {
        &self.planes[index as usize]
    }

    /// Borrow plane `index` mutably. Panics if `index >= plane_count()`.
    pub fn plane_mut(&mut self, index: u32) -> &mut CpuReadbackPlaneViewMut<'g> {
        &mut self.planes[index as usize]
    }

    /// All planes in declaration order, immutable.
    pub fn planes(&self) -> &[CpuReadbackPlaneViewMut<'g>] {
        &self.planes
    }

    /// All planes in declaration order, mutable.
    pub fn planes_mut(&mut self) -> &mut [CpuReadbackPlaneViewMut<'g>] {
        &mut self.planes
    }
}

impl CpuReadable for CpuReadbackWriteView<'_> {
    fn read_bytes(&self) -> &[u8] {
        self.planes[0].bytes
    }
}

impl CpuWritable for CpuReadbackWriteView<'_> {
    /// Returns mutable access to the **primary plane**'s bytes (plane 0).
    /// For NV12 surfaces, callers wanting to write chroma must use
    /// [`CpuReadbackWriteView::plane_mut`].
    fn write_bytes(&mut self) -> &mut [u8] {
        self.planes[0].bytes
    }
}
