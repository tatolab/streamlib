// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::os::fd::RawFd;
use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib::sdk::rhi::{PixelFormat, PixelPlanesWithinOneAllocation};

use crate::python_helper_process_pixel_exchange::{
    HelperCheckedOutPixelSurface, HelperCheckedOutSurface, HelperProcessGpuExchangeClient,
};

/// The unregistration a foreign-fd adoption owes the surface-share service:
/// one `release` op under the child-scoped registration id, closing the
/// service's dups and removing the entry the graph resolved.
pub(crate) struct HelperForeignSurfaceUnregisterDebt {
    exchange_client: Arc<HelperProcessGpuExchangeClient>,
    surface_id: String,
}

impl Drop for HelperForeignSurfaceUnregisterDebt {
    /// Best-effort: a service that is already gone released everything with
    /// its socket, and the crash watchdog covers a child that never gets
    /// here — so a failure is logged, never raised.
    fn drop(&mut self) {
        Python::attach(|python| {
            if let Err(release_failure) = self
                .exchange_client
                .release_the_color_conversion_source_the_parent_resolved(python, &self.surface_id)
            {
                tracing::warn!(
                    "handing the parent the release of adopted surface {}'s conversion source \
                     failed ({release_failure}); the parent holds its import until this node \
                     stops",
                    self.surface_id
                );
            }
            let released = python.detach(|| {
                self.exchange_client
                    .unregister_foreign_surface(&self.surface_id)
            });
            if let Err(release_failure) = released {
                tracing::warn!(
                    "unregistering adopted surface {} failed ({release_failure}); the service's \
                     dup of the foreign fd stays open until this node stops",
                    self.surface_id
                );
            }
        });
    }
}

/// `DRM_FORMAT_MOD_LINEAR`: rows laid out one after another at the stated
/// pitch, the only modifier a buffer read through a plane layout can carry.
const DRM_FORMAT_MOD_LINEAR: u64 = 0;

/// The allocation size and plane layout an `import_dma_buf` call describes.
///
/// A stated layout needs the allocation's byte size beside it. Without one, a
/// single plane's pitch is the byte size spread over whole rows, and several
/// planes sit tightly packed.
pub(crate) fn foreign_dma_buf_plane_layout(
    pixel_format: PixelFormat,
    width: u32,
    height: u32,
    byte_size: Option<u64>,
    plane_offsets: Option<Vec<u64>>,
    plane_strides: Option<Vec<u64>>,
    drm_format_modifier: u64,
) -> Result<(u64, PixelPlanesWithinOneAllocation), String> {
    if drm_format_modifier != DRM_FORMAT_MOD_LINEAR {
        return Err(format!(
            "import_dma_buf reads linear buffers only; drm_format_modifier \
             {drm_format_modifier:#x} is a tiled or compressed layout"
        ));
    }
    if width == 0 || height == 0 {
        return Err(
            "import_dma_buf needs a non-zero width and height; the fd carries no \
                    geometry of its own"
                .to_string(),
        );
    }
    let layout_refused = |refusal: streamlib::sdk::error::Error| refusal.to_string();
    let (allocation_byte_size, plane_offsets, plane_strides) = match (plane_offsets, plane_strides)
    {
        (Some(plane_offsets), Some(plane_strides)) => {
            let allocation_byte_size = byte_size.ok_or_else(|| {
                "import_dma_buf with plane_offsets and plane_strides needs byte_size, the size \
                 of the allocation the planes sit in"
                    .to_string()
            })?;
            (allocation_byte_size, plane_offsets, plane_strides)
        }
        (None, None) if pixel_format.plane_count() == 1 => {
            let allocation_byte_size = match byte_size {
                Some(allocation_byte_size) => allocation_byte_size,
                None => PixelPlanesWithinOneAllocation::tightly_packed_byte_size(
                    pixel_format,
                    width,
                    height,
                )
                .map_err(layout_refused)?,
            };
            if allocation_byte_size == 0 || !allocation_byte_size.is_multiple_of(u64::from(height))
            {
                return Err(format!(
                    "import_dma_buf byte_size {allocation_byte_size} is not a whole number of \
                     {height} rows; state plane_strides when the last row is unpadded"
                ));
            }
            (
                allocation_byte_size,
                vec![0],
                vec![allocation_byte_size / u64::from(height)],
            )
        }
        (None, None) => {
            let tight = PixelPlanesWithinOneAllocation::tightly_packed(pixel_format, width, height)
                .map_err(layout_refused)?;
            let allocation_byte_size = match byte_size {
                Some(allocation_byte_size) => allocation_byte_size,
                None => PixelPlanesWithinOneAllocation::tightly_packed_byte_size(
                    pixel_format,
                    width,
                    height,
                )
                .map_err(layout_refused)?,
            };
            (
                allocation_byte_size,
                tight.plane_offsets_bytes().to_vec(),
                tight.plane_strides_bytes().to_vec(),
            )
        }
        _ => {
            return Err(
                "import_dma_buf takes plane_offsets and plane_strides together or neither"
                    .to_string(),
            );
        }
    };
    let planes = PixelPlanesWithinOneAllocation::new(
        pixel_format,
        width,
        height,
        allocation_byte_size,
        plane_offsets,
        plane_strides,
        true,
    )
    .map_err(layout_refused)?;
    Ok((allocation_byte_size, planes))
}

