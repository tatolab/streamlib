// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use crate::core::context::surfaces_and_their_stores::surface_check_out_lease_registry::SurfaceCheckOutLeaseRegistry;
use crate::core::context::surfaces_and_their_stores::surface_share_wire_verbs::{
    SURFACE_RESOURCE_TYPE_PIXEL_BUFFER, SURFACE_RESOURCE_TYPE_STORAGE_BUFFER,
    SURFACE_RESOURCE_TYPE_TEXTURE,
};
use crate::core::rhi::PixelBuffer;
use crate::core::{Error, Result};
use crate::host_rhi::HostTextureExt;

use super::{
    IOSurfaceRegistrationWithItsExtent, SurfaceStoreInner, refusal_of_a_registration_answer,
    surface_id_of_check_in_answer,
};

/// How long a connect waits for the service to admit this process.
const SURFACE_SHARE_MACH_CONNECT_HANDSHAKE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(10);

/// A fresh send right to the IOSurface `pixel_buffer`'s memory is, for a
/// registration to move to the service.
fn exported_iosurface_port(
    pixel_buffer: &PixelBuffer,
) -> Result<streamlib_surface_client::OwnedMachSendRight> {
    pixel_buffer
        .buffer_ref()
        .inner
        .export_iosurface_mach_send_right()
}

impl SurfaceStoreInner {
    /// As [`Self::new_reading_check_out_leases`], also sharing the Mach
    /// service's timeline-pair table so a registration with a pair can be
    /// ordered host-side when a helper cannot import it.
    pub(crate) fn new_sharing_the_mach_services_tables(
        service_name: String,
        runtime_id: String,
        check_out_leases: Arc<SurfaceCheckOutLeaseRegistry>,
        cross_process_timeline_pairs: Arc<
            crate::apple::surface_share::CrossProcessTimelinePairsBySurface,
        >,
    ) -> Arc<Self> {
        Self::sharing_the_services_tables(
            service_name,
            runtime_id,
            Some(check_out_leases),
            Some(cross_process_timeline_pairs),
        )
    }

    // =========================================================================
    // macOS: raw Mach client
    // =========================================================================

    /// Connect to the surface-share service's bootstrap name.
    pub(crate) fn connect(&self) -> Result<()> {
        let connection = streamlib_surface_client::SurfaceShareMachServiceConnection::connect(
            &self.service_name,
            SURFACE_SHARE_MACH_CONNECT_HANDSHAKE_TIMEOUT,
        )
        .map_err(|e| {
            Error::Configuration(format!(
                "Failed to connect to surface-share Mach service '{}': {}",
                self.service_name, e
            ))
        })?;
        *self.connection.lock() = Some(connection);
        tracing::info!(
            "SurfaceStore: Connected to surface-share Mach service '{}'",
            self.service_name
        );
        Ok(())
    }

    /// Check in a pixel buffer's IOSurface, returning the id the service
    /// minted for it.
    pub(crate) fn check_in(&self, pixel_buffer: &PixelBuffer) -> Result<String> {
        let request = serde_json::json!({
            "op": "check_in",
            "runtime_id": self.runtime_id,
            "width": pixel_buffer.width,
            "height": pixel_buffer.height,
            "format": pixel_buffer.format().wire_name(),
            "resource_type": SURFACE_RESOURCE_TYPE_PIXEL_BUFFER,
        });
        let (response, _) = self.send_surface_share_mach_request(
            "check_in",
            &request,
            vec![exported_iosurface_port(pixel_buffer)?],
        )?;
        let surface_id = surface_id_of_check_in_answer(&response)?;
        self.cache
            .lock()
            .insert(surface_id.clone(), pixel_buffer.clone());
        tracing::debug!("SurfaceStore: Checked in as '{}'", surface_id);
        Ok(surface_id)
    }

    /// Resolve a surface id to a pixel buffer over its IOSurface, caching it.
    ///
    /// `lookup`, not `check_out`, for the same reason as on Linux: this store
    /// is the service owner's own process, and the cached clone is its claim.
    pub(crate) fn check_out(&self, surface_id: &str) -> Result<PixelBuffer> {
        if let Some(cached) = self.cache.lock().checked_out_clone(surface_id) {
            return Ok(cached);
        }
        let pixel_buffer = self.import_looked_up_iosurface("check_out", surface_id)?;
        self.cache
            .lock()
            .insert(surface_id.to_string(), pixel_buffer.clone());
        Ok(pixel_buffer)
    }

