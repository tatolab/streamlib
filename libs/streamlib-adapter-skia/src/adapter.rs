// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `SkiaSurfaceAdapter<D>` — Skia-typed `SurfaceAdapter`.
//!
//! Composes on `streamlib-adapter-vulkan`'s `VulkanSurfaceAdapter<D>`
//! (held as `Arc<VulkanSurfaceAdapter<D>>`). The inner adapter does
//! the timeline-semaphore wait and `VkImageLayout` transition; this
//! adapter wraps the resulting `VkImage` as a Skia `Surface`
//! (write) or `Image` (read) using the create-time metadata exposed
//! by `VulkanImageInfoExt`.
//!
//! Generic over the device flavor `D: VulkanRhiDevice` for the same
//! reason `VulkanSurfaceAdapter<D>` is — works against either
//! `HostVulkanDevice` (in-tree host) or `ConsumerVulkanDevice`
//! (subprocess cdylib). The single-pattern shape from
//! `docs/architecture/subprocess-rhi-parity.md` carries through.

use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use libloading::Library;
use skia_safe::gpu::vk::{
    Alloc, AllocFlag, BackendContext, GetProcOf, ImageInfo as SkiaVkImageInfo,
};
use skia_safe::gpu::{
    backend_render_targets, backend_textures, direct_contexts, images as skia_images, surfaces,
    BackendTexture, DirectContext, Protected, SurfaceOrigin,
};
use skia_safe::{AlphaType, ColorSpace, ColorType};
use streamlib_adapter_abi::{
    AdapterError, ReadGuard, StreamlibSurface, SurfaceAdapter, SurfaceId, VulkanImageInfoExt,
    WriteGuard,
};
use streamlib_adapter_vulkan::VulkanSurfaceAdapter;
use streamlib_consumer_rhi::VulkanRhiDevice;
use vulkanalia::loader::LIBRARY;
use vulkanalia::vk::{self, DeviceV1_0, Handle as _, InstanceV1_0};

use crate::error::SkiaAdapterError;
use crate::skia_internal::SyncDirectContext;
use crate::view::{SkiaReadView, SkiaWriteView};

/// Skia-typed surface adapter. Composes on
/// [`VulkanSurfaceAdapter<D>`]; customers see only `skia::Surface` /
/// `skia::Image` through the [`SurfaceAdapter`] trait.
///
/// Skia's `GrDirectContext` is single-thread-affine and `!Send`; we
/// hold it inside a [`SyncDirectContext`] newtype with explicit
/// `unsafe impl Send + Sync` so the [`SurfaceAdapter`] trait's
/// `Send + Sync` bound is satisfiable. The adapter serializes every
/// operation through `Mutex<SyncDirectContext>`, so the upgrade is
/// sound. This is the standard pattern in graphics frameworks
/// (gst-plugin-skia, the rust-skia examples).
pub struct SkiaSurfaceAdapter<D: VulkanRhiDevice + 'static> {
    inner: Arc<VulkanSurfaceAdapter<D>>,
    direct_context: Arc<Mutex<SyncDirectContext>>,
}

impl<D: VulkanRhiDevice + 'static> SkiaSurfaceAdapter<D> {
    /// Build a Skia adapter on top of an existing Vulkan adapter.
    ///
    /// Constructs Skia's `BackendContext` from the underlying device's
    /// vulkanalia handles (instance, physical device, device, queue,
    /// queue family) and a `GetProc` shim that resolves Vulkan
    /// function pointers via the same `vkGetInstance/DeviceProcAddr`
    /// chain vulkanalia is already using. Returns
    /// [`SkiaAdapterError::DirectContextBuildFailed`] when Skia
    /// rejects the backend context (rare — typically caused by a
    /// missing extension).
    pub fn new(
        inner: Arc<VulkanSurfaceAdapter<D>>,
    ) -> Result<Self, SkiaAdapterError> {
        let direct_context = build_direct_context(inner.device())?;
        Ok(Self {
            inner,
            direct_context: Arc::new(Mutex::new(SyncDirectContext(direct_context))),
        })
    }

    /// Returns the inner Vulkan adapter — power-user accessor for
    /// callers that need to also touch the surface as raw Vulkan
    /// (debug tooling, custom RHIs). Customers should prefer the
    /// scoped `acquire_*` API.
    pub fn inner(&self) -> &Arc<VulkanSurfaceAdapter<D>> {
        &self.inner
    }
}

