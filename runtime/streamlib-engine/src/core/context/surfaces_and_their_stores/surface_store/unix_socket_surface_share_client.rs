// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

use crate::core::context::surfaces_and_their_stores::surface_share_wire_verbs::{
    SURFACE_RESOURCE_TYPE_PIXEL_BUFFER, SURFACE_RESOURCE_TYPE_TEXTURE,
};
use crate::core::rhi::PixelBuffer;
use crate::core::rhi::PixelFormat;
use crate::core::{Error, Result};
use crate::host_rhi::HostTextureExt;

use super::registration_payloads::{
    AnnouncedTimelineEdges, PixelBufferShapeOnTheWire, dma_buf_texture_registration_payload,
    exported_plane_wire_handles, exported_timeline_edge_opaque_fd,
    memory_type_index_stated_by_an_opaque_fd_export, opaque_fd_texture_registration_payload,
    pixel_buffer_check_in_payload, pixel_buffer_registration_payload,
    storage_buffer_registration_payload, surface_export_staging_registration_payload,
};
use super::unix_socket_surface_share_replies::{
    TiledDmaBufTextureOfALookupReply, adopt_reply_fds,
    pixel_buffer_imported_from_surface_share_reply, pixel_format_extent_and_planes_of_lookup_reply,
    plane_fds_of_reply, plane_sizes_of_reply,
};
use super::{
    DmaBufRegistrationWithItsPlaneLayout, PixelBufferWithItsPlaneLayout,
    SURFACE_HANDLE_TYPE_DMA_BUF, SurfaceStoreInner, refusal_of_a_registration_answer,
    surface_id_of_check_in_answer,
};

impl SurfaceStoreInner {
    // =========================================================================
    // Linux: Unix socket client
    // =========================================================================

    /// Connect to the surface-share Unix socket.
    pub fn connect(&self) -> Result<()> {
        let stream = std::os::unix::net::UnixStream::connect(&self.service_name).map_err(|e| {
            Error::Configuration(format!(
                "Failed to connect to surface-share socket '{}': {}",
                self.service_name, e
            ))
        })?;

        *self.connection.lock() = Some(stream);

        tracing::info!(
            "SurfaceStore: Connected to surface-share service socket '{}'",
            self.service_name
        );

        Ok(())
    }

    /// Check in a pixel buffer via Unix socket, returning a surface ID.
    pub fn check_in(&self, pixel_buffer: &PixelBuffer) -> Result<String> {
        let exported_planes = exported_plane_wire_handles(pixel_buffer)?;

        let request = pixel_buffer_check_in_payload(
            &self.runtime_id,
            PixelBufferShapeOnTheWire::of(pixel_buffer),
            &exported_planes,
        );

        let response = self.send_surface_share_request_owning_fds(
            "check_in",
            &request,
            exported_planes.plane_fds,
        )?;

        let surface_id = surface_id_of_check_in_answer(&response)?;

        self.cache
            .lock()
            .insert(surface_id.clone(), pixel_buffer.clone());

        tracing::debug!("SurfaceStore: Checked in as '{}'", surface_id);

        Ok(surface_id)
    }

    /// Check `pixel_buffer`'s memory in as a `width` × `height`
    /// `pixel_format` surface laid out as `planes` — the registration a
    /// helper's foreign-allocation adoption makes.
    #[cfg(test)]
    pub(crate) fn check_in_with_a_pixel_plane_layout(
        &self,
        pixel_buffer: &PixelBuffer,
        pixel_format: PixelFormat,
        width: u32,
        height: u32,
        planes: &crate::core::rhi::PixelPlanesWithinOneAllocation,
    ) -> Result<String> {
        let exported_planes = exported_plane_wire_handles(pixel_buffer)?;
        let mut request = pixel_buffer_check_in_payload(
            &self.runtime_id,
            PixelBufferShapeOnTheWire {
                width,
                height,
                format: pixel_format,
            },
            &exported_planes,
        );
        planes.write_surface_share_fields(&mut request);
        let response = self.send_surface_share_request_owning_fds(
            "check_in",
            &request,
            exported_planes.plane_fds,
        )?;
        if let Some(refusal) = response.get("error").and_then(serde_json::Value::as_str) {
            return Err(Error::Configuration(format!("check_in: {refusal}")));
        }
        surface_id_of_check_in_answer(&response)
    }

