// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use parking_lot::Mutex;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib_consumer_rhi::{ConsumerVulkanBuffer, ConsumerVulkanDevice};

use super::{
    HelperCheckedOutPixelSurface, HelperCheckedOutSurface, HelperProcessGpuExchangeClient,
    HelperSurfaceCheckOutLeaseDebt, SURFACE_SHARE_RESPONSE_TIMEOUT, SurfaceShareAnswer,
    SurfaceShareTransferredHandle, refuse_check_out_the_service_declined,
    required_positive_u32_check_out_metadata_field,
};

mod texture;
pub(crate) use texture::HelperCheckedOutTextureSurface;

/// A pool slot's IOSurface pages imported as host memory on this helper's
/// consumer device; the import retains the surface.
pub(crate) struct HelperIOSurfacePoolSlotImport {
    pub(super) consumer_buffer: ConsumerVulkanBuffer,
}

impl HelperIOSurfacePoolSlotImport {
    /// Import `iosurface`'s pages on `vulkan_device`.
    fn import(
        vulkan_device: &Arc<ConsumerVulkanDevice>,
        iosurface: &objc2_io_surface::IOSurfaceRef,
    ) -> streamlib_consumer_rhi::Result<Self> {
        Ok(Self {
            consumer_buffer: ConsumerVulkanBuffer::from_iosurface_pages(vulkan_device, iosurface)?,
        })
    }

    /// The slot's IOSurface.
    fn iosurface(&self) -> &objc2_io_surface::IOSurfaceRef {
        self.consumer_buffer
            .backing_iosurface()
            .expect("an import built by `import` is backed by the IOSurface it was built from")
    }
}

/// A freshly minted send right to a surface's IOSurface plus the
/// allocation-stable shape native code needs to address it.
pub(crate) struct IOSurfaceMachPortExportDescription {
    pub(crate) iosurface_send_right: streamlib_surface_client::OwnedMachSendRight,
    pub(crate) allocation_byte_size: u64,
    pub(crate) bytes_per_row: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format_wire_name: &'static str,
    /// Present when the surface backs a texture.
    pub(crate) vk_image_creation_recipe:
        Option<crate::python_processor_context::ExportedVkImageCreationRecipe>,
}

impl IOSurfaceMachPortExportDescription {
    /// Mint a send right to `iosurface` and describe it as `surface_id`'s
    /// allocation.
    fn minted_from(
        iosurface: &objc2_io_surface::IOSurfaceRef,
        surface_id: &str,
        width: u32,
        height: u32,
        format_wire_name: &'static str,
        vk_image_creation_recipe: Option<
            crate::python_processor_context::ExportedVkImageCreationRecipe,
        >,
    ) -> PyResult<Self> {
        let iosurface_send_right =
            streamlib::sdk::engine::apple_surface_share::create_iosurface_mach_send_right(
                iosurface,
            )
            .map_err(|mint_failure| {
                PyRuntimeError::new_err(format!(
                    "surface {surface_id:?} minted no IOSurface port: {mint_failure}"
                ))
            })?;
        Ok(Self {
            iosurface_send_right,
            allocation_byte_size: iosurface.alloc_size() as u64,
            bytes_per_row: iosurface.bytes_per_row() as u64,
            width,
            height,
            format_wire_name,
            vk_image_creation_recipe,
        })
    }
}

impl HelperCheckedOutSurface {
    /// A fresh send right to the surface's IOSurface plus its
    /// allocation-stable shape, whichever backing answers.
    pub(crate) fn export_iosurface(&self) -> PyResult<IOSurfaceMachPortExportDescription> {
        match self {
            Self::PixelBuffer(pixel_surface) => IOSurfaceMachPortExportDescription::minted_from(
                pixel_surface.iosurface_pool_slot_import.iosurface(),
                &pixel_surface.surface_id,
                pixel_surface.width,
                pixel_surface.height,
                pixel_surface.format.wire_name(),
                None,
            ),
            Self::Texture(texture_surface) => texture_surface.export_iosurface(),
        }
    }
}

/// The per-slot cache, shared with the thread that empties it when the
/// parent's service goes away.
pub(super) type HelperIOSurfaceImportsByPoolSlot =
    Arc<Mutex<std::collections::HashMap<String, Arc<HelperIOSurfacePoolSlotImport>>>>;