/// Build a Skia `DirectContext` from a `VulkanRhiDevice`.
///
/// `vkGetInstanceProcAddr` is the bottom of the Vulkan loader chain;
/// vulkanalia keeps it on a private `StaticCommands` field, so we
/// load it directly from `libvulkan.so` via `libloading`. Skia copies
/// every resolved proc into its own command tables when
/// `direct_contexts::make_vulkan` constructs the DirectContext, so
/// the captured fn pointer only needs to live across this function;
/// dropping the `Library` after `make_vulkan` is safe — `libvulkan`
/// stays loaded for the process lifetime via vulkanalia's own loader.
fn build_direct_context<D: VulkanRhiDevice>(
    device: &Arc<D>,
) -> Result<DirectContext, SkiaAdapterError> {
    let instance = device.instance();
    let physical_device = device.physical_device();
    let logical_device = device.device();
    let queue = device.queue();
    let queue_family_index = device.queue_family_index() as usize;

    let library = unsafe { Library::new(LIBRARY) }.map_err(|e| {
        SkiaAdapterError::DirectContextBuildFailed {
            reason: format!("dlopen {LIBRARY}: {e}"),
        }
    })?;
    let get_instance_proc_addr_sym: libloading::Symbol<vk::PFN_vkGetInstanceProcAddr> =
        unsafe { library.get(b"vkGetInstanceProcAddr\0") }.map_err(|e| {
            SkiaAdapterError::DirectContextBuildFailed {
                reason: format!("dlsym vkGetInstanceProcAddr: {e}"),
            }
        })?;
    let entry_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr =
        *get_instance_proc_addr_sym;
    let device_get_device_proc_addr = instance.commands().get_device_proc_addr;

    // Skia hands us its own typed handles inside GetProcOf and we use
    // those directly. The Vulkan spec requires
    // `vkGetInstanceProcAddr(NULL, "vkCreateInstance")` to work, so
    // accepting the skia-provided pointer (which may be `null` during
    // bootstrap) is the correct shape. The handles' raw bits round-trip
    // through skia's pointer-shaped typedefs back to vulkanalia's
    // `Handle::from_raw` cleanly on every Linux target.
    let get_proc = move |of: GetProcOf| -> *const c_void {
        match of {
            GetProcOf::Instance(skia_instance, name) => unsafe {
                let raw: Option<unsafe extern "system" fn()> =
                    entry_get_instance_proc_addr(
                        vk::Instance::from_raw(skia_instance as usize),
                        name,
                    );
                match raw {
                    Some(f) => f as *const c_void,
                    None => std::ptr::null(),
                }
            },
            GetProcOf::Device(skia_device, name) => unsafe {
                let raw: Option<unsafe extern "system" fn()> =
                    device_get_device_proc_addr(
                        vk::Device::from_raw(skia_device as usize),
                        name,
                    );
                match raw {
                    Some(f) => f as *const c_void,
                    None => std::ptr::null(),
                }
            },
        }
    };

    let backend_context = unsafe {
        BackendContext::new(
            instance.handle().as_raw() as _,
            physical_device.as_raw() as _,
            logical_device.handle().as_raw() as _,
            (queue.as_raw() as _, queue_family_index),
            &get_proc,
        )
    };

    direct_contexts::make_vulkan(&backend_context, None).ok_or_else(|| {
        SkiaAdapterError::DirectContextBuildFailed {
            reason: "skia_safe::gpu::direct_contexts::make_vulkan returned None"
                .into(),
        }
    })
}

/// Map a Vulkan `vk::Format` to a Skia `ColorType`.
fn vk_format_to_skia_color_type(format: vk::Format) -> Option<ColorType> {
    match format {
        vk::Format::B8G8R8A8_UNORM | vk::Format::B8G8R8A8_SRGB => {
            Some(ColorType::BGRA8888)
        }
        vk::Format::R8G8B8A8_UNORM | vk::Format::R8G8B8A8_SRGB => {
            Some(ColorType::RGBA8888)
        }
        vk::Format::R16G16B16A16_SFLOAT => Some(ColorType::RGBAF16),
        vk::Format::R32G32B32A32_SFLOAT => Some(ColorType::RGBAF32),
        _ => None,
    }
}

