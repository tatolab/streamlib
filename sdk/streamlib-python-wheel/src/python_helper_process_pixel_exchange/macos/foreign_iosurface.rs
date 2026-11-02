// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib::sdk::rhi::PixelFormat;

use super::super::HelperProcessGpuExchangeClient;

impl HelperProcessGpuExchangeClient {
    /// Register a foreign IOSurface with the surface-share service, sending it
    /// a send right to the surface, and return the surface id the service minted. The
    /// caller owes the registration its [`Self::unregister_foreign_surface`].
    pub(crate) fn register_foreign_iosurface(
        &self,
        iosurface: &objc2_io_surface::IOSurfaceRef,
        width: u32,
        height: u32,
        pixel_format: PixelFormat,
    ) -> PyResult<String> {
        refuse_an_iosurface_smaller_than_its_stated_extent(iosurface, width, height)?;
        let iosurface_send_right =
            streamlib::sdk::engine::apple_surface_share::create_iosurface_mach_send_right(
                iosurface,
            )
            .map_err(|mint_failure| {
                PyRuntimeError::new_err(format!(
                    "the foreign IOSurface minted no Mach port: {mint_failure}"
                ))
            })?;
        let (check_in_response, _no_ports_back) = self
            .surface_share_mach_connection()?
            .send_request_with_ports(
                &serde_json::json!({
                    "op": "check_in",
                    "runtime_id": self.foreign_surface_registration_runtime_id,
                    "width": width,
                    "height": height,
                    "format": pixel_format.wire_name(),
                    "resource_type": "pixel_buffer",
                }),
                vec![iosurface_send_right],
            )
            .map_err(|request_failure| {
                PyRuntimeError::new_err(format!(
                    "the surface-share check_in of a foreign IOSurface failed: {request_failure}"
                ))
            })?;
        if let Some(check_in_error) = check_in_response
            .get("error")
            .and_then(|value| value.as_str())
        {
            return Err(PyRuntimeError::new_err(format!(
                "the surface-share service refused to adopt the foreign IOSurface: \
                 {check_in_error}"
            )));
        }
        let adopted_surface_id = check_in_response
            .get("surface_id")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .ok_or_else(|| {
                PyRuntimeError::new_err(
                    "the surface-share service's check_in answered without a surface_id",
                )
            })?;
        Ok(adopted_surface_id)
    }
}

/// Refuse a surface whose own extent cannot hold the frame the caller states.
fn refuse_an_iosurface_smaller_than_its_stated_extent(
    iosurface: &objc2_io_surface::IOSurfaceRef,
    width: u32,
    height: u32,
) -> PyResult<()> {
    if width == 0 || height == 0 {
        return Err(PyRuntimeError::new_err(
            "import_iosurface needs a non-zero width and height",
        ));
    }
    if iosurface.width() < width as usize || iosurface.height() < height as usize {
        return Err(PyRuntimeError::new_err(format!(
            "the foreign IOSurface is {}x{}, which cannot hold the stated {width}x{height}",
            iosurface.width(),
            iosurface.height()
        )));
    }
    Ok(())
}