    /// Register `foreign_dma_buf_fd` the way a helper adopts a foreign
    /// device's buffer: one DMA-BUF holding every plane, at the stated layout.
    #[cfg(test)]
    pub(crate) fn check_in_a_foreign_dma_buf_fd_with_a_pixel_plane_layout(
        &self,
        foreign_dma_buf_fd: OwnedFd,
        allocation_byte_size: u64,
        pixel_format: PixelFormat,
        width: u32,
        height: u32,
        planes: &crate::core::rhi::PixelPlanesWithinOneAllocation,
    ) -> Result<String> {
        let mut request = serde_json::json!({
            "op": "check_in",
            "runtime_id": self.runtime_id,
            "width": width,
            "height": height,
            "format": pixel_format.wire_name(),
            "resource_type": "pixel_buffer",
            "handle_type": SURFACE_HANDLE_TYPE_DMA_BUF,
            "plane_sizes": [allocation_byte_size],
            "plane_offsets": [0u64],
            "plane_strides": [planes.plane_strides_bytes()[0]],
        });
        planes.write_surface_share_fields(&mut request);
        let response = self.send_surface_share_request_owning_fds(
            "check_in",
            &request,
            vec![foreign_dma_buf_fd],
        )?;
        if let Some(refusal) = response.get("error").and_then(serde_json::Value::as_str) {
            return Err(Error::Configuration(format!("check_in: {refusal}")));
        }
        surface_id_of_check_in_answer(&response)
    }

    /// Check out a surface by ID via Unix socket.
    pub fn check_out(&self, surface_id: &str) -> Result<PixelBuffer> {
        if let Some(cached) = self.cache.lock().checked_out_clone(surface_id) {
            return Ok(cached);
        }

        // Cache miss - fetch from the surface-share service
        tracing::debug!(
            "SurfaceStore: Cache miss for '{}', fetching from the surface-share service",
            surface_id
        );

        // `lookup`, not `check_out`: this store is the service owner's own
        // process, and the cached `PixelBuffer` clone it takes below IS its
        // protection — the pool's in-process refcount. A `check_out` here
        // would mint a lease on the host's own long-lived connection that
        // nothing ever releases (the host's `release` op unregisters, it
        // does not unlease), pinning the slot for the runtime's life.
        let request = serde_json::json!({
            "op": "lookup",
            "surface_id": surface_id,
        });

        let connection = self.connection.lock();
        let stream = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        let (response, received_fds) = streamlib_surface_client::send_request_with_fds(
            stream,
            &request,
            &[],
            streamlib_surface_client::MAX_SCM_RIGHTS_FDS,
        )
        .map_err(|e| Error::Configuration(format!("Unix socket check_out failed: {}", e)))?;
        let pixel_buffer = pixel_buffer_imported_from_surface_share_reply(
            "check_out",
            &response,
            adopt_reply_fds(received_fds),
        )?;

        // Cache for future use
        self.cache
            .lock()
            .insert(surface_id.to_string(), pixel_buffer.clone());

        Ok(pixel_buffer)
    }

    /// Send one request over the surface-share socket, passing `fds` with
    /// SCM_RIGHTS under `operation`'s name, and return the service's reply.
    ///
    /// Takes the fds by value because the ordering here is the contract:
    /// the raw descriptors must stay valid across the send, this side's
    /// copies must close before the send's outcome is inspected (the
    /// service dup'd what it needs during the send), and any fd the reply
    /// carries must close on the success path. Every caller goes through
    /// here so those four steps cannot drift apart.
    fn send_surface_share_request_owning_fds(
        &self,
        operation: &str,
        request: &serde_json::Value,
        fds: Vec<OwnedFd>,
    ) -> Result<serde_json::Value> {
        let connection = self.connection.lock();
        let stream = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        let raw_fds: Vec<std::os::unix::io::RawFd> = fds.iter().map(OwnedFd::as_raw_fd).collect();
        let send_result =
            streamlib_surface_client::send_request_with_fds(stream, request, &raw_fds, 0);
        drop(fds);
        let (response, response_fds) = send_result.map_err(|failure| {
            Error::Configuration(format!("Unix socket {operation} failed: {failure}"))
        })?;
        // Neither op returns fds; close any the service attached so a future
        // protocol drift doesn't leak them.
        for fd in &response_fds {
            unsafe { libc::close(*fd) };
        }
        Ok(response)
    }