/// Build a Skia `gpu::vk::ImageInfo` from the inner Vulkan view's
/// metadata. Skia's `ImageInfo::new` doesn't take `image_usage_flags`
/// or `sample_count` — those are public fields on the resulting
/// struct, set after construction.
///
/// Type-cast strategy: skia-safe re-exports its `vk::*` types as
/// bindgen-generated typed integers (e.g. `pub use sb::VkFormat as
/// Format` where `VkFormat` is a typed integer). Vulkanalia hides
/// these behind newtypes. Both representations are the same width on
/// 64-bit platforms (Vulkan handles are u64-sized non-dispatchable;
/// enums are i32; bitmasks are u32) — `as _` lets the compiler
/// resolve the inferred coercion target.
fn build_skia_image_info<V: VulkanImageInfoExt>(
    view: &V,
) -> Result<SkiaVkImageInfo, SkiaAdapterError> {
    let info = view.vk_image_info();
    let vk_format = vk::Format::from_raw(info.format);
    if vk_format == vk::Format::UNDEFINED {
        return Err(SkiaAdapterError::UndefinedFormat);
    }

    // SAFETY: caller (the adapter) ensures the underlying VkImage and
    // VkDeviceMemory outlive the SkiaWriteView that wraps this info,
    // which in turn outlives any GrBackendRenderTarget built from it.
    // The inner WriteGuard pinned inside SkiaWriteView keeps the
    // streamlib-side Arc<P::Texture> alive for the same scope.
    let alloc = unsafe {
        Alloc::from_device_memory(
            info.memory_handle as _,
            info.memory_offset,
            info.memory_size,
            AllocFlag::empty(),
        )
    };

    // Skia's render-target paths reject `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`
    // — `wrap_backend_render_target` returns `None` on that combo even
    // though the underlying image is render-target-capable per the EGL
    // probe. We tell Skia the image is OPTIMAL: tiled DMA-BUF images
    // with a non-LINEAR DRM modifier behave equivalently to OPTIMAL
    // for Skia's purposes (the actual layout is opaque to the driver-
    // visible Vulkan commands Skia issues), so this cast doesn't
    // change runtime behavior — it just gets past Skia's validation.
    // LINEAR-tiled DMA-BUF imports are correctly reported as LINEAR.
    let normalized_tiling = if info.tiling == vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT.as_raw() {
        vk::ImageTiling::OPTIMAL.as_raw()
    } else {
        info.tiling
    };

    tracing::debug!(
        target: "streamlib_adapter_skia::adapter",
        vk_image = format!("0x{:x}", view.vk_image().0),
        vk_image_layout = view.vk_image_layout().0,
        memory_handle = format!("0x{:x}", info.memory_handle),
        memory_offset = info.memory_offset,
        memory_size = info.memory_size,
        memory_property_flags = format!("0x{:x}", info.memory_property_flags),
        format = info.format,
        tiling = normalized_tiling,
        usage_flags = format!("0x{:x}", info.usage_flags),
        sample_count = info.sample_count,
        level_count = info.level_count,
        queue_family = info.queue_family,
        protected = info.protected,
        "build_skia_image_info"
    );

    let mut skia_info = unsafe {
        SkiaVkImageInfo::new(
            view.vk_image().0 as _,
            alloc,
            std::mem::transmute::<i32, skia_safe::gpu::vk::ImageTiling>(normalized_tiling),
            std::mem::transmute::<i32, skia_safe::gpu::vk::ImageLayout>(
                view.vk_image_layout().0,
            ),
            std::mem::transmute::<i32, skia_safe::gpu::vk::Format>(info.format),
            info.level_count,
            Some(info.queue_family),
            None, // ycbcr_conversion_info
            Some(if info.protected != 0 {
                Protected::Yes
            } else {
                Protected::No
            }),
            None, // sharing_mode defaults to EXCLUSIVE
        )
    };

    skia_info.image_usage_flags = info.usage_flags;
    skia_info.sample_count = info.sample_count;

    Ok(skia_info)
}