impl HelperProcessGpuExchangeClient {
    /// Adopt a foreign DMA-BUF fd as a surface this graph can resolve.
    ///
    /// The fd crosses to the surface-share service over SCM_RIGHTS on a
    /// `check_in` (the kernel and the service each dup it — the caller keeps
    /// ownership of the original), with the plane layout of the pixels inside
    /// it and the mark that a foreign device writes it; the service mints a
    /// surface id, and the checkout of that id lands this process's own
    /// mapping of the memory. The returned surface carries the unregister
    /// debt: dropping the last share removes the service entry the adoption
    /// created.
    #[expect(
        clippy::too_many_arguments,
        reason = "each is one stated property of the foreign allocation"
    )]
    pub(crate) fn import_foreign_dma_buf(
        self: &Arc<Self>,
        python: Python<'_>,
        foreign_dma_buf_fd: RawFd,
        width: u32,
        height: u32,
        pixel_format: PixelFormat,
        allocation_byte_size: u64,
        planes: &PixelPlanesWithinOneAllocation,
    ) -> PyResult<HelperCheckedOutPixelSurface> {
        let checked_out = python.detach(|| {
            let adopted_surface_id = self.register_foreign_dma_buf(
                foreign_dma_buf_fd,
                width,
                height,
                pixel_format,
                allocation_byte_size,
                planes,
            )?;
            self.record_a_foreign_adoption(&adopted_surface_id);
            // The debt exists from the moment the service registered: if the
            // checkout below fails, this drops on the error path and removes
            // the entry instead of stranding the service's fd dup.
            let unregister_debt = HelperForeignSurfaceUnregisterDebt {
                exchange_client: Arc::clone(self),
                surface_id: adopted_surface_id.clone(),
            };
            let checked_out = self.check_out_and_import(&adopted_surface_id)?;
            let HelperCheckedOutSurface::PixelBuffer(mut checked_out_pixel_surface) = checked_out
            else {
                return Err(PyRuntimeError::new_err(format!(
                    "adopted surface {adopted_surface_id:?} resolved to a texture \
                     registration; check_in registers pixel buffers only"
                )));
            };
            checked_out_pixel_surface.unregister_foreign_from_surface_share = Some(unregister_debt);
            Ok(checked_out_pixel_surface)
        })?;
        Ok(checked_out)
    }

    /// Register a foreign DMA-BUF fd with the surface-share service, with the
    /// plane layout of the pixels inside it and the mark that a foreign device
    /// writes it, and return the surface id the service minted. The caller
    /// owes the registration its [`Self::unregister_foreign_surface`].
    pub(crate) fn register_foreign_dma_buf(
        &self,
        foreign_dma_buf_fd: RawFd,
        width: u32,
        height: u32,
        pixel_format: PixelFormat,
        allocation_byte_size: u64,
        planes: &PixelPlanesWithinOneAllocation,
    ) -> PyResult<String> {
        let mut check_in_request = serde_json::json!({
            "op": "check_in",
            "runtime_id": self.foreign_surface_registration_runtime_id,
            "width": width,
            "height": height,
            "format": pixel_format.wire_name(),
            "resource_type": "pixel_buffer",
            "handle_type": "dma_buf",
            "plane_sizes": [allocation_byte_size],
            "plane_offsets": [0u64],
            "plane_strides": [planes.plane_strides_bytes()[0]],
        });
        planes.write_surface_share_fields(&mut check_in_request);
        let (check_in_response, _no_fds_back) =
            self.surface_share_request_with_fds(&check_in_request, &[foreign_dma_buf_fd])?;
        if let Some(check_in_error) = check_in_response
            .get("error")
            .and_then(|value| value.as_str())
        {
            return Err(PyRuntimeError::new_err(format!(
                "the surface-share service refused to adopt the foreign DMA-BUF: \
                 {check_in_error}"
            )));
        }
        check_in_response
            .get("surface_id")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .ok_or_else(|| {
                PyRuntimeError::new_err(
                    "the surface-share service's check_in answered without a surface_id",
                )
            })
    }
}

/// The adoption round trip against a real surface-share service, at the
/// socket seam the Vulkan-free half of `import_dma_buf` rides.
///
/// Provable without a GPU — the fd genuinely crosses via SCM_RIGHTS and the
/// service dups it, so a sized memfd stands in for a DMA-BUF: registration
/// bookkeeping never touches the memory. The Vulkan mapping of an adopted
/// surface is `requires_gpu` and runs on the rig.
#[cfg(test)]
mod foreign_dma_buf_adoption_tests;