    /// Send one `register` request and hold the service to its answer.
    fn send_surface_share_registration(
        &self,
        operation: &str,
        request: &serde_json::Value,
        fds: Vec<OwnedFd>,
    ) -> Result<()> {
        let response = self.send_surface_share_request_owning_fds(operation, request, fds)?;
        refusal_of_a_registration_answer(operation, &response)
    }

    /// Register a DEVICE_LOCAL OPAQUE_FD tensor storage buffer under
    /// `surface_id` as a `storage_buffer` surface carrying `shape` and `dtype`.
    ///
    /// Its one plane is the tensor's exact byte size — what a CUDA import
    /// maps — not the allocation's rounded size.
    pub fn register_storage_buffer(
        &self,
        surface_id: &str,
        buffer: &crate::core::rhi::StorageBuffer,
        tensor_layout: &crate::core::rhi::TensorStorageBufferLayout,
    ) -> Result<()> {
        let host_buffer = buffer.host_inner();
        let memory_type_index = memory_type_index_stated_by_an_opaque_fd_export(
            host_buffer.vma_allocation_memory_type_index(),
            surface_id,
            "OPAQUE_FD tensor storage buffer registration",
        )?;
        let exporting_device_uuid = streamlib_consumer_rhi::lowercase_hex_of_vulkan_device_uuid(
            host_buffer.vulkan_device().physical_device_uuid(),
        );
        // SAFETY: the export mints a fresh fd this process owns and has handed
        // to no one else; adopting it closes it exactly once on every path.
        let owned_memory_fd =
            unsafe { OwnedFd::from_raw_fd(host_buffer.export_opaque_fd_memory()?) };
        let request = storage_buffer_registration_payload(
            surface_id,
            &self.runtime_id,
            tensor_layout,
            memory_type_index,
            &exporting_device_uuid,
        );
        self.send_surface_share_registration(
            "register_storage_buffer",
            &request,
            vec![owned_memory_fd],
        )
    }

    /// Register a buffer with the surface-share service via Unix socket.
    ///
    /// The plane sizes travel with the registration because a checkout's
    /// importer derives the row pitch from them; a registration without
    /// sizes checks out as an unusable zero-byte plane.
    pub fn register_buffer(&self, pool_id: &str, pixel_buffer: &PixelBuffer) -> Result<()> {
        let exported_planes = exported_plane_wire_handles(pixel_buffer)?;

        let request = pixel_buffer_registration_payload(
            pool_id,
            &self.runtime_id,
            PixelBufferShapeOnTheWire::of(pixel_buffer),
            &exported_planes,
            None,
        );

        self.send_surface_share_registration("register", &request, exported_planes.plane_fds)?;
        tracing::debug!("SurfaceStore: Registered buffer '{}'", pool_id);
        Ok(())
    }

