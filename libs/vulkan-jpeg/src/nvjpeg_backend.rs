// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! NVIDIA `libnvjpeg` (CUDA-based JPEG decode) [`JpegDecodeBackend`]
//! implementation.
//!
//! Engine-allocates / vendor-imports per
//! `docs/architecture/third-party-gpu-backends.md`:
//!
//! 1. Host allocates a DEVICE_LOCAL OPAQUE_FD-exportable `VkBuffer` sized
//!    for `max_width * max_height * 4` (worst-case RGBA8 output) via
//!    [`HostVulkanBuffer::new_opaque_fd_export_device_local`] — one per
//!    [`MAX_FRAMES_IN_FLIGHT`] ring slot.
//! 2. Host exports the FD via `vkGetMemoryFdKHR` and imports it into
//!    CUDA via `cudaImportExternalMemory(OPAQUE_FD)` →
//!    `cudaExternalMemoryGetMappedBuffer`, yielding a `CUdeviceptr`
//!    aliased to the same physical memory the Vulkan side sees.
//! 3. Host allocates an OPAQUE_FD-exportable Vulkan timeline semaphore
//!    via [`HostVulkanTimelineSemaphore::new_exportable`] and imports
//!    it into CUDA via `cudaImportExternalSemaphore(TimelineSemaphoreFd)`.
//! 4. Per-frame: `nvjpegDecode` decodes into the imported CUDA pointer
//!    (output format `NVJPEG_OUTPUT_RGBI` packed to a 4-channel layout
//!    matching `Rgba8Unorm`); CUDA signals the timeline; the Vulkan
//!    side waits on the same timeline (cross-API sync) and records
//!    `vkCmdCopyBufferToImage` into the next ring slot's render-target
//!    [`Texture`].
//! 5. The ring slot's `surface_id` is returned through
//!    [`JpegDecodeOutput`] — downstream consumers consume the texture
//!    identically regardless of which backend produced it.
//!
//! The `nvjpeg.h` symbols are resolved via [`libloading`] at construction
//! time; the workspace builds cleanly on hosts without libnvjpeg
//! installed, and the dlopen-failure path surfaces as
//! [`Error::NotSupported`]. See [`crate::backend::JpegDecodeBackend`] for
//! the trait contract and
//! `docs/architecture/third-party-gpu-backends.md` for the architecture
//! framing and "when to lift to engine-tier" trigger.

use streamlib::sdk::context::GpuContextFullAccess;
use streamlib::sdk::error::{Error, Result};

use crate::backend::{JpegBackendKind, JpegDecodeBackend};
use crate::simple_decoder::JpegDecodeOutput;

mod ffi;
mod resources;

use resources::NvJpegResources;

/// NVIDIA `libnvjpeg` backend for [`crate::simple_decoder::SimpleJpegDecoder`].
///
/// Allocates one OPAQUE_FD-exportable `VkBuffer` per ring slot (sized for
/// worst-case RGBA8 output at `max_width * max_height`) plus an exportable
/// Vulkan timeline semaphore — all imported into a CUDA context that
/// `libnvjpeg` decodes into. Per-frame decode runs on CUDA, signals the
/// timeline, and the host-side `vkCmdCopyBufferToImage` lands the result
/// in a normal render-target [`crate::simple_decoder::JpegDecodeOutput::texture`].
pub struct NvJpegBackend {
    resources: NvJpegResources,
    max_width: u32,
    max_height: u32,
}

impl std::fmt::Debug for NvJpegBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NvJpegBackend")
            .field("max_width", &self.max_width)
            .field("max_height", &self.max_height)
            .finish()
    }
}

impl NvJpegBackend {
    /// Build an nvJPEG backend ready to handle JPEGs up to
    /// `(max_width, max_height)` pixels.
    ///
    /// Returns [`Error::NotSupported`] when:
    /// - `libnvjpeg.so.12` cannot be `dlopen`ed (caller can fall back to
    ///   the Vulkan-compute backend; this is the gating check
    ///   [`crate::simple_decoder::SimpleJpegDecoder::new_with_preference`]
    ///   uses to choose `Auto`).
    /// - `libcudart.so.12` cannot be loaded.
    /// - CUDA context initialization fails (no CUDA-capable device, driver
    ///   mismatch, etc.).
    /// - `nvjpegCreateSimple` returns a non-success status.
    ///
    /// All other errors (OPAQUE_FD pool unavailable, FD export failure,
    /// timeline-semaphore export failure, CUDA memory-import failure)
    /// surface as [`Error::GpuError`].
    pub fn new(
        full_access: &GpuContextFullAccess,
        max_width: u32,
        max_height: u32,
    ) -> Result<Self> {
        if max_width == 0 || max_height == 0 {
            return Err(Error::GpuError(format!(
                "NvJpegBackend::new: max dimensions must be non-zero, got {}x{}",
                max_width, max_height,
            )));
        }
        let resources = NvJpegResources::new(full_access, max_width, max_height)?;
        Ok(Self {
            resources,
            max_width,
            max_height,
        })
    }
}

impl JpegDecodeBackend for NvJpegBackend {
    fn decode(&mut self, jpeg_bytes: &[u8]) -> Result<JpegDecodeOutput> {
        self.resources.decode(jpeg_bytes, self.max_width, self.max_height)
    }

    fn kind(&self) -> JpegBackendKind {
        JpegBackendKind::NvJpeg
    }
}
