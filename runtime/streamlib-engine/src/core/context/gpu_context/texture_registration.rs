// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl GpuContext {
    /// Register a texture in the same-process texture cache.
    ///
    /// On Linux the texture is registered with `VulkanLayout::UNDEFINED`
    /// as its initial layout — callers that know the texture's actual
    /// post-allocation layout (e.g. camera ring textures left in
    /// `SHADER_READ_ONLY_OPTIMAL` after compute) should use
    /// [`Self::register_texture_with_layout`] instead so consumers
    /// reaching the texture via [`Self::resolve_texture_registration_by_surface_id`]
    /// can issue correct layout transitions.
    pub fn register_texture(&self, id: &str, texture: Texture) {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let registration = TextureRegistration::new(texture, VulkanLayout::UNDEFINED);
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let registration = TextureRegistration::new(texture);
        let mut cache = self.texture_cache.lock();
        cache.insert(pool_slot_key_of_surface_id(id).to_string(), registration);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn register_texture_with_layout(
        &self,
        id: &str,
        texture: Texture,
        initial_layout: VulkanLayout,
    ) {
        let registration = TextureRegistration::new(texture, initial_layout);
        let mut cache = self.texture_cache.lock();
        // Keyed by the slot so a producer re-registering per published
        // frame replaces one entry instead of growing one per frame; the
        // published id's generation is checked at resolve time instead.
        cache.insert(pool_slot_key_of_surface_id(id).to_string(), registration);
    }

    /// Remove a `surface_id` from the same-process texture cache.
    ///
    /// Idempotent — missing entries are a no-op. Producers that
    /// pre-register textures with a known lifetime (e.g.
    /// [`TextureRing`](crate::core::context::TextureRing)) call this on
    /// teardown so the cache doesn't outlive the underlying texture.
    pub fn unregister_texture(&self, id: &str) {
        // The guard is scoped, not held across the eviction below: that call
        // takes the staging map and then the surface-share socket, so holding
        // this one through it would couple three locks into an order nothing
        // else needs. The two removals were never atomic together anyway.
        self.texture_cache
            .lock()
            .remove(pool_slot_key_of_surface_id(id));
        #[cfg(target_os = "linux")]
        self.evict_surface_export_stagings(id);
    }

    /// Register a tensor storage buffer under `id` in the parent-wide map.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn register_storage_buffer_in_the_parent_wide_map(
        &self,
        id: &str,
        tensor_storage_buffer: crate::core::rhi::StorageBuffer,
    ) {
        self.storage_buffer_registrations.lock().insert(
            pool_slot_key_of_surface_id(id).to_string(),
            tensor_storage_buffer,
        );
    }

    /// Remove `id` from the parent-wide storage buffer map. Idempotent.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn unregister_storage_buffer_from_the_parent_wide_map(&self, id: &str) {
        self.storage_buffer_registrations
            .lock()
            .remove(pool_slot_key_of_surface_id(id));
    }

    /// The tensor storage buffer the parent-wide map holds under
    /// `surface_id`, refusing a published frame id whose slot has been
    /// recycled since.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn resolve_storage_buffer_from_the_parent_wide_map_by_surface_id(
        &self,
        surface_id: &str,
    ) -> Result<crate::core::rhi::StorageBuffer> {
        self.refuse_a_retired_frame_id(surface_id)?;
        self.storage_buffer_registrations
            .lock()
            .get(pool_slot_key_of_surface_id(surface_id))
            .cloned()
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "no tensor storage buffer is registered under surface id {surface_id:?}"
                ))
            })
    }

    /// Acquire this process's registered texture for `surface_id` back from
    /// the external queue family when a helper that held its slot checked out
    /// released it there — a module drawing on its own device into this
    /// process's output pool — so every same-process reader sees the layout
    /// the helper left it in. A no-op when no such release is outstanding.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn acquire_the_registered_texture_a_check_out_holder_released(
        &self,
        surface_id: &str,
    ) -> Result<()> {
        let Some(check_out_leases) = SurfaceStore::check_out_lease_registry_of_the_engines_own(
            &self.engines_own_surface_store,
        ) else {
            return Ok(());
        };
        let texture_cache = self.texture_cache.lock();
        let Some(registration) = texture_cache.get(pool_slot_key_of_surface_id(surface_id)) else {
            return Ok(());
        };
        let Some(layout_the_helper_released_in) = check_out_leases
            .take_image_layout_a_check_out_holder_released_in(surface_id)?
            .map(VulkanLayout)
            .filter(|layout| *layout != VulkanLayout::UNDEFINED)
        else {
            return Ok(());
        };
        if let Some(image) = registration.texture().vulkan_inner().image() {
            self.device
                .inner
                .acquire_from_foreign(image, layout_the_helper_released_in.as_vk())?;
        }
        registration.update_layout(layout_the_helper_released_in);
        Ok(())
    }

    /// The texture a producer registered of its own under `surface_id`
    /// in this process, if any.
    ///
    /// Deliberately not [`Self::resolve_texture_registration_by_surface_id`]:
    /// that call's Path 3 synthesizes a texture *from* the pooled
    /// backing, so a surface with no producer texture at all would still
    /// answer yes. Only a real registration distinguishes a frame its
    /// producer still owns from one whose only backing is its pool
    /// member. Same-process only — a producer that registered a texture
    /// with the surface-share service alone is invisible here, which no
    /// producer in this tree is: cross-process registrations mint their
    /// own handle id rather than a pool id.
    pub(crate) fn producer_registered_texture_for_surface_id(
        &self,
        surface_id: &str,
    ) -> Option<TextureRegistration> {
        self.texture_cache
            .lock()
            .get(pool_slot_key_of_surface_id(surface_id))
            .cloned()
    }

    /// The pooled allocation this process holds for `surface_id`, if any
    /// — the pool's own cache, with no surface-share round trip.
    ///
    /// For a published frame id this answers only while the frame is
    /// current: the pool owns the entry and evicts it on recycle. Ids with
    /// no generation ([`Self::get_pixel_buffer`] caches their successful
    /// cross-process lookups here) answer from the second resolution on.
    pub(crate) fn pooled_backing_held_in_this_process(
        &self,
        surface_id: &str,
    ) -> Option<PixelBuffer> {
        self.pixel_buffer_pool_manager.get_from_cache(surface_id)
    }

    /// Refresh the registration's `current_layout` for a given
    /// `surface_id`. No-op if the surface_id isn't in the cache.
    /// Used by producers after a layout transition (e.g.
    /// [`TextureRing`](crate::core::context::TextureRing)'s per-frame
    /// copy ends in `SHADER_READ_ONLY_OPTIMAL`).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn update_texture_registration_layout(&self, id: &str, layout: VulkanLayout) {
        if let Some(reg) = self
            .texture_cache
            .lock()
            .get(pool_slot_key_of_surface_id(id))
        {
            reg.update_layout(layout);
        }
    }

    /// Resolve a VideoFrame's full registration record (texture + layout).
    ///
    /// Same lookup path as [`Self::resolve_texture_by_surface_id`] but
    /// returns the registration so consumers can read `current_layout`
    /// for barrier-source correctness.
    ///
    /// Path 2 (cross-process DMA-BUF VkImage import) reads the
    /// producer's last-published `VkImageLayout` from the surface-share
    /// IPC (#633). The consumer feeds this into the source layout of
    /// its first QFOT acquire barrier. Surfaces registered without a
    /// declared layout default to `UNDEFINED` (back-compat —
    /// content-discard permitted on the consumer's first transition).
    ///
    /// Path 3 (pixel buffer fallback) declares whatever
    /// terminal layout the upload reports leaving the host-owned
    /// texture in — `SHADER_READ_ONLY_OPTIMAL` for the sampled-capable
    /// texture that path allocates.
    pub fn resolve_texture_registration_by_surface_id(
        &self,
        surface_id: &str,
        #[cfg_attr(
            not(any(target_os = "linux", target_os = "macos")),
            allow(unused_variables, reason = "only the Linux and macOS builds read it")
        )]
        texture_layout: Option<i32>,
        #[cfg_attr(
            not(any(target_os = "linux", target_os = "macos")),
            allow(unused_variables, reason = "only the Linux and macOS builds read it")
        )]
        width: u32,
        #[cfg_attr(
            not(any(target_os = "linux", target_os = "macos")),
            allow(unused_variables, reason = "only the Linux and macOS builds read it")
        )]
        height: u32,
    ) -> Result<TextureRegistration> {
        // A retired published frame id resolves to an error, not to the
        // slot's current pixels — every path below serves per-slot backings,
        // so the frame-vs-slot distinction lives here.
        self.refuse_a_retired_frame_id(surface_id)?;

        // Path 1: same-process texture cache (fastest).
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        self.acquire_the_registered_texture_a_check_out_holder_released(surface_id)?;
        if let Some(reg) = self
            .texture_cache
            .lock()
            .get(pool_slot_key_of_surface_id(surface_id))
        {
            return Ok(reg.clone());
        }

        // Path 2: cross-process VkImage import via the surface-share service —
        // a DMA-BUF on Linux, an IOSurface on macOS.
        // Synthesized registration is not cached — Path 2 reimports per-call by
        // design, and caching would defeat that.
        //
        // QFOT acquire step (#633): the consumer-side VkImage was just
        // created with `initialLayout = UNDEFINED`. The producer's
        // post-release `VkImageLayout` is sourced from (priority order):
        //   1. `VideoFrame.texture_layout` — per-frame override for
        //      producers that vary layout per frame.
        //   2. The surface-share IPC's per-surface `current_image_layout`
        //      — published at registration via `register_texture` and
        //      refreshed via `update_image_layout` after each producer
        //      release.
        // When the resolved layout is non-UNDEFINED, run a one-shot
        // QFOT acquire on the host queue. `acquire_from_foreign` uses
        // `VK_QUEUE_FAMILY_EXTERNAL` (core Vulkan 1.1, always
        // available) for the src family, and chains
        // `VkExternalMemoryAcquireUnmodifiedEXT` so producer-side
        // content survives the transfer when the optional
        // `VK_EXT_external_memory_acquire_unmodified` extension is
        // enabled. When that extension is missing (NVIDIA Linux
        // today and per the current driver roadmap), the helper falls
        // back to a bridging UNDEFINED → resolved_layout transition
        // (content-discard permitted by spec but preserved in
        // practice on every modern Linux Vulkan driver). Either way
        // the consumer-side tracker ends up at the resolved layout so
        // subsequent consumer barriers (`oldLayout = resolved →
        // target`) are validation-clean per
        // VUID-VkImageMemoryBarrier-oldLayout-01197.
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let surface_store = self.surface_store.lock();
            if let Some(store) = surface_store.as_ref()
                && let Ok((texture, ipc_layout)) = store.lookup_texture(surface_id)
            {
                let resolved_layout = texture_layout.map(VulkanLayout).unwrap_or(ipc_layout);
                if resolved_layout != VulkanLayout::UNDEFINED
                    && let Some(image) = texture.vulkan_inner().image()
                {
                    self.device
                        .inner
                        .acquire_from_foreign(image, resolved_layout.as_vk())?;
                }
                return Ok(TextureRegistration::new(texture, resolved_layout));
            }
        }

        // Path 3: pixel buffer fallback — refresh a private host-owned
        // texture from the latest buffer contents. The cache is separate
        // from `texture_cache` because a pool slot serves a new frame every
        // cycle and a cache hit on stale contents would silently render the
        // previous one.
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            // Same-process pool first: a producer that published only a
            // pixel buffer (no texture registration) resolves through the
            // pool's local cache without any socket round-trip — the
            // cross-process store can't serve OPAQUE_FD-backed buffers to a
            // host-side consumer at all.
            let buffer = self.pixel_buffer_pool_manager.get_from_cache(surface_id);
            #[cfg(target_os = "linux")]
            let buffer = buffer.or_else(|| {
                let surface_store = self.surface_store.lock();
                surface_store
                    .as_ref()
                    .and_then(|store| store.lookup_buffer(surface_id).ok())
            });
            if let Some(buffer) = buffer {
                return self.refresh_pixel_buffer_texture(surface_id, &buffer, width, height);
            }
        }

        Err(Error::GpuError(format!(
            "No texture or pixel buffer found for surface_id '{}'",
            surface_id
        )))
    }

    /// Resolve a VideoFrame's texture — unified entry point for consumers
    /// that don't need layout metadata.
    ///
    /// Thin projection over [`Self::resolve_texture_registration_by_surface_id`].
    /// Layout-aware consumers (display, future encoders) should call
    /// `resolve_texture_registration_by_surface_id` directly so they can issue
    /// correct barriers.
    pub fn resolve_texture_by_surface_id(
        &self,
        surface_id: &str,
        texture_layout: Option<i32>,
        width: u32,
        height: u32,
    ) -> Result<Texture> {
        Ok(self
            .resolve_texture_registration_by_surface_id(surface_id, texture_layout, width, height)?
            .texture()
            .clone())
    }
}