    /// Register a surface-export staging buffer and, where the device
    /// exports one, the timeline its refills signal, so a helper process can
    /// check them out and reach the staging — importing it into an external device API, or
    /// mapping it, according to the residency it was minted at.
    ///
    /// The staging is one flat OPAQUE_FD allocation, never a pixel
    /// buffer: pool allocations are DMA-BUF-flavoured wherever the device
    /// exports DMA-BUF, external device APIs import OPAQUE_FD, and on NVIDIA
    /// one allocation cannot export both. That is why this cannot go through
    /// [`Self::register_pixel_buffer_with_timeline`], which takes a
    /// [`PixelBuffer`] and its plane handles.
    ///
    /// The refill timeline travels in the `produce_done` slot, and
    /// `consume_done` stays empty. That is the shape, not a compromise:
    /// the host produces the staging's contents and the consumer waits
    /// before reading, which is exactly the `produce_done` edge — and
    /// there is no consumer-side drain for the host to wait on, because
    /// each refill overwrites the staging wholesale. `None` registers the
    /// staging alone, with `produce_done` empty too: on a device that
    /// exports no timeline every copy is retired on the host before the
    /// escalate reply, so the consumer has nothing to wait on.
    pub fn register_surface_export_staging(
        &self,
        surface_id: &str,
        staging_buffer: &crate::vulkan::rhi::HostVulkanBuffer,
        staging_byte_size: u64,
        width: u32,
        height: u32,
        format: PixelFormat,
        exporting_device_uuid: [u8; 16],
        refill_done: Option<&crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
    ) -> Result<()> {
        // Read ahead of the export so a staging with no allocation to
        // state fails before an fd is minted. A conforming OPAQUE_FD
        // import binds the exporter's memory type index
        // (VUID-VkMemoryAllocateInfo-allocationSize-01742) and OPAQUE_FD
        // has no fd-properties query to derive it from, so the consumer
        // has nowhere else to get it.
        let memory_type_index = memory_type_index_stated_by_an_opaque_fd_export(
            staging_buffer.vma_allocation_memory_type_index(),
            surface_id,
            "surface-export staging",
        )?;
        let exported_staging_fd = staging_buffer.export_opaque_fd_memory()?;
        // SAFETY: each export mints a fresh fd this process owns and has
        // handed to no one; adopting it here is what closes it exactly once
        // — including on the early return the timeline export can take.
        let staging_fd = unsafe { OwnedFd::from_raw_fd(exported_staging_fd) };
        let mut registered_fds = vec![staging_fd];
        if let Some(refill_done) = refill_done {
            registered_fds.push(exported_timeline_edge_opaque_fd(
                "register_surface_export_staging",
                "refill_done",
                refill_done,
            )?);
        }

        let request = surface_export_staging_registration_payload(
            surface_id,
            &self.runtime_id,
            staging_byte_size,
            width,
            height,
            format,
            memory_type_index,
            &streamlib_consumer_rhi::lowercase_hex_of_vulkan_device_uuid(exporting_device_uuid),
            refill_done.is_some(),
        );

        self.send_surface_share_registration(
            "register_surface_export_staging",
            &request,
            registered_fds,
        )?;
        tracing::debug!(
            "SurfaceStore: Registered surface-export staging '{}' ({} bytes)",
            surface_id,
            staging_byte_size,
        );
        Ok(())
    }