/// One frame's raise of its IOSurface's use count, lowered on drop.
///
/// This is the claim `IOSurfaceIsInUse` answers across processes, so the pool
/// skips the slot while a view of the frame is live — even after a lease was
/// reclaimed on a connection drop — and the kernel lowers it if this process
/// dies. A cached `IOSurfaceRef` alone raises nothing.
pub(crate) struct HelperIOSurfaceUseCountClaim {
    iosurface_pool_slot_import: Arc<HelperIOSurfacePoolSlotImport>,
}

impl HelperIOSurfaceUseCountClaim {
    fn claiming(iosurface_pool_slot_import: Arc<HelperIOSurfacePoolSlotImport>) -> Self {
        iosurface_pool_slot_import.iosurface().increment_use_count();
        Self {
            iosurface_pool_slot_import,
        }
    }
}

impl Drop for HelperIOSurfaceUseCountClaim {
    fn drop(&mut self) {
        self.iosurface_pool_slot_import
            .iosurface()
            .decrement_use_count();
    }
}

/// IOSurfaceLock or IOSurfaceUnlock refused, with the kernel's code.
#[derive(Debug)]
pub(crate) struct IOSurfaceLockRefused {
    operation: &'static str,
    kern_return: i32,
}

impl std::fmt::Display for IOSurfaceLockRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} refused ({:#x})", self.operation, self.kern_return)
    }
}

/// The IOSurface lock a surface's CPU access holds, if any, with the options
/// it took — what its unlock must repeat.
#[derive(Default)]
pub(crate) struct HelperIOSurfaceCpuLock {
    held_lock_options: Mutex<Option<objc2_io_surface::IOSurfaceLockOptions>>,
}

impl HelperIOSurfaceCpuLock {
    /// Take `iosurface`'s lock read-only or read-write, replacing any lock
    /// already held. On a discrete-GPU Mac the lock is what makes the host
    /// view coherent with the GPU's copy.
    fn lock(
        &self,
        iosurface: &objc2_io_surface::IOSurfaceRef,
        read_only: bool,
    ) -> Result<(), IOSurfaceLockRefused> {
        let mut held_lock_options = self.held_lock_options.lock();
        Self::lock_replacing_held(&mut held_lock_options, iosurface, read_only)
    }

    /// Take `iosurface`'s lock read-only or read-write unless a lock is
    /// already held — the lock a CPU door takes once per lock scope.
    fn lock_unless_held(
        &self,
        iosurface: &objc2_io_surface::IOSurfaceRef,
        read_only: bool,
    ) -> Result<(), IOSurfaceLockRefused> {
        let mut held_lock_options = self.held_lock_options.lock();
        if held_lock_options.is_some() {
            return Ok(());
        }
        Self::lock_replacing_held(&mut held_lock_options, iosurface, read_only)
    }

    /// Take `iosurface`'s lock under the caller's guard on the held options,
    /// unlocking whatever lock those options record first.
    fn lock_replacing_held(
        held_lock_options: &mut Option<objc2_io_surface::IOSurfaceLockOptions>,
        iosurface: &objc2_io_surface::IOSurfaceRef,
        read_only: bool,
    ) -> Result<(), IOSurfaceLockRefused> {
        use objc2_io_surface::IOSurfaceLockOptions;
        if let Some(held_options) = *held_lock_options {
            Self::unlock_with(iosurface, held_options)?;
            *held_lock_options = None;
        }
        let lock_options = if read_only {
            IOSurfaceLockOptions::ReadOnly
        } else {
            IOSurfaceLockOptions::empty()
        };
        // SAFETY: a null seed pointer is documented as "not wanted".
        let kern_return = unsafe { iosurface.lock(lock_options, std::ptr::null_mut()) };
        if kern_return != 0 {
            return Err(IOSurfaceLockRefused {
                operation: "IOSurfaceLock",
                kern_return,
            });
        }
        *held_lock_options = Some(lock_options);
        Ok(())
    }

    /// Release the held lock, if any; a refused unlock leaves it recorded
    /// as held.
    fn release(
        &self,
        iosurface: &objc2_io_surface::IOSurfaceRef,
    ) -> Result<(), IOSurfaceLockRefused> {
        let mut held_lock_options = self.held_lock_options.lock();
        if let Some(held_options) = *held_lock_options {
            Self::unlock_with(iosurface, held_options)?;
            *held_lock_options = None;
        }
        Ok(())
    }