    /// Register a pool slot's IOSurface under `pool_id`.
    pub(crate) fn register_buffer(&self, pool_id: &str, pixel_buffer: &PixelBuffer) -> Result<()> {
        self.send_pixel_buffer_registration(pool_id, pixel_buffer, None)?;
        tracing::debug!("SurfaceStore: Registered buffer '{}'", pool_id);
        Ok(())
    }

    /// Register a pool slot's IOSurface under `surface_id` with its timeline
    /// pair. The pair's shared events cross beside the surface; when either
    /// will not export, the registration crosses without them and the pair
    /// orders host-side.
    pub(crate) fn register_pixel_buffer_with_timeline_pair(
        &self,
        surface_id: &str,
        pixel_buffer: &PixelBuffer,
        timeline_pair: &Arc<crate::apple::surface_share::CrossProcessTimelinePair>,
    ) -> Result<()> {
        let cross_process_timeline_pairs =
            self.cross_process_timeline_pairs.as_ref().ok_or_else(|| {
                Error::Configuration(format!(
                    "register_pixel_buffer_with_timeline_pair('{surface_id}'): this store shares \
                     no timeline-pair table with a surface-share service"
                ))
            })?;
        // Recorded only once the service accepted the id: a refused duplicate
        // must not displace the live registration's pair.
        self.send_pixel_buffer_registration(surface_id, pixel_buffer, Some(timeline_pair))?;
        cross_process_timeline_pairs.insert(surface_id, Arc::clone(timeline_pair));
        tracing::debug!(
            "SurfaceStore: Registered buffer '{}' with its timeline pair (host-side ordering: {})",
            surface_id,
            timeline_pair.orders_host_side()
        );
        Ok(())
    }

    /// Send one `register` for a pool slot's IOSurface, with its timeline
    /// pair's shared events after it when there is a pair and it exports.
    fn send_pixel_buffer_registration(
        &self,
        surface_id: &str,
        pixel_buffer: &PixelBuffer,
        timeline_pair: Option<&crate::apple::surface_share::CrossProcessTimelinePair>,
    ) -> Result<()> {
        let mut ports = vec![exported_iosurface_port(pixel_buffer)?];
        if let Some(timeline_pair) = timeline_pair {
            timeline_pair.append_exported_send_rights_to(&mut ports);
        }
        self.send_iosurface_registration(
            ports,
            serde_json::json!({
                "surface_id": surface_id,
                "width": pixel_buffer.width,
                "height": pixel_buffer.height,
                "format": pixel_buffer.format().wire_name(),
                "resource_type": SURFACE_RESOURCE_TYPE_PIXEL_BUFFER,
            }),
        )
    }

    /// Register an IOSurface-backed tensor storage buffer under `surface_id`
    /// as a `storage_buffer` surface carrying `shape` and `dtype`. No timeline
    /// pair crosses: a tensor's writes are ordered by its writer's queue
    /// drain, as on Linux.
    pub(crate) fn register_storage_buffer(
        &self,
        surface_id: &str,
        buffer: &crate::core::rhi::StorageBuffer,
        tensor_layout: &crate::core::rhi::TensorStorageBufferLayout,
    ) -> Result<()> {
        let mut registration = serde_json::json!({
            "surface_id": surface_id,
            "resource_type": SURFACE_RESOURCE_TYPE_STORAGE_BUFFER,
        });
        tensor_layout.write_surface_share_fields(&mut registration);
        self.send_iosurface_registration(
            vec![buffer.host_inner().export_iosurface_mach_send_right()?],
            registration,
        )
    }