    /// Register a texture with the surface-share service via Unix socket.
    ///
    /// `timeline` — when `Some`, the host's exportable timeline semaphore is
    /// exported as an OPAQUE_FD and shipped alongside the DMA-BUF FD. The
    /// surface-share service stores the FD; subprocess Vulkan adapters
    /// `check_out` it via [`streamlib_adapter_vulkan::VulkanSurfaceAdapter`]
    /// and import it through `HostVulkanTimelineSemaphore::from_imported_opaque_fd`,
    /// reusing the host adapter's timeline-wait + signal path (#531). `None`
    /// for adapters that don't need explicit Vulkan sync (OpenGL — its
    /// `glFinish` + DMA-BUF kernel-fence semantics carry visibility).
    ///
    /// Dispatches internally on the texture's underlying memory flavor:
    ///
    /// - **DMA-BUF** (the default; tiled `VkImage` with a DRM format
    ///   modifier OR linear `VkBuffer`-backed surface) — exports a
    ///   DMA-BUF FD and publishes with `handle_type: "dma_buf"`.
    /// - **OPAQUE_FD** (`HostVulkanTexture::new_opaque_fd_export` —
    ///   DEVICE_LOCAL `VkImage`, `VK_IMAGE_TILING_OPTIMAL`, no DRM
    ///   modifier, any single-plane format) — exports the OPAQUE_FD
    ///   memory handle and publishes with `handle_type: "opaque_fd"` plus
    ///   the `vk_image_*` round-trip fields (#806) the consumer needs to
    ///   rebuild a matching `VkImageCreateInfo`, usage included.
    pub fn register_texture(
        &self,
        surface_id: &str,
        texture: &crate::core::rhi::Texture,
        produce_done: Option<&crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
        consume_done: Option<&crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
        current_image_layout: streamlib_consumer_rhi::VulkanLayout,
    ) -> Result<()> {
        let is_opaque_fd = texture.vulkan_inner().is_opaque_fd_export();

        // Export the memory FD per the texture's underlying memory
        // flavor. OPAQUE_FD textures have no DMA-BUF export path on
        // NVIDIA (and the call would fail at the driver); DMA-BUF
        // textures have no OPAQUE_FD export path with VMA's
        // per-pool memory configuration.
        let exported_memory_fd = if is_opaque_fd {
            texture.vulkan_inner().export_opaque_fd_memory()?
        } else {
            texture.vulkan_inner().export_dma_buf_fd()?
        };
        // SAFETY: each export mints a fresh fd this process owns and has
        // handed to no one else; adopting it here is what closes it exactly
        // once, on every path out — including the early returns the timeline
        // exports below can take.
        let owned_memory_fd = unsafe { OwnedFd::from_raw_fd(exported_memory_fd) };

        // Optionally export the producer-side `produce_done` and
        // consumer-side `consume_done` timeline-semaphores as
        // OPAQUE_FDs (single-writer-per-edge model — see
        // the single-writer-per-edge timeline rule).
        let produce_done_fd = produce_done
            .map(|edge| exported_timeline_edge_opaque_fd("register_texture", "produce_done", edge))
            .transpose()?;
        let consume_done_fd = consume_done
            .map(|edge| exported_timeline_edge_opaque_fd("register_texture", "consume_done", edge))
            .transpose()?;

        // Per-flavor wire fields. The DMA-BUF path carries the DRM
        // modifier + per-plane layout the EGL / Vulkan import paths
        // need; the OPAQUE_FD path the `vk_image_*` shape an importer
        // rebuilds the image with.
        let request = if is_opaque_fd {
            opaque_fd_texture_registration_payload(
                texture.vulkan_inner(),
                surface_id,
                &self.runtime_id,
                produce_done_fd.is_some(),
                consume_done_fd.is_some(),
                current_image_layout,
            )?
        } else {
            dma_buf_texture_registration_payload(
                texture.vulkan_inner(),
                surface_id,
                &self.runtime_id,
                produce_done_fd.is_some(),
                consume_done_fd.is_some(),
                current_image_layout,
            )
        };

        let mut wire_fds_in_published_order: Vec<OwnedFd> = vec![owned_memory_fd];
        wire_fds_in_published_order.extend(produce_done_fd);
        wire_fds_in_published_order.extend(consume_done_fd);
        self.send_surface_share_registration(
            "register_texture",
            &request,
            wire_fds_in_published_order,
        )?;

        tracing::debug!(
            "SurfaceStore: Registered texture '{}' (produce_done={}, consume_done={})",
            surface_id,
            produce_done.is_some(),
            consume_done.is_some(),
        );
        Ok(())
    }

    /// Register a host-allocated multi-plane pixel buffer with the
    /// surface-share service under an explicit `surface_id`, optionally
    /// shipping the host's exportable timeline semaphore alongside as an
    /// OPAQUE_FD.
    ///
    /// Distinct from [`Self::register_buffer`] (single-plane, no
    /// timeline) and [`Self::register_texture`] (image, with optional
    /// timeline). This is the cpu-readback adapter's registration path:
    /// the host pre-allocates one HOST_VISIBLE / HOST_COHERENT linear
    /// staging `VkBuffer` per plane and an exportable timeline; the
    /// subprocess `check_out`s the bundle once at registration time and
    /// imports each plane via [`streamlib_consumer_rhi::ConsumerVulkanBuffer::from_dma_buf_fds`]
    /// (or its one OPAQUE_FD plane via
    /// [`streamlib_consumer_rhi::ConsumerVulkanBuffer::from_opaque_fd_at_stated_memory_type_index`])
    /// + the timeline via [`streamlib_consumer_rhi::ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd`].
    ///
    /// Per-acquire IPC after registration is a thin trigger that
    /// signals a new timeline value on the same shared timeline; no
    /// further FD passing is needed.
    pub fn register_pixel_buffer_with_timeline(
        &self,
        surface_id: &str,
        pixel_buffer: &PixelBuffer,
        produce_done: Option<&crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
        consume_done: Option<&crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
    ) -> Result<()> {
        let exported_planes = exported_plane_wire_handles(pixel_buffer)?;

        // Export `produce_done` + `consume_done` as OPAQUE_FDs (the
        // single-writer-per-edge pair documented in
        // the single-writer-per-edge timeline rule). The
        // surface-share daemon's wire format peels both trailing FDs
        // in the published order.
        let produce_done_fd = produce_done
            .map(|edge| {
                exported_timeline_edge_opaque_fd(
                    "register_pixel_buffer_with_timeline",
                    "produce_done",
                    edge,
                )
            })
            .transpose()?;
        let consume_done_fd = consume_done
            .map(|edge| {
                exported_timeline_edge_opaque_fd(
                    "register_pixel_buffer_with_timeline",
                    "consume_done",
                    edge,
                )
            })
            .transpose()?;

        let request = pixel_buffer_registration_payload(
            surface_id,
            &self.runtime_id,
            PixelBufferShapeOnTheWire::of(pixel_buffer),
            &exported_planes,
            Some(AnnouncedTimelineEdges {
                has_produce_done_fd: produce_done_fd.is_some(),
                has_consume_done_fd: consume_done_fd.is_some(),
            }),
        );

        let mut wire_fds_in_published_order = exported_planes.plane_fds;
        wire_fds_in_published_order.extend(produce_done_fd);
        wire_fds_in_published_order.extend(consume_done_fd);
        self.send_surface_share_registration(
            "register_pixel_buffer_with_timeline",
            &request,
            wire_fds_in_published_order,
        )?;

        tracing::debug!(
            "SurfaceStore: Registered pixel buffer '{}' ({} plane(s), produce_done={}, consume_done={})",
            surface_id,
            exported_planes.plane_sizes.len(),
            produce_done.is_some(),
            consume_done.is_some(),
        );
        Ok(())
    }

