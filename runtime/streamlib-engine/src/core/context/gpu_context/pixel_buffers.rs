// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl GpuContext {
    /// Acquire a pixel buffer from the shared pool.
    ///
    /// Pools are cached by (width, height, format) - the first call creates the pool
    /// and pre-allocates buffers, subsequent calls reuse it. Returns (id, buffer) where
    /// the id names this acquisition's frame and can be used with
    /// `get_pixel_buffer()` until the slot's next acquisition retires it.
    ///
    /// If SurfaceStore is initialized, pre-allocated buffers are registered with the surface-share service.
    ///
    /// Never waits on another holder; past the device's memory budget the
    /// allocation is refused by name. The slot is attributed to the stream
    /// this view serves, and at most one slot due to be freed is freed.
    pub fn acquire_pixel_buffer(
        &self,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<(PublishedPixelBufferFrameId, PixelBuffer)> {
        tracing::debug!(
            rhi_op = "acquire_pixel_buffer",
            width,
            height,
            format = ?format,
            "GpuContext::acquire_pixel_buffer"
        );
        self.pixel_buffer_pool_manager.acquire(
            width,
            height,
            format,
            self.loaded_stream_this_view_serves,
        )
    }

    pub(crate) fn create_node_output_pixel_buffer_pool_for_a_native_node(
        &self,
        node_run_id_of_the_owning_node: &str,
    ) -> crate::core::context::NodeOutputPixelBufferPoolOfOneNativeNode {
        crate::core::context::NodeOutputPixelBufferPoolOfOneNativeNode::new(
            node_run_id_of_the_owning_node,
            self.loaded_stream_this_view_serves,
            Arc::clone(&self.pixel_buffer_pool_manager),
            Arc::clone(&self.engines_own_surface_store),
        )
    }

    /// Get a pixel buffer by its published surface id.
    ///
    /// First checks local cache, then falls back to surface-share service
    /// lookup for cross-process sharing. Either path fails for a retired
    /// published frame id: the pool evicts on recycle and the service
    /// refuses, so "found" always means "still that frame".
    pub fn get_pixel_buffer(&self, surface_id: &str) -> Result<PixelBuffer> {
        // Check local cache first
        if let Some(buffer) = self.pixel_buffer_pool_manager.get_from_cache(surface_id) {
            tracing::trace!(
                "GpuContext::get_pixel_buffer: cache hit for '{}'",
                surface_id
            );
            return Ok(buffer);
        }

        // Cache miss - try surface-share service lookup
        tracing::debug!(
            "GpuContext::get_pixel_buffer: cache miss for '{}', trying surface-share service",
            surface_id
        );

        let surface_store = self.surface_store.lock();
        let store = surface_store.as_ref().ok_or_else(|| {
            Error::Configuration(NO_SURFACE_STORE_IS_CONNECTED_TO_THIS_GPU_CONTEXT.into())
        })?;

        let buffer = store.lookup_buffer(surface_id)?;
        self.pixel_buffer_pool_manager
            .cache_pixel_buffer_looked_up_through_the_surface_store(surface_id, buffer.clone());

        Ok(buffer)
    }

    /// Forget the pixel buffer [`Self::get_pixel_buffer`] cached under a
    /// surface-store id, once that id is released; a published frame's entry
    /// stays, the pool's to retire.
    pub(crate) fn forget_pixel_buffer_looked_up_through_the_surface_store(&self, surface_id: &str) {
        self.pixel_buffer_pool_manager
            .forget_pixel_buffer_looked_up_through_the_surface_store(surface_id);
    }

    /// The published frame ids this process's pixel-buffer pools hold in
    /// their cache, one per slot that has published.
    #[cfg(test)]
    pub(crate) fn published_pixel_buffer_frame_ids_cached_in_this_process(&self) -> Vec<String> {
        self.pixel_buffer_pool_manager
            .buffer_cache
            .lock()
            .buffers_by_surface_id
            .keys()
            .filter(|surface_id| PublishedPixelBufferFrameId::parse(surface_id).is_some())
            .cloned()
            .collect()
    }

    /// Resolve a VideoFrame's buffer from its surface_id.
    pub fn resolve_pixel_buffer_by_surface_id(&self, surface_id: &str) -> Result<PixelBuffer> {
        self.get_pixel_buffer(surface_id)
    }

    /// The generation every lease-aware ring this context owns most recently
    /// minted per slot.
    pub(crate) fn lease_aware_pool_minted_frame_generations(
        &self,
    ) -> &LeaseAwarePoolMintedFrameGenerations {
        &self.lease_aware_pool_minted_frame_generations
    }

    /// Refuse a published frame id whose slot has been recycled since.
    ///
    /// An id with no generation suffix passes. A slot one of this context's
    /// own lease-aware rings minted answers from their generation index —
    /// present with or without a lease registry, so the refusal holds in a
    /// context no service was wired into. Anything else falls to the
    /// registry, which fails closed on slots nobody published.
    pub(crate) fn refuse_a_retired_frame_id(&self, surface_id: &str) -> Result<()> {
        let Some((pool_slot, published_generation)) =
            crate::core::rhi::split_pool_slot_and_frame_generation(surface_id)
        else {
            return Ok(());
        };
        if let Some(answer) = self
            .lease_aware_pool_minted_frame_generations
            .refusal_of_a_retired_frame_id(surface_id, pool_slot, published_generation)
        {
            return answer;
        }
        let surface_store = self.surface_store.lock();
        match surface_store
            .as_ref()
            .and_then(SurfaceStore::check_out_leases)
        {
            Some(leases) => leases.refuse_a_retired_frame_id(surface_id),
            None => Ok(()),
        }
    }

    /// Refresh a private texture from a host-visible pixel buffer for cross-
    /// process producers that registered a buffer (not a texture). The texture
    /// is created on first call and reused for subsequent calls under the same
    /// `surface_id`; contents are re-uploaded every time so rotating-pool
    /// producers see fresh frames. The returned registration declares the
    /// terminal layout the upload reports leaving the texture in.
    /// Drop the canvas just cached for `slot_key` when the slot it was drawn
    /// for was freed meanwhile: freeing evicts the slot's published id before
    /// its canvas, so a canvas cached after that eviction would outlive the
    /// slot for the runtime's life.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn evict_a_canvas_cached_after_its_slot_was_freed(
        &self,
        surface_id: &str,
        slot_key: &str,
        cached_canvas: &Texture,
    ) {
        if PublishedPixelBufferFrameId::parse(surface_id).is_none()
            || self
                .pixel_buffer_pool_manager
                .publishes_a_frame_of_the_slot(slot_key)
        {
            return;
        }
        let mut cache = self.buffer_texture_cache.lock();
        if cache.get(slot_key).is_some_and(|still_cached| {
            Arc::ptr_eq(still_cached.vulkan_inner(), cached_canvas.vulkan_inner())
        }) {
            cache.remove(slot_key);
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn refresh_pixel_buffer_texture(
        &self,
        surface_id: &str,
        pixel_buffer: &crate::core::rhi::PixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<TextureRegistration> {
        use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};

        // Refused before touching the cache: a zero extent cannot describe a
        // texture, and callers that pass one (a kernel-dispatch binding
        // resolving a surface id) are saying a buffer-backed surface is not
        // acceptable to them at all. Evicting the slot's cached canvas on the
        // way to an invalid create would break the caller that *can* use it.
        if width == 0 || height == 0 {
            return Err(Error::GpuError(format!(
                "surface {surface_id:?} is buffer-backed, and this caller cannot synthesize a \
                 texture from a pixel buffer"
            )));
        }

        // Get-or-create the cached texture for this surface's slot: the
        // texture is a reusable canvas for the slot, and the per-frame
        // identity check happened before the buffer was resolved.
        let slot_key = pool_slot_key_of_surface_id(surface_id);
        let texture = {
            let mut cache = self.buffer_texture_cache.lock();
            if let Some(existing) = cache.get(slot_key) {
                if existing.width() == width && existing.height() == height {
                    existing.clone()
                } else {
                    cache.remove(slot_key);
                    let desc = TextureDescriptor::new(width, height, TextureFormat::Rgba8Unorm)
                        .with_usage(
                            TextureUsages::COPY_DST
                                | TextureUsages::TEXTURE_BINDING
                                | TextureUsages::STORAGE_BINDING,
                        );
                    let new_texture = self.device.create_texture_local(&desc)?;
                    cache.insert(slot_key.to_string(), new_texture.clone());
                    new_texture
                }
            } else {
                let desc = TextureDescriptor::new(width, height, TextureFormat::Rgba8Unorm)
                    .with_usage(
                        TextureUsages::COPY_DST
                            | TextureUsages::TEXTURE_BINDING
                            | TextureUsages::STORAGE_BINDING,
                    );
                let new_texture = self.device.create_texture_local(&desc)?;
                cache.insert(slot_key.to_string(), new_texture.clone());
                new_texture
            }
        };
        self.evict_a_canvas_cached_after_its_slot_was_freed(surface_id, slot_key, &texture);

        let upload = unsafe {
            self.device.inner.upload_buffer_to_image(
                pixel_buffer.buffer_ref().inner.buffer(),
                texture.vulkan_inner(),
                width,
                height,
            )
        }?;
        Ok(TextureRegistration::new(
            texture,
            upload.final_texture_layout,
        ))
    }

    /// Upload a pixel buffer's contents to a GPU texture and register it in the texture cache.
    ///
    /// Copies the host-visible pixel buffer data to a device-local texture via
    /// vkCmdCopyBufferToImage, then registers the texture so display/encoder
    /// consumers can resolve it by surface_id.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn upload_pixel_buffer_as_texture(
        &self,
        surface_id: &str,
        pixel_buffer: &crate::core::rhi::PixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()> {
        use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};

        // Defense-in-depth for the FullAccess (privileged) tier — #1388 is
        // the first SDK/plugin exposure of this slot. The copy region is
        // tightly packed (buffer_row_length = 0 in
        // record_and_submit_buffer_to_image) and the destination format is
        // hardcoded Rgba8Unorm (4 bytes/pixel), so vkCmdCopyBufferToImage
        // reads exactly width * height * 4 bytes from the source
        // HOST_VISIBLE buffer. A buggy privileged plugin passing oversized
        // dimensions (or a smaller / non-RGBA8 source) would drive a
        // GPU-side out-of-bounds read of the staging buffer —
        // VK_ERROR_DEVICE_LOST (driver corruption) with no error and no
        // panic. Validate the required byte size against the source
        // buffer's actual allocation and return a typed error before submit.
        let required_byte_size = (width as u64)
            .checked_mul(height as u64)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| {
                crate::core::Error::GpuError(format!(
                    "upload_pixel_buffer_as_texture: {width}x{height} Rgba8Unorm copy region \
                     byte size overflows u64"
                ))
            })?;
        let source_byte_size = pixel_buffer.buffer_ref().inner.size();
        if required_byte_size > source_byte_size {
            return Err(crate::core::Error::GpuError(format!(
                "upload_pixel_buffer_as_texture: {width}x{height} Rgba8Unorm copy region requires \
                 {required_byte_size} bytes but the source pixel buffer holds only \
                 {source_byte_size} bytes"
            )));
        }

        let desc = TextureDescriptor::new(width, height, TextureFormat::Rgba8Unorm).with_usage(
            TextureUsages::COPY_DST
                | TextureUsages::TEXTURE_BINDING
                | TextureUsages::STORAGE_BINDING,
        );
        // Same-process texture cache path: nothing outside this process reads
        // it, so it skips the DMA-BUF export pool.
        let texture = self.device.create_texture_local(&desc)?;

        let upload = unsafe {
            self.device.inner.upload_buffer_to_image(
                pixel_buffer.buffer_ref().inner.buffer(),
                texture.vulkan_inner(),
                width,
                height,
            )
        }?;

        self.register_texture_with_layout(surface_id, texture, upload.final_texture_layout);
        Ok(())
    }

    /// Copy a host-visible pixel buffer's contents into an *already-allocated*
    /// device-local texture.
    ///
    /// Counterpart to [`Self::upload_pixel_buffer_as_texture`]: that one
    /// allocates a fresh texture per call (privileged), this one writes
    /// to a texture the caller already owns (sampbox-safe — no
    /// allocation, no descriptor / pipeline construction, just a
    /// `vkCmdCopyBufferToImage` queue submit). The shared command queue
    /// serializes the submit; layout transitions run UNDEFINED →
    /// TRANSFER_DST → the destination's usage-legal terminal layout via
    /// the existing `upload_buffer_to_image` path (content discard on
    /// the UNDEFINED transition is intended — the caller is about to
    /// overwrite the slot's contents anyway).
    ///
    /// When `surface_id` resolves to an entry in the texture cache
    /// (e.g. a ring slot pre-registered via
    /// [`crate::core::context::GpuContextFullAccess::create_texture_ring`])
    /// the registration's `current_layout` is refreshed to that same
    /// terminal layout, so a consumer barriers out of the layout the
    /// image is actually in.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn copy_pixel_buffer_to_texture(
        &self,
        pixel_buffer: &crate::core::rhi::PixelBuffer,
        texture: &Texture,
        surface_id: &str,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let upload = unsafe {
            self.device.inner.upload_buffer_to_image(
                pixel_buffer.buffer_ref().inner.buffer(),
                texture.vulkan_inner(),
                width,
                height,
            )
        }?;
        // Refresh the registration's layout (no-op for unregistered surface_ids).
        if let Some(reg) = self
            .texture_cache
            .lock()
            .get(pool_slot_key_of_surface_id(surface_id))
        {
            reg.update_layout(upload.final_texture_layout);
        }
        Ok(())
    }

    pub(crate) fn free_every_unheld_shared_pool_entry_the_stream_last_acquired(
        &self,
        stream: crate::core::runtime::LoadedStreamTag,
    ) {
        let mut surface_ids_naming_the_streams_entries = self
            .texture_pool
            .flag_every_slot_the_stream_last_acquired(stream);
        surface_ids_naming_the_streams_entries.extend(
            self.pixel_buffer_pool_manager
                .flag_every_slot_the_stream_last_acquired(stream),
        );
        if let Some(check_out_leases) = SurfaceStore::check_out_lease_registry_of_the_engines_own(
            &self.engines_own_surface_store,
        ) {
            match check_out_leases.wait_until_no_check_out_lease_names_any_of(
                &surface_ids_naming_the_streams_entries,
                LONGEST_A_STOPPING_STREAM_WAITS_FOR_THE_CHECK_OUT_LEASES_ON_ITS_POOL_ENTRIES,
            ) {
                Ok(true) => {}
                still_leased_or_unreadable => tracing::debug!(
                    stream = %stream,
                    "a checkout lease on a stopped stream's shared pool entries outlasted {:?} \
                     ({still_leased_or_unreadable:?}); those entries are freed once it is \
                     released",
                    LONGEST_A_STOPPING_STREAM_WAITS_FOR_THE_CHECK_OUT_LEASES_ON_ITS_POOL_ENTRIES
                ),
            }
        }
        let destroyed_texture_pool_slot_count = self
            .texture_pool
            .free_every_unheld_slot_due_to_be_freed_waiting_for_its_gpu_work();
        let destroyed_pixel_buffer_pool_slot_count = self
            .pixel_buffer_pool_manager
            .free_every_unheld_slot_due_to_be_freed_waiting_for_its_gpu_work();
        tracing::debug!(
            stream = %stream,
            destroyed_texture_pool_slot_count,
            destroyed_pixel_buffer_pool_slot_count,
            "freed the shared pool entries a stopped stream last acquired"
        );
    }

    /// Free the shared pool entry behind a helper's one-off pixel buffer as
    /// soon as nothing holds it, without waiting out the idle window.
    pub(crate) fn flag_the_pixel_buffer_pool_slot_of_the_buffer_freed_as_soon_as_unheld(
        &self,
        buffer: &PixelBuffer,
    ) {
        self.pixel_buffer_pool_manager
            .flag_the_slot_of_the_buffer_freed_as_soon_as_unheld(buffer);
    }

    // =========================================================================
    // GPU Blit Operations
    // =========================================================================

    /// Copy pixels between same-format, same-size buffers.
    ///
    /// Uses GPU blit with texture caching for efficient repeated copies.
    pub fn blit_copy(&self, src: &PixelBuffer, dest: &PixelBuffer) -> Result<()> {
        self.blitter.blit_copy(src, dest)
    }

    /// Clear the blitter's texture cache to free GPU memory.
    pub fn clear_blitter_cache(&self) {
        self.blitter.clear_cache();
    }
}
