// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib::sdk::rhi::{PixelFormat, TextureFormat};

use super::super::HelperProcessGpuExchangeClient;

/// `VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_SAMPLED_BIT`: a foreign
/// BGRA IOSurface is only ever read.
const FOREIGN_BGRA_IOSURFACE_TEXTURE_VK_IMAGE_USAGE: u32 = 0x01 | 0x04;

impl HelperProcessGpuExchangeClient {
    /// Register a foreign IOSurface with the surface-share service, sending it
    /// a send right to the surface, and return the surface id the service
    /// minted: a BGRA surface as a texture the engine's image copy reads, any
    /// other as a pixel buffer. The caller owes the registration its
    /// [`Self::unregister_foreign_surface`].
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
        let check_in_request = check_in_request_of_a_foreign_iosurface(
            &self.foreign_surface_registration_runtime_id,
            width,
            height,
            pixel_format,
            iosurface.alloc_size() as u64,
        );
        let (check_in_response, _no_ports_back) = self
            .surface_share_mach_connection()?
            .send_request_with_ports(&check_in_request, vec![iosurface_send_right])
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

/// The `check_in` a foreign IOSurface of `pixel_format` registers with.
fn check_in_request_of_a_foreign_iosurface(
    runtime_id: &str,
    width: u32,
    height: u32,
    pixel_format: PixelFormat,
    allocation_byte_size: u64,
) -> serde_json::Value {
    match pixel_format {
        PixelFormat::Bgra32 => serde_json::json!({
            "op": "check_in",
            "runtime_id": runtime_id,
            "width": width,
            "height": height,
            "format": TextureFormat::Bgra8Unorm.wire_name(),
            "resource_type": "texture",
            "vk_image_usage": FOREIGN_BGRA_IOSURFACE_TEXTURE_VK_IMAGE_USAGE,
            "vk_image_allocation_size": allocation_byte_size,
        }),
        _ => serde_json::json!({
            "op": "check_in",
            "runtime_id": runtime_id,
            "width": width,
            "height": height,
            "format": pixel_format.wire_name(),
            "resource_type": "pixel_buffer",
        }),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bgra_iosurface_checks_in_as_a_read_only_bgra_texture() {
        let request =
            check_in_request_of_a_foreign_iosurface("runtime", 64, 32, PixelFormat::Bgra32, 8192);
        assert_eq!(request["resource_type"], "texture");
        assert_eq!(request["format"], TextureFormat::Bgra8Unorm.wire_name());
        assert_eq!(request["vk_image_usage"], 0x05);
        assert_eq!(request["vk_image_allocation_size"], 8192);
    }

    #[test]
    fn an_nv12_iosurface_checks_in_as_a_pixel_buffer() {
        let request = check_in_request_of_a_foreign_iosurface(
            "runtime",
            64,
            32,
            PixelFormat::Nv12VideoRange,
            4096,
        );
        assert_eq!(request["resource_type"], "pixel_buffer");
        assert_eq!(request["format"], PixelFormat::Nv12VideoRange.wire_name());
        assert!(request.get("vk_image_usage").is_none());
    }
}