    /// Register an IOSurface-backed texture under `surface_id` whole: its
    /// surface, its image recipe, the layout it is in, and its timeline pair.
    /// Refused when the pair will not export: a helper imports a texture only
    /// with both edges, since a reader outside the pair is unsynchronised.
    pub(crate) fn register_texture_with_timeline_pair(
        &self,
        surface_id: &str,
        texture: &crate::core::rhi::Texture,
        timeline_pair: &Arc<crate::apple::surface_share::CrossProcessTimelinePair>,
        current_image_layout: streamlib_consumer_rhi::VulkanLayout,
    ) -> Result<()> {
        use crate::vulkan::rhi::VulkanTextureLike as _;

        let cross_process_timeline_pairs =
            self.cross_process_timeline_pairs.as_ref().ok_or_else(|| {
                Error::Configuration(format!(
                    "register_texture_with_timeline_pair('{surface_id}'): this store shares no \
                     timeline-pair table with a surface-share service"
                ))
            })?;
        let image = texture.vulkan_inner();
        let mut registration = serde_json::json!({
            "surface_id": surface_id,
            "width": texture.width(),
            "height": texture.height(),
            "format": texture.format().wire_name(),
            "resource_type": SURFACE_RESOURCE_TYPE_TEXTURE,
            "current_image_layout": current_image_layout.as_vk().as_raw(),
        });
        if let Some(registration_fields) = registration.as_object_mut() {
            use crate::core::context::surfaces_and_their_stores::surface_share_wire_verbs as wire;
            wire::VkImageCreateInfoFields {
                vk_image_type: wire::VK_IMAGE_TYPE_DEFAULT,
                vk_image_mip_levels: wire::VK_IMAGE_MIP_LEVELS_DEFAULT,
                vk_image_array_layers: wire::VK_IMAGE_ARRAY_LAYERS_DEFAULT,
                vk_image_samples: wire::VK_IMAGE_SAMPLES_DEFAULT,
                vk_image_tiling: image.vk_image_tiling().as_raw(),
                vk_image_usage: image.vk_image_usage_flags().bits(),
                vk_image_allocation_size: image.vk_memory_size(),
            }
            .insert_into_wire_fields(registration_fields);
        }
        let (produce_done_port, consume_done_port) = timeline_pair
            .exported_mach_send_rights()
            .map_err(|refusal| {
                Error::NotSupported(format!(
                    "register_texture_with_timeline_pair('{surface_id}'): the texture's timeline \
                     pair will not export as shared events ({refusal}), and a helper imports a \
                     texture only with both edges"
                ))
            })?;
        self.send_iosurface_registration(
            vec![
                image.export_iosurface_mach_send_right()?,
                produce_done_port,
                consume_done_port,
            ],
            registration,
        )?;
        // Recorded only once the service accepted the id: a refused duplicate
        // must not displace the live registration's pair.
        cross_process_timeline_pairs.insert(surface_id, Arc::clone(timeline_pair));
        tracing::debug!(
            "SurfaceStore: Registered texture '{}' with its timeline pair",
            surface_id
        );
        Ok(())
    }

    /// Send one `register` of `registration`'s fields with `ports` — the
    /// IOSurface's first, then, when the registration carries its timeline
    /// pair, the pair's two shared events.
    fn send_iosurface_registration(
        &self,
        ports: Vec<streamlib_surface_client::OwnedMachSendRight>,
        mut registration: serde_json::Value,
    ) -> Result<()> {
        let carries_timeline_pair = ports.len() > 1;
        if let Some(registration_fields) = registration.as_object_mut() {
            registration_fields.insert("op".into(), "register".into());
            registration_fields.insert("runtime_id".into(), self.runtime_id.clone().into());
            registration_fields.insert(
                streamlib_surface_client::SURFACE_SHARE_HAS_PRODUCE_DONE_PORT.into(),
                carries_timeline_pair.into(),
            );
            registration_fields.insert(
                streamlib_surface_client::SURFACE_SHARE_HAS_CONSUME_DONE_PORT.into(),
                carries_timeline_pair.into(),
            );
        }
        let (response, _) =
            self.send_surface_share_mach_request("register", &registration, ports)?;
        refusal_of_a_registration_answer("register", &response)
    }