    /// Look up a pixel buffer together with the format, extent and plane
    /// layout its registration states — a tight layout when it states none.
    ///
    /// Bypasses the lookup cache: that cache keeps no geometry, and a caller
    /// of this keeps what it resolves for as long as it reads the surface.
    pub(crate) fn lookup_pixel_buffer_with_its_plane_layout(
        &self,
        surface_id: &str,
    ) -> Result<PixelBufferWithItsPlaneLayout> {
        let (response, reply_fds) = self.lookup_reply_and_its_fds(surface_id)?;
        let pixel_buffer =
            pixel_buffer_imported_from_surface_share_reply("lookup", &response, reply_fds)?;
        let (pixel_format, width, height, planes) = pixel_format_extent_and_planes_of_lookup_reply(
            surface_id,
            &response,
            pixel_buffer.plane_size(0),
        )?;
        Ok(PixelBufferWithItsPlaneLayout {
            pixel_buffer,
            pixel_format,
            width,
            height,
            planes,
        })
    }

    /// Look up a single-plane DMA-BUF registration and hand back its fd with
    /// the format, extent and plane layout it states, importing nothing — for
    /// a caller that reaches the pixels some other way than a Vulkan import.
    pub(crate) fn lookup_dma_buf_registration_without_importing_it(
        &self,
        surface_id: &str,
    ) -> Result<DmaBufRegistrationWithItsPlaneLayout> {
        let (response, reply_fds) = self.lookup_reply_and_its_fds(surface_id)?;
        if let Some(error) = response.get("error").and_then(serde_json::Value::as_str) {
            return Err(Error::Configuration(format!("lookup: {error}")));
        }
        let handle_type = response
            .get("handle_type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(SURFACE_HANDLE_TYPE_DMA_BUF);
        if handle_type != SURFACE_HANDLE_TYPE_DMA_BUF {
            return Err(Error::Configuration(format!(
                "lookup of '{surface_id}': registered as {handle_type:?}, not a DMA-BUF"
            )));
        }
        let mut plane_fds = plane_fds_of_reply("lookup", &response, reply_fds)?;
        if plane_fds.len() != 1 {
            return Err(Error::Configuration(format!(
                "lookup of '{surface_id}': {} plane fds, where one DMA-BUF holds every plane",
                plane_fds.len()
            )));
        }
        let allocation_byte_size = plane_sizes_of_reply(&response, 1)[0];
        if allocation_byte_size == 0 {
            return Err(Error::Configuration(format!(
                "lookup of '{surface_id}': the registration states no allocation size"
            )));
        }
        let (pixel_format, width, height, planes) = pixel_format_extent_and_planes_of_lookup_reply(
            surface_id,
            &response,
            allocation_byte_size,
        )?;
        Ok(DmaBufRegistrationWithItsPlaneLayout {
            dma_buf_fd: plane_fds.remove(0),
            allocation_byte_size,
            pixel_format,
            width,
            height,
            planes,
        })
    }

    /// One `lookup` round trip, with the fds the reply carried adopted.
    fn lookup_reply_and_its_fds(
        &self,
        surface_id: &str,
    ) -> Result<(serde_json::Value, Vec<OwnedFd>)> {
        let request = serde_json::json!({
            "op": "lookup",
            "surface_id": surface_id,
        });
        let connection = self.connection.lock();
        let stream = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;
        let (response, received_fds) = streamlib_surface_client::send_request_with_fds(
            stream,
            &request,
            &[],
            streamlib_surface_client::MAX_SCM_RIGHTS_FDS,
        )
        .map_err(|e| Error::Configuration(format!("Unix socket lookup failed: {}", e)))?;
        Ok((response, adopt_reply_fds(received_fds)))
    }

    /// Lookup a buffer from the surface-share service via Unix socket.
    ///
    /// Checks the host-local cache first so producers that `check_in`'d the
    /// buffer in the same process (e.g. the escalate-on-behalf flow) skip the
    /// per-frame unix-socket round-trip and DMA-BUF re-import.
    pub fn lookup_buffer(&self, pool_id: &str) -> Result<PixelBuffer> {
        if let Some(cached) = self.cache.lock().cached_clone(pool_id) {
            return Ok(cached);
        }

        let request = serde_json::json!({
            "op": "lookup",
            "surface_id": pool_id,
        });

        let connection = self.connection.lock();
        let stream = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        let (response, received_fds) = streamlib_surface_client::send_request_with_fds(
            stream,
            &request,
            &[],
            streamlib_surface_client::MAX_SCM_RIGHTS_FDS,
        )
        .map_err(|e| Error::Configuration(format!("Unix socket lookup failed: {}", e)))?;
        pixel_buffer_imported_from_surface_share_reply(
            "lookup",
            &response,
            adopt_reply_fds(received_fds),
        )
    }

    /// Publish a producer's post-release `VkImageLayout` for the given
    /// `surface_id`. Issued through the surface-share `update_layout`
    /// op (#633): producers call this immediately after their QFOT
    /// release barrier records, so the next consumer's `lookup_texture`
    /// sees the post-release layout instead of the previous one.
    /// Returns `Ok(())` on success; `Err` on socket failure or wire
    /// rejection (e.g., unknown surface_id).
    pub fn update_image_layout(
        &self,
        surface_id: &str,
        layout: streamlib_consumer_rhi::VulkanLayout,
    ) -> Result<()> {
        let request = serde_json::json!({
            "op": "update_layout",
            "surface_id": surface_id,
            "current_image_layout": layout.as_vk().as_raw(),
        });

        let connection = self.connection.lock();
        let stream = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        let (response, response_fds) =
            streamlib_surface_client::send_request_with_fds(stream, &request, &[], 0).map_err(
                |e| Error::Configuration(format!("Unix socket update_layout failed: {}", e)),
            )?;
        for f in &response_fds {
            unsafe { libc::close(*f) };
        }

        if let Some(error) = response.get("error").and_then(|v| v.as_str()) {
            return Err(Error::Configuration(format!("update_layout: {}", error)));
        }

        match response.get("success").and_then(|v| v.as_bool()) {
            Some(true) => Ok(()),
            Some(false) => Err(Error::Configuration(format!(
                "update_layout: surface_id '{}' not registered",
                surface_id
            ))),
            None => Err(Error::Configuration(
                "update_layout: malformed response (missing `success`)".into(),
            )),
        }
    }

    /// Look a texture up from the surface-share service via Unix socket and
    /// import it — through its DRM format modifier when the registration
    /// states a tiled one, as a linear image otherwise. Returns the imported
    /// [`Texture`] paired with the producer's last-published
    /// `current_image_layout`, the source layout of a cross-process
    /// consumer's first acquire barrier.
    pub fn lookup_texture(
        &self,
        surface_id: &str,
    ) -> Result<(
        crate::core::rhi::Texture,
        streamlib_consumer_rhi::VulkanLayout,
    )> {
        let request = serde_json::json!({
            "op": "lookup",
            "surface_id": surface_id,
        });

        let connection = self.connection.lock();
        let stream = connection.as_ref().ok_or_else(|| {
            Error::Configuration("SurfaceStore not connected to surface-share service".into())
        })?;

        let (response, received_fds) = streamlib_surface_client::send_request_with_fds(
            stream,
            &request,
            &[],
            streamlib_surface_client::MAX_SCM_RIGHTS_FDS,
        )
        .map_err(|e| Error::Configuration(format!("Unix socket lookup_texture failed: {}", e)))?;
        let reply_fds = adopt_reply_fds(received_fds);

        if let Some(error) = response
            .get("error")
            .and_then(|v: &serde_json::Value| v.as_str())
        {
            return Err(Error::Configuration(format!("lookup_texture: {}", error)));
        }

        // Refused by name before anything is parsed: a pool slot answers
        // this lookup too, in the pixel-buffer vocabulary, and it is not a
        // texture whatever its format string parses as.
        let resource_type = response
            .get("resource_type")
            .and_then(|v| v.as_str())
            .unwrap_or(SURFACE_RESOURCE_TYPE_PIXEL_BUFFER);
        if resource_type != SURFACE_RESOURCE_TYPE_TEXTURE {
            return Err(Error::Configuration(format!(
                "lookup_texture: {surface_id:?} is registered as a {resource_type}, not a texture"
            )));
        }

        // A texture registration carries one plane; anything past it closes
        // here.
        let mut plane_fds = plane_fds_of_reply("lookup_texture", &response, reply_fds)?.into_iter();
        let dma_buf_fd = plane_fds
            .next()
            .expect("plane_fds_of_reply refuses an empty plane list");
        drop(plane_fds);

        let width = response
            .get("width")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                Error::Configuration("lookup_texture: missing width in response".into())
            })? as u32;