impl<D: VulkanRhiDevice + 'static> SkiaSurfaceAdapter<D> {
    fn wrap_write<'g>(
        &'g self,
        inner_guard: WriteGuard<'g, VulkanSurfaceAdapter<D>>,
        width: i32,
        height: i32,
    ) -> Result<WriteGuard<'g, Self>, AdapterError> {
        let surface_id = inner_guard.surface_id();
        let view = inner_guard.view();
        let info = view.vk_image_info();

        let skia_info = build_skia_image_info(view).map_err(skia_to_adapter_error)?;
        let color_type = vk_format_to_skia_color_type(vk::Format::from_raw(info.format))
            .ok_or_else(|| AdapterError::UnsupportedFormat {
                surface_id,
                reason: format!(
                    "vk::Format {} not in Skia BGRA/RGBA mapping",
                    info.format
                ),
            })?;

        let backend_render_target = backend_render_targets::make_vk((width, height), &skia_info);

        let mut ctx_guard = self
            .direct_context
            .lock()
            .map_err(|_| AdapterError::BackendRejected {
                reason: "Skia DirectContext mutex poisoned".into(),
            })?;
        let surface = surfaces::wrap_backend_render_target(
            &mut ctx_guard.0,
            &backend_render_target,
            SurfaceOrigin::TopLeft,
            color_type,
            None::<ColorSpace>,
            None,
        )
        .ok_or_else(|| AdapterError::BackendRejected {
            reason: "skia_safe::gpu::surfaces::wrap_backend_render_target returned None"
                .into(),
        })?;
        drop(ctx_guard);

        let view = SkiaWriteView::new(
            surface,
            backend_render_target,
            inner_guard,
            Arc::clone(&self.direct_context),
        );

        Ok(WriteGuard::new(self, surface_id, view))
    }

    fn wrap_read<'g>(
        &'g self,
        inner_guard: ReadGuard<'g, VulkanSurfaceAdapter<D>>,
        width: i32,
        height: i32,
    ) -> Result<ReadGuard<'g, Self>, AdapterError> {
        let surface_id = inner_guard.surface_id();
        let view = inner_guard.view();
        let info = view.vk_image_info();

        let skia_info = build_skia_image_info(view).map_err(skia_to_adapter_error)?;
        let color_type = vk_format_to_skia_color_type(vk::Format::from_raw(info.format))
            .ok_or_else(|| AdapterError::UnsupportedFormat {
                surface_id,
                reason: format!(
                    "vk::Format {} not in Skia BGRA/RGBA mapping",
                    info.format
                ),
            })?;

        let backend_texture: BackendTexture = unsafe {
            backend_textures::make_vk((width, height), &skia_info, "streamlib-skia-read")
        };

        let mut ctx_guard = self
            .direct_context
            .lock()
            .map_err(|_| AdapterError::BackendRejected {
                reason: "Skia DirectContext mutex poisoned".into(),
            })?;
        let image = skia_images::borrow_texture_from(
            &mut ctx_guard.0,
            &backend_texture,
            SurfaceOrigin::TopLeft,
            color_type,
            AlphaType::Opaque,
            None::<ColorSpace>,
        )
        .ok_or_else(|| AdapterError::BackendRejected {
            reason: "skia_safe::gpu::images::borrow_texture_from returned None".into(),
        })?;
        drop(ctx_guard);

        let view = SkiaReadView::new(image, inner_guard, Arc::clone(&self.direct_context));
        Ok(ReadGuard::new(self, surface_id, view))
    }
}

fn skia_to_adapter_error(e: SkiaAdapterError) -> AdapterError {
    AdapterError::BackendRejected {
        reason: format!("skia adapter: {e}"),
    }
}

impl<D: VulkanRhiDevice + 'static> SurfaceAdapter for SkiaSurfaceAdapter<D> {
    type ReadView<'g> = SkiaReadView<'g, D>;
    type WriteView<'g> = SkiaWriteView<'g, D>;

    fn acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'g, Self>, AdapterError> {
        let inner_guard = self.inner.acquire_read(surface)?;
        self.wrap_read(inner_guard, surface.width as i32, surface.height as i32)
    }

    fn acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'g, Self>, AdapterError> {
        let inner_guard = self.inner.acquire_write(surface)?;
        self.wrap_write(inner_guard, surface.width as i32, surface.height as i32)
    }

    fn try_acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'g, Self>>, AdapterError> {
        match self.inner.try_acquire_read(surface)? {
            Some(g) => self
                .wrap_read(g, surface.width as i32, surface.height as i32)
                .map(Some),
            None => Ok(None),
        }
    }

    fn try_acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'g, Self>>, AdapterError> {
        match self.inner.try_acquire_write(surface)? {
            Some(g) => self
                .wrap_write(g, surface.width as i32, surface.height as i32)
                .map(Some),
            None => Ok(None),
        }
    }

    fn end_read_access(&self, _surface_id: SurfaceId) {
        // No-op. SkiaReadView::drop runs after the ReadGuard's Drop
        // hook fires this method (Rust drops fields *after* the
        // explicit Drop impl), and SkiaReadView::drop is what actually
        // releases the inner ReadGuard — which signals the timeline.
        // Doing anything here would be a use-after-release.
    }

    fn end_write_access(&self, _surface_id: SurfaceId) {
        // No-op — same reason as end_read_access. SkiaWriteView::drop
        // is the one place we flush Skia + signal the timeline.
    }
}