    /// Resolve a texture another runtime registered to an image over its
    /// IOSurface, with the layout it was last published in.
    pub(crate) fn lookup_texture(
        &self,
        surface_id: &str,
    ) -> Result<(
        crate::core::rhi::Texture,
        streamlib_consumer_rhi::VulkanLayout,
    )> {
        const OPERATION: &str = "lookup_texture";
        let request = serde_json::json!({"op": "lookup", "surface_id": surface_id});
        let (answer, reply_ports) =
            self.send_surface_share_mach_request(OPERATION, &request, Vec::new())?;
        if answer
            .get("resource_type")
            .and_then(serde_json::Value::as_str)
            != Some(SURFACE_RESOURCE_TYPE_TEXTURE)
        {
            return Err(Error::Configuration(format!(
                "{OPERATION}: '{surface_id}' is registered as {}, not a texture",
                answer["resource_type"]
            )));
        }
        // Ports after the IOSurface's are the pair's shared events, which the
        // registering runtime orders; they are released here.
        let iosurface_port = reply_ports.into_iter().next().ok_or_else(|| {
            Error::Configuration(format!(
                "{OPERATION}: the answer for '{surface_id}' carried no IOSurface port"
            ))
        })?;
        let iosurface =
            objc2_io_surface::IOSurfaceRef::lookup_from_mach_port(iosurface_port.as_raw_name())
                .ok_or_else(|| {
                    Error::Configuration(format!(
                        "{OPERATION}: the port answered for '{surface_id}' names no IOSurface"
                    ))
                })?;
        let format = answer
            .get("format")
            .and_then(serde_json::Value::as_str)
            .and_then(crate::core::rhi::TextureFormat::from_wire_name)
            .ok_or_else(|| {
                Error::Configuration(format!(
                    "{OPERATION}: the answer for '{surface_id}' names no texture format"
                ))
            })?;
        let recipe =
            crate::core::context::surfaces_and_their_stores::surface_share_wire_verbs::parse_vk_image_create_info_fields(
                &answer,
            );
        let vulkan_device =
            crate::vulkan::rhi::memory_and_buffers::vulkan_buffer::VULKAN_DEVICE_FOR_IMPORT
                .get()
                .ok_or_else(|| {
                    Error::NotSupported(format!(
                        "{OPERATION}: HostVulkanDevice not initialized for import"
                    ))
                })?;
        let vulkan_texture = crate::vulkan::rhi::HostVulkanTexture::from_iosurface(
            vulkan_device,
            iosurface,
            format,
            streamlib_consumer_rhi::VulkanImageUsage(recipe.vk_image_usage),
        )?;
        let current_image_layout =
            crate::core::context::surfaces_and_their_stores::surface_share_wire_verbs::stated_current_image_layout(&answer)
                .map(streamlib_consumer_rhi::VulkanLayout)
                .unwrap_or(streamlib_consumer_rhi::VulkanLayout::UNDEFINED);
        Ok((
            crate::core::rhi::Texture::from_vulkan(vulkan_texture),
            current_image_layout,
        ))
    }

    /// Publish the layout a registered texture was left in, so the next
    /// holder transitions out of the layout the image is actually in.
    pub(crate) fn update_image_layout(
        &self,
        surface_id: &str,
        layout: streamlib_consumer_rhi::VulkanLayout,
    ) -> Result<()> {
        let request = serde_json::json!({
            "op": "update_layout",
            "surface_id": surface_id,
            "current_image_layout": layout.as_vk().as_raw(),
        });
        let (response, _) =
            self.send_surface_share_mach_request("update_layout", &request, Vec::new())?;
        match response.get("success").and_then(serde_json::Value::as_bool) {
            Some(true) => Ok(()),
            _ => Err(Error::Configuration(format!(
                "update_layout: no texture is registered as '{surface_id}'"
            ))),
        }
    }

    /// Resolve a registered pool slot to a pixel buffer over its IOSurface.
    pub(crate) fn lookup_buffer(&self, pool_id: &str) -> Result<PixelBuffer> {
        if let Some(cached) = self.cache.lock().cached_clone(pool_id) {
            return Ok(cached);
        }
        self.import_looked_up_iosurface("lookup", pool_id)
    }