        let height = response
            .get("height")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                Error::Configuration("lookup_texture: missing height in response".into())
            })? as u32;

        let format_str = response
            .get("format")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::Configuration("lookup_texture: missing format in response".into())
            })?;

        use crate::core::rhi::TextureFormat;

        let format = TextureFormat::from_wire_name(format_str).ok_or_else(|| {
            Error::Configuration(format!("lookup_texture: unknown format '{}'", format_str))
        })?;

        let vulkan_device =
            crate::vulkan::rhi::memory_and_buffers::vulkan_buffer::VULKAN_DEVICE_FOR_IMPORT
                .get()
                .ok_or_else(|| {
                    Error::NotSupported(
                        "lookup_texture: HostVulkanDevice not initialized for import".into(),
                    )
                })?;

        let drm_format_modifier = response
            .get("drm_format_modifier")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let vulkan_texture = if drm_format_modifier == 0 {
            let allocation_size =
                (width as u64) * (height as u64) * (format.bytes_per_pixel() as u64);
            crate::vulkan::rhi::HostVulkanTexture::from_dma_buf_fd(
                vulkan_device,
                dma_buf_fd,
                width,
                height,
                format,
                allocation_size,
            )?
        } else {
            let tiled = TiledDmaBufTextureOfALookupReply::of(&response)?;
            crate::vulkan::rhi::HostVulkanTexture::import_dma_buf_with_drm_format_modifier(
                vulkan_device,
                vec![dma_buf_fd],
                &[tiled.plane_offset_bytes],
                &[tiled.plane_stride_bytes],
                drm_format_modifier,
                width,
                height,
                format,
                tiled.allocation_byte_size,
                tiled.usage,
            )?
        };

        // Absent or unparseable is UNDEFINED: the producer published no
        // layout.
        let current_image_layout = response
            .get("current_image_layout")
            .and_then(|v| v.as_i64())
            .map(|raw| streamlib_consumer_rhi::VulkanLayout(raw as i32))
            .unwrap_or(streamlib_consumer_rhi::VulkanLayout::UNDEFINED);

        Ok((
            crate::core::rhi::Texture::from_vulkan(vulkan_texture),
            current_image_layout,
        ))
    }

    /// Send release request to surface-share service via Unix socket.
    pub(super) fn release_from_surface_share_unix(&self, surface_id: &str) -> Result<()> {
        let request = serde_json::json!({
            "op": "release",
            "surface_id": surface_id,
            "runtime_id": self.runtime_id,
        });

        let connection = self.connection.lock();
        let stream = match connection.as_ref() {
            Some(s) => s,
            None => return Ok(()), // Already disconnected
        };

        let _ = streamlib_surface_client::send_request_with_fds(stream, &request, &[], 0);
        Ok(())
    }
}
