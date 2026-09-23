// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use streamlib::sdk::rhi::PixelFormat;

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

impl HelperProcessGpuExchangeClient {
    /// Adopt a foreign DMA-BUF fd as a surface this graph can resolve.
    ///
    /// The fd crosses to the surface-share service over SCM_RIGHTS on a
    /// `check_in` (the kernel and the service each dup it — the caller keeps
    /// ownership of the original), the service mints a surface id, and the
    /// checkout of that id lands this process's own mapping of the memory.
    /// The returned surface carries the unregister debt: dropping the last
    /// share removes the service entry the adoption created.
    pub(crate) fn import_foreign_dma_buf(
        self: &Arc<Self>,
        python: Python<'_>,
        foreign_dma_buf_fd: RawFd,
        width: u32,
        height: u32,
        pixel_format: PixelFormat,
        plane_byte_size: u64,
    ) -> PyResult<HelperCheckedOutPixelSurface> {
        if width == 0 || height == 0 {
            return Err(PyValueError::new_err(
                "import_dma_buf needs a non-zero width and height; the fd carries no geometry \
                 of its own",
            ));
        }
        if plane_byte_size == 0 || !plane_byte_size.is_multiple_of(u64::from(height)) {
            return Err(PyValueError::new_err(format!(
                "import_dma_buf byte_size {plane_byte_size} is not a whole number of {height} \
                 rows; every consumer derives the row pitch from it"
            )));
        }
        let bytes_per_row = plane_byte_size / u64::from(height);
        let checked_out = python.detach(|| {
            let (check_in_response, _no_fds_back) = self.surface_share_request_with_fds(
                &serde_json::json!({
                    "op": "check_in",
                    "runtime_id": self.foreign_surface_registration_runtime_id,
                    "width": width,
                    "height": height,
                    "format": pixel_format.wire_name(),
                    "resource_type": "pixel_buffer",
                    "handle_type": "dma_buf",
                    "plane_sizes": [plane_byte_size],
                    "plane_offsets": [0u64],
                    "plane_strides": [bytes_per_row],
                }),
                &[foreign_dma_buf_fd],
            )?;
            if let Some(check_in_error) = check_in_response
                .get("error")
                .and_then(|value| value.as_str())
            {
                return Err(PyRuntimeError::new_err(format!(
                    "the surface-share service refused to adopt the foreign DMA-BUF: \
                     {check_in_error}"
                )));
            }
            let adopted_surface_id: String = check_in_response
                .get("surface_id")
                .and_then(|value| value.as_str())
                .ok_or_else(|| {
                    PyRuntimeError::new_err(
                        "the surface-share service's check_in answered without a surface_id",
                    )
                })?
                .to_string();
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

    /// Remove an adopted surface's registration, closing the service's fd
    /// dups. The one place the `release` op is spelled for adoptions; every
    /// caller is a [`HelperForeignSurfaceUnregisterDebt`].
    fn unregister_foreign_surface(&self, surface_id: &str) -> PyResult<()> {
        let (response, _no_fds) = self.surface_share_request(&serde_json::json!({
            "op": "release",
            "surface_id": surface_id,
            "runtime_id": self.foreign_surface_registration_runtime_id,
        }))?;
        match response.get("success").and_then(|value| value.as_bool()) {
            Some(true) => Ok(()),
            _ => Err(PyRuntimeError::new_err(format!(
                "the surface-share service did not release adopted surface {surface_id:?}"
            ))),
        }
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