    /// `lookup` `surface_id` and import the IOSurface the answer's port
    /// names, zero-copy.
    fn import_looked_up_iosurface(&self, operation: &str, surface_id: &str) -> Result<PixelBuffer> {
        use crate::core::rhi::{PixelFormat, RhiExternalHandle, RhiPixelBufferImport};

        let request = serde_json::json!({"op": "lookup", "surface_id": surface_id});
        let (answer, reply_ports) =
            self.send_surface_share_mach_request(operation, &request, Vec::new())?;
        // Any timeline ports after the IOSurface's are the engine's own
        // timelines, which this process already holds; they are released here.
        let Some(iosurface_port) = reply_ports.into_iter().next() else {
            return Err(Error::Configuration(format!(
                "{operation}: the answer for '{surface_id}' carried no IOSurface port"
            )));
        };
        let stated_u32 = |key: &str| {
            answer
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(0)
        };
        let format = answer
            .get("format")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("the answer for '{surface_id}' names no format"))
            .and_then(PixelFormat::parse_wire_name)
            .map_err(|unreadable| Error::Configuration(format!("{operation}: {unreadable}")))?;
        PixelBuffer::from_external_handle(
            RhiExternalHandle::IOSurfaceMachPort {
                port: iosurface_port.into_raw_name(),
            },
            stated_u32("width"),
            stated_u32("height"),
            format,
        )
    }

    /// Look up a pixel-buffer registration and hand back the IOSurface its
    /// port names, with the format and extent it states, importing nothing —
    /// for a caller that reaches the surface's pages itself.
    pub(crate) fn lookup_iosurface_registration_without_importing_it(
        &self,
        surface_id: &str,
    ) -> Result<IOSurfaceRegistrationWithItsExtent> {
        use crate::core::rhi::PixelFormat;

        let request = serde_json::json!({"op": "lookup", "surface_id": surface_id});
        let (answer, reply_ports) =
            self.send_surface_share_mach_request("lookup", &request, Vec::new())?;
        let Some(iosurface_port) = reply_ports.into_iter().next() else {
            return Err(Error::Configuration(format!(
                "lookup: the answer for '{surface_id}' carried no IOSurface port"
            )));
        };
        let iosurface =
            objc2_io_surface::IOSurfaceRef::lookup_from_mach_port(iosurface_port.as_raw_name())
                .ok_or_else(|| {
                    Error::Configuration(format!(
                        "lookup: the port answered for '{surface_id}' names no IOSurface"
                    ))
                })?;
        let stated_u32 = |key: &str| {
            answer
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| {
                    Error::Configuration(format!(
                        "lookup: the answer for '{surface_id}' states no {key}"
                    ))
                })
        };
        let pixel_format = answer
            .get("format")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("the answer for '{surface_id}' names no format"))
            .and_then(PixelFormat::parse_wire_name)
            .map_err(|unreadable| Error::Configuration(format!("lookup: {unreadable}")))?;
        Ok(IOSurfaceRegistrationWithItsExtent {
            iosurface,
            pixel_format,
            width: stated_u32("width")?,
            height: stated_u32("height")?,
        })
    }

    /// Send one request under `operation`'s name and hold the service to its
    /// answer, returning it with the ports it carries.
    fn send_surface_share_mach_request(
        &self,
        operation: &str,
        request: &serde_json::Value,
        ports: Vec<streamlib_surface_client::OwnedMachSendRight>,
    ) -> Result<(
        serde_json::Value,
        Vec<streamlib_surface_client::OwnedMachSendRight>,
    )> {
        let connection = self.connection.lock();
        let connection = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;
        let (response, reply_ports) =
            connection
                .send_request_with_ports(request, ports)
                .map_err(|failure| {
                    Error::Configuration(format!(
                        "Mach surface-share {operation} failed: {failure}"
                    ))
                })?;
        if let Some(error) = response.get("error").and_then(serde_json::Value::as_str) {
            return Err(Error::Configuration(format!("{operation}: {error}")));
        }
        Ok((response, reply_ports))
    }

    /// Best-effort `release`; with no connection there is nothing to release,
    /// because the service already let go when the connection closed.
    pub(super) fn release_from_surface_share_mach(&self, surface_id: &str) -> Result<()> {
        let request = serde_json::json!({
            "op": "release",
            "surface_id": surface_id,
            "runtime_id": self.runtime_id,
        });
        if let Some(connection) = self.connection.lock().as_ref() {
            let _ = connection.send_request_with_ports(&request, Vec::new());
        }
        Ok(())
    }
}