    fn unlock_with(
        iosurface: &objc2_io_surface::IOSurfaceRef,
        held_options: objc2_io_surface::IOSurfaceLockOptions,
    ) -> Result<(), IOSurfaceLockRefused> {
        // SAFETY: unlocks with the options the matching lock took.
        let kern_return = unsafe { iosurface.unlock(held_options, std::ptr::null_mut()) };
        if kern_return != 0 {
            return Err(IOSurfaceLockRefused {
                operation: "IOSurfaceUnlock",
                kern_return,
            });
        }
        Ok(())
    }
}

impl HelperCheckedOutPixelSurface {
    /// Take the IOSurface lock for CPU access, read-only or read-write,
    /// unless this surface already holds it.
    pub(crate) fn lock_the_iosurface_for_cpu_access_once(&self, read_only: bool) -> PyResult<()> {
        self.iosurface_cpu_lock
            .lock_unless_held(self.iosurface_pool_slot_import.iosurface(), read_only)
            .map_err(|refused| self.iosurface_lock_error(refused))
    }

    /// Release the IOSurface lock this surface's CPU access holds, if any.
    pub(crate) fn unlock_the_iosurface_after_cpu_access(&self) -> PyResult<()> {
        self.iosurface_cpu_lock
            .release(self.iosurface_pool_slot_import.iosurface())
            .map_err(|refused| self.iosurface_lock_error(refused))
    }

    fn iosurface_lock_error(&self, refused: IOSurfaceLockRefused) -> PyErr {
        PyRuntimeError::new_err(format!("{refused} on surface {:?}", self.surface_id))
    }

    /// A no-copy `MTLBuffer` over the pool slot's IOSurface pages.
    pub(crate) fn metal_buffer_over_the_iosurface_pages(
        &self,
    ) -> PyResult<objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLBuffer>>>
    {
        self.iosurface_pool_slot_import
            .consumer_buffer
            .exported_metal_buffer()
            .map_err(|export_failure| {
                PyRuntimeError::new_err(format!(
                    "surface {:?} has no Metal buffer over its IOSurface: {export_failure}",
                    self.surface_id
                ))
            })
    }
}

impl Drop for HelperCheckedOutPixelSurface {
    /// A surface dropped mid-access lets its IOSurface lock go before its
    /// use-count claim does.
    fn drop(&mut self) {
        if let Err(refused) = self
            .iosurface_cpu_lock
            .release(self.iosurface_pool_slot_import.iosurface())
        {
            tracing::warn!("{refused} on surface {:?}", self.surface_id);
        }
    }
}

impl HelperProcessGpuExchangeClient {
    /// This helper's connection to the parent's surface-share Mach service,
    /// opened on first use together with the thread that empties the
    /// per-slot cache when the service goes away — an IOSurface this helper
    /// still holds stays readable after the engine dies, so nothing else
    /// would release it.
    fn surface_share_mach_connection(
        &self,
    ) -> PyResult<Arc<streamlib_surface_client::SurfaceShareMachServiceConnection>> {
        let mut connection = self.surface_share_mach_connection.lock();
        if let Some(open_connection) = connection.as_ref() {
            return Ok(Arc::clone(open_connection));
        }
        let service_name = self
            .surface_share_mach_service_name
            .to_str()
            .ok_or_else(|| {
                PyRuntimeError::new_err(format!(
                    "the surface-share Mach service name {:?} is not UTF-8, so it names no \
                 bootstrap service",
                    self.surface_share_mach_service_name
                ))
            })?;
        let opened = Arc::new(
            streamlib_surface_client::SurfaceShareMachServiceConnection::connect(
                service_name,
                SURFACE_SHARE_RESPONSE_TIMEOUT,
            )
            .map_err(|connect_failure| {
                PyRuntimeError::new_err(format!(
                    "could not reach the surface-share Mach service '{service_name}': \
                     {connect_failure}. The parent runtime owns that service; if it is gone, \
                     this helper is orphaned",
                ))
            })?,
        );
        let watched_connection = Arc::clone(&opened);
        let iosurface_imports_by_pool_slot = Arc::clone(&self.iosurface_imports_by_pool_slot);
        std::thread::Builder::new()
            .name("surface-share-service-watch".into())
            .spawn(
                move || match watched_connection.wait_for_the_service_to_go_away(None) {
                    Ok(_) => {
                        let released = std::mem::take(&mut *iosurface_imports_by_pool_slot.lock());
                        tracing::info!(
                            "the surface-share service went away; released the {} pool slot(s) \
                             this helper had imported",
                            released.len()
                        );
                    }
                    Err(watch_failure) => tracing::warn!(
                        "could not watch the surface-share service for its going away \
                         ({watch_failure}); this helper keeps its imported pool slots until it \
                         stops"
                    ),
                },
            )
            .map_err(|spawn_failure| {
                PyRuntimeError::new_err(format!(
                    "could not start the thread that watches the surface-share service: \
                     {spawn_failure}"
                ))
            })?;
        *connection = Some(Arc::clone(&opened));
        Ok(opened)
    }

