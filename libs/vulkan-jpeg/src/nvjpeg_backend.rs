// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! NVIDIA `libnvjpeg` (CUDA-based JPEG decode) [`JpegDecodeBackend`]
//! implementation — placeholder. The dispatcher in
//! [`crate::simple_decoder::SimpleJpegDecoder`] references this type so
//! [`crate::backend::JpegBackendKind::NvJpeg`] has a typed
//! implementation to dispatch to; the actual decode flow (CUDA context
//! init, OPAQUE_FD interop, `nvjpegDecode` + cudaMemcpy2DAsync +
//! vkCmdCopyBufferToImage) lands in a follow-up commit. Until then,
//! `NvJpegBackend::new` returns [`Error::NotSupported`] and the
//! dispatcher's `Auto` path falls back to
//! [`crate::vulkan_compute_backend::VulkanComputeBackend`].

use streamlib::sdk::context::GpuContextFullAccess;
use streamlib::sdk::error::{Error, Result};

use crate::backend::{JpegBackendKind, JpegDecodeBackend};
use crate::simple_decoder::JpegDecodeOutput;

pub struct NvJpegBackend;

impl std::fmt::Debug for NvJpegBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NvJpegBackend").finish_non_exhaustive()
    }
}

impl NvJpegBackend {
    pub fn new(
        _full_access: &GpuContextFullAccess,
        _max_width: u32,
        _max_height: u32,
    ) -> Result<Self> {
        Err(Error::NotSupported(
            "nvJPEG backend implementation pending — only the dispatcher \
             wiring + capability probe land in this commit"
                .into(),
        ))
    }
}

impl JpegDecodeBackend for NvJpegBackend {
    fn decode(&mut self, _jpeg_bytes: &[u8]) -> Result<JpegDecodeOutput> {
        // Unreachable: `Self::new` always errors at this commit's tree
        // state, so no caller holds a constructed `NvJpegBackend`.
        Err(Error::NotSupported(
            "nvJPEG backend implementation pending".into(),
        ))
    }

    fn kind(&self) -> JpegBackendKind {
        JpegBackendKind::NvJpeg
    }
}