    pub(super) fn surface_share_request(
        &self,
        request: &serde_json::Value,
    ) -> PyResult<SurfaceShareAnswer> {
        self.surface_share_mach_connection()?
            .send_request_with_ports(request, Vec::new())
            .map_err(|request_failure| {
                PyRuntimeError::new_err(format!(
                    "the surface-share request failed: {request_failure}"
                ))
            })
    }

    /// Whether an edit written back into `surface_id` publishes at all.
    ///
    /// On macOS a surface's CPU view is its IOSurface's own pages — a pooled
    /// pixel buffer's and a texture's alike — which every other holder
    /// imports too, so the allocation is the surface's only backing and the
    /// edit reaches every holder. One checkout per pool slot, memoised on the
    /// same key the Linux door uses.
    pub(crate) fn surface_can_take_write_back(
        self: &Arc<Self>,
        python: Python<'_>,
        surface_id: &str,
    ) -> PyResult<bool> {
        let source_pool_slot_key = streamlib::sdk::rhi::pool_slot_key_of_surface_id(surface_id);
        if let Some(already_answered) = self
            .write_back_answers_by_pool_slot
            .lock()
            .get(source_pool_slot_key)
        {
            return Ok(*already_answered);
        }
        let (response, _transferred_handles_released_by_scope) =
            python.detach(|| self.check_out_surface(surface_id))?;
        refuse_check_out_the_service_declined(format_args!("{surface_id:?}"), &response)?;
        let _release_the_check_out_on_return = HelperSurfaceCheckOutLeaseDebt {
            exchange_client: Arc::clone(self),
            surface_id: surface_id.to_string(),
        };
        let registered_as = |field: &str, default: &'static str| {
            response
                .get(field)
                .and_then(|value| value.as_str())
                .unwrap_or(default)
                .to_string()
        };
        let can_take_write_back = matches!(
            registered_as("resource_type", "pixel_buffer").as_str(),
            "pixel_buffer" | "texture"
        ) && registered_as("handle_type", "iosurface") == "iosurface";
        self.write_back_answers_by_pool_slot
            .lock()
            .insert(source_pool_slot_key.to_string(), can_take_write_back);
        Ok(can_take_write_back)
    }

    /// The import of a checked-out frame's IOSurface: the pool slot's cached
    /// import, or a lookup of the port and a fresh import on the slot's first
    /// touch — plus this frame's use-count claim. The CPU reaches the pixels
    /// through the import's mapping, which is the IOSurface's own memory.
    pub(super) fn import_checked_out_surface(
        self: &Arc<Self>,
        surface_id: &str,
        response: &serde_json::Value,
        received_ports: Vec<SurfaceShareTransferredHandle>,
    ) -> PyResult<HelperCheckedOutSurface> {
        refuse_check_out_the_service_declined(format_args!("{surface_id:?}"), response)?;
        // From here the lease is this surface's, so every refusal below
        // releases it on the way out.
        let release_check_out_to_surface_share = HelperSurfaceCheckOutLeaseDebt {
            exchange_client: Arc::clone(self),
            surface_id: surface_id.to_string(),
        };

        let resource_type = response
            .get("resource_type")
            .and_then(|value| value.as_str())
            .unwrap_or("pixel_buffer");
        let handle_type = response
            .get("handle_type")
            .and_then(|value| value.as_str())
            .unwrap_or("iosurface");
        if handle_type != "iosurface" || !matches!(resource_type, "pixel_buffer" | "texture") {
            return Err(PyRuntimeError::new_err(format!(
                "surface {surface_id:?} is registered as a {resource_type:?} over a \
                 {handle_type:?} handle; a macOS helper imports IOSurface-backed pixel buffers \
                 and textures only"
            )));
        }
        if resource_type == "texture" {
            return self
                .import_checked_out_texture(
                    surface_id,
                    response,
                    received_ports,
                    release_check_out_to_surface_share,
                )
                .map(HelperCheckedOutSurface::Texture);
        }
        let width = required_positive_u32_check_out_metadata_field(response, surface_id, "width")?;
        let height =
            required_positive_u32_check_out_metadata_field(response, surface_id, "height")?;
        let format_name = response
            .get("format")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        let format = crate::python_processor_context::parse_pixel_format_name(format_name)?;

        let iosurface_pool_slot_import =
            self.iosurface_pool_slot_import_for(surface_id, received_ports)?;
        let iosurface = iosurface_pool_slot_import.iosurface();
        let bytes_per_row = iosurface.bytes_per_row() as u64;
        if iosurface.width() < width as usize
            || iosurface.height() < height as usize
            || bytes_per_row * u64::from(height) > iosurface.alloc_size() as u64
        {
            return Err(PyRuntimeError::new_err(format!(
                "surface {surface_id:?} is registered as {width}x{height}, which its {}x{} \
                 IOSurface of {bytes_per_row}-byte rows cannot hold",
                iosurface.width(),
                iosurface.height(),
            )));
        }

        Ok(HelperCheckedOutSurface::PixelBuffer(
            HelperCheckedOutPixelSurface {
                surface_id: surface_id.to_string(),
                iosurface_use_count_claim: HelperIOSurfaceUseCountClaim::claiming(Arc::clone(
                    &iosurface_pool_slot_import,
                )),
                iosurface_pool_slot_import,
                iosurface_cpu_lock: HelperIOSurfaceCpuLock::default(),
                width,
                height,
                format,
                bytes_per_row,
                release_to_parent: None,
                release_check_out_to_surface_share,
            },
        ))
    }

    /// The pool slot's import, from the cache — releasing the fresh port
    /// unlooked-up — or from the port on the slot's first touch.
    fn iosurface_pool_slot_import_for(
        &self,
        surface_id: &str,
        received_ports: Vec<SurfaceShareTransferredHandle>,
    ) -> PyResult<Arc<HelperIOSurfacePoolSlotImport>> {
        let mut received_ports = received_ports.into_iter();
        let (Some(iosurface_port), None) = (received_ports.next(), received_ports.next()) else {
            return Err(PyRuntimeError::new_err(format!(
                "check_out of {surface_id:?} did not carry exactly one IOSurface port"
            )));
        };
        let pool_slot_key = streamlib::sdk::rhi::pool_slot_key_of_surface_id(surface_id);
        if let Some(cached) = self
            .iosurface_imports_by_pool_slot
            .lock()
            .get(pool_slot_key)
        {
            return Ok(Arc::clone(cached));
        }
        let iosurface =
            objc2_io_surface::IOSurfaceRef::lookup_from_mach_port(iosurface_port.as_raw_name())
                .ok_or_else(|| {
                    PyRuntimeError::new_err(format!(
                        "check_out of {surface_id:?} carried a port that names no IOSurface"
                    ))
                })?;
        // Released as soon as it is looked up: a live port keeps the
        // surface reading in use.
        drop(iosurface_port);
        let vulkan_device = self.consumer_vulkan_device()?;
        let imported = Arc::new(
            HelperIOSurfacePoolSlotImport::import(&vulkan_device, &iosurface).map_err(
                |import_failure| {
                    PyRuntimeError::new_err(format!(
                        "Vulkan could not import surface {surface_id:?}'s IOSurface: \
                         {import_failure}"
                    ))
                },
            )?,
        );
        Ok(Arc::clone(
            self.iosurface_imports_by_pool_slot
                .lock()
                .entry(pool_slot_key.to_string())
                .or_insert(imported),
        ))
    }
}

/// The macOS frame path against a real Mach service: the per-slot IOSurface
/// cache and the use-count claim a held frame raises. Needs a Vulkan device
/// for the import, and says so rather than failing where there is none.
#[cfg(test)]
mod iosurface_pool_slot_import_tests;
