// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use parking_lot::Mutex;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib::sdk::rhi::PixelFormat;
use streamlib_consumer_rhi::{ConsumerVulkanBuffer, VulkanLayout};
use streamlib_surface_client::{
    OwnedMachSendRight, SURFACE_SHARE_HOLDS_POOL_SLOT_IMPORT_FIELD,
    SURFACE_SHARE_IMPORT_POOL_SLOT_OP, refusal_the_surface_share_service_answered,
};

use super::{
    GpuPoolSlotFrameClaimOfThisImporter, HelperCheckedOutPixelSurface, HelperCheckedOutSurface,
    HelperProcessGpuExchangeClient, HelperSurfaceCheckOutLeaseDebt,
    HelperSurfaceHeldAgainstProducerReuse, SurfaceShareAnswer, SurfaceShareTransferredHandle,
    refuse_check_out_the_service_declined, required_positive_u32_check_out_metadata_field,
};
use streamlib_gpu_frame_import_client::{
    pool_slot_of_a_published_frame_id, pool_slots_a_reply_lists_as_retired,
};

mod foreign_iosurface;
mod storage_buffer;
mod texture;
pub(crate) use storage_buffer::HelperCheckedOutStorageBuffer;
use storage_buffer::HelperIOSurfaceStorageBufferPoolSlotImport;
pub(crate) use texture::HelperCheckedOutTextureSurface;
use texture::HelperIOSurfaceTexturePoolSlotImport;

/// A pixel-buffer pool slot's IOSurface pages imported as host memory on this
/// helper's consumer device, and the layout every frame over the slot shares;
/// the import retains the surface.
pub(crate) struct HelperIOSurfacePixelBufferPoolSlotImport {
    pub(super) consumer_buffer: ConsumerVulkanBuffer,
    width: u32,
    height: u32,
    format: PixelFormat,
    bytes_per_row: u64,
}

impl HelperIOSurfacePixelBufferPoolSlotImport {
    /// The slot's IOSurface.
    fn iosurface(&self) -> &objc2_io_surface::IOSurfaceRef {
        self.consumer_buffer
            .backing_iosurface()
            .expect("a pixel-buffer slot's import is backed by the IOSurface it was built from")
    }
}

/// One pool slot's IOSurface as this helper imported it, shared by every
/// frame over the slot.
#[derive(Clone)]
pub(crate) enum HelperPoolSlotImport {
    PixelBuffer(Arc<HelperIOSurfacePixelBufferPoolSlotImport>),
    Texture(Arc<HelperIOSurfaceTexturePoolSlotImport>),
    StorageBuffer(Arc<HelperIOSurfaceStorageBufferPoolSlotImport>),
}

impl HelperPoolSlotImport {
    /// The slot's IOSurface.
    fn iosurface(&self) -> &objc2_io_surface::IOSurfaceRef {
        match self {
            Self::PixelBuffer(pixel_buffer_pool_slot_import) => {
                pixel_buffer_pool_slot_import.iosurface()
            }
            Self::Texture(texture_pool_slot_import) => texture_pool_slot_import.iosurface(),
            Self::StorageBuffer(storage_buffer_pool_slot_import) => {
                storage_buffer_pool_slot_import.iosurface()
            }
        }
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
            Self::StorageBuffer(_) => Err(super::a_tensor_surface_is_not_a_pixel_surface()),
        }
    }
}

/// The IOSurface a checkout's port names, releasing the port as soon as it is
/// looked up: a live port keeps the surface reading in use.
fn iosurface_named_by_the_check_out_port(
    checked_out_surface_description: &str,
    iosurface_port: SurfaceShareTransferredHandle,
) -> PyResult<objc2_core_foundation::CFRetained<objc2_io_surface::IOSurfaceRef>> {
    let iosurface =
        objc2_io_surface::IOSurfaceRef::lookup_from_mach_port(iosurface_port.as_raw_name())
            .ok_or_else(|| {
                PyRuntimeError::new_err(format!(
                    "check_out of {checked_out_surface_description} carried a port that names \
                     no IOSurface"
                ))
            })?;
    drop(iosurface_port);
    Ok(iosurface)
}

/// One frame's raise of its IOSurface's use count, lowered on drop.
///
/// This is the claim `IOSurfaceIsInUse` answers across processes, so the pool
/// skips the slot while a view of the frame is live — even after a lease was
/// reclaimed on a connection drop — and the kernel lowers it if this process
/// dies. A cached `IOSurfaceRef` alone raises nothing.
pub(crate) struct HelperIOSurfaceUseCountClaim {
    pool_slot_import: HelperPoolSlotImport,
}

impl HelperIOSurfaceUseCountClaim {
    fn claiming(pool_slot_import: HelperPoolSlotImport) -> Self {
        pool_slot_import.iosurface().increment_use_count();
        Self { pool_slot_import }
    }
}

impl Drop for HelperIOSurfaceUseCountClaim {
    fn drop(&mut self) {
        self.pool_slot_import.iosurface().decrement_use_count();
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
    /// The one exchange every surface-share request makes, over this
    /// helper's connection; see
    /// [`SurfaceShareConnectionOfThisImporter::exchange`](streamlib_gpu_frame_import_client::SurfaceShareConnectionOfThisImporter::exchange).
    pub(super) fn surface_share_exchange<DecidedWhileTheConnectionIsHeld>(
        &self,
        request: &serde_json::Value,
        outbound_ports: Vec<OwnedMachSendRight>,
        while_the_connection_is_held: impl FnOnce(&serde_json::Value) -> DecidedWhileTheConnectionIsHeld,
    ) -> PyResult<(
        serde_json::Value,
        Vec<SurfaceShareTransferredHandle>,
        DecidedWhileTheConnectionIsHeld,
    )> {
        self.surface_share_connection
            .exchange(request, outbound_ports, while_the_connection_is_held)
            .map_err(PyRuntimeError::new_err)
    }

    pub(super) fn surface_share_request(
        &self,
        request: &serde_json::Value,
    ) -> PyResult<SurfaceShareAnswer> {
        let (reply, received_ports, ()) =
            self.surface_share_exchange(request, Vec::new(), |_| ())?;
        Ok((reply, received_ports))
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

    /// `check_out` over the surface-share service, then the import of
    /// whichever backing the registration names — or, for a published frame
    /// over a pool slot this helper already imported, that import, with no
    /// port sent. A pooled frame its pool's claim table covers is claimed in
    /// that table instead of checked out.
    pub(crate) fn check_out_and_import(
        self: &Arc<Self>,
        surface_id: &str,
    ) -> PyResult<HelperCheckedOutSurface> {
        if let Some(claimed) = self.claim_in_its_pool_claim_table(surface_id)? {
            return self.import_a_frame_claimed_in_its_pool_claim_table(surface_id, claimed);
        }
        let published_pool_slot = pool_slot_of_a_published_frame_id(surface_id);
        let held_pool_slot_import = published_pool_slot.and_then(|published_pool_slot| {
            self.surface_share_connection
                .pool_slot_imports_by_pool_slot()
                .import_held_for(published_pool_slot)
        });
        let holds_pool_slot_import = held_pool_slot_import.is_some();
        let (response, received_ports, fresh_import_reservation) = self.surface_share_exchange(
            &serde_json::json!({
                "op": "check_out",
                "surface_id": surface_id,
                SURFACE_SHARE_HOLDS_POOL_SLOT_IMPORT_FIELD: holds_pool_slot_import,
            }),
            Vec::new(),
            |response| match published_pool_slot {
                Some(published_pool_slot)
                    if !holds_pool_slot_import
                        && refusal_the_surface_share_service_answered(response).is_none()
                        && !pool_slots_a_reply_lists_as_retired(response)
                            .any(|retired_pool_slot| retired_pool_slot == published_pool_slot) =>
                {
                    Some(
                        self.surface_share_connection
                            .pool_slot_imports_by_pool_slot()
                            .reserve_for_a_fresh_import(published_pool_slot),
                    )
                }
                _ => None,
            },
        )?;
        refuse_check_out_the_service_declined(format_args!("{surface_id:?}"), &response)?;
        // From here the lease is this surface's, so every refusal below
        // releases it on the way out.
        let held_against_producer_reuse =
            HelperSurfaceHeldAgainstProducerReuse::CheckOutLease(HelperSurfaceCheckOutLeaseDebt {
                exchange_client: Arc::clone(self),
                surface_id: surface_id.to_string(),
            });
        let pool_slot_import = match held_pool_slot_import {
            Some(held_pool_slot_import) => held_pool_slot_import,
            None => {
                let imported = self.import_pool_slot_the_service_handed_over(
                    surface_id,
                    &response,
                    received_ports,
                )?;
                if let Some(fresh_import_reservation) = fresh_import_reservation {
                    let _ = fresh_import_reservation.set(imported.clone());
                }
                imported
            }
        };
        self.frame_over_a_pool_slot_import(
            surface_id,
            texture::current_image_layout_of_a_texture_check_out(&response),
            pool_slot_import,
            held_against_producer_reuse,
        )
    }

    /// One frame held by its pool claim table claim, read through the slot's
    /// import — made once per slot with `import_pool_slot`, which takes no
    /// lease.
    fn import_a_frame_claimed_in_its_pool_claim_table(
        self: &Arc<Self>,
        surface_id: &str,
        claimed: Arc<GpuPoolSlotFrameClaimOfThisImporter>,
    ) -> PyResult<HelperCheckedOutSurface> {
        let pool_slot_key = pool_slot_of_a_published_frame_id(surface_id).ok_or_else(|| {
            PyRuntimeError::new_err(format!(
                "{surface_id:?} was claimed in a pool claim table but names no pool slot"
            ))
        })?;
        let pool_slot_import = match self
            .surface_share_connection
            .pool_slot_imports_by_pool_slot()
            .import_held_for(pool_slot_key)
        {
            Some(held_pool_slot_import) => held_pool_slot_import,
            None => {
                let (reply, received_ports, fresh_import_reservation) = self
                    .surface_share_exchange(
                        &serde_json::json!({
                            "op": SURFACE_SHARE_IMPORT_POOL_SLOT_OP,
                            "surface_id": pool_slot_key,
                        }),
                        Vec::new(),
                        |reply| {
                            (refusal_the_surface_share_service_answered(reply).is_none()
                                && !pool_slots_a_reply_lists_as_retired(reply)
                                    .any(|retired_pool_slot| retired_pool_slot == pool_slot_key))
                            .then(|| {
                                self.surface_share_connection
                                    .pool_slot_imports_by_pool_slot()
                                    .reserve_for_a_fresh_import(pool_slot_key)
                            })
                        },
                    )?;
                if let Some(refusal) = refusal_the_surface_share_service_answered(&reply) {
                    return Err(PyRuntimeError::new_err(format!(
                        "the surface-share service refused to import pool slot \
                         {pool_slot_key:?}: {refusal}"
                    )));
                }
                let imported = self.import_pool_slot_the_service_handed_over(
                    pool_slot_key,
                    &reply,
                    received_ports,
                )?;
                if let Some(fresh_import_reservation) = fresh_import_reservation {
                    let _ = fresh_import_reservation.set(imported.clone());
                }
                imported
            }
        };
        let image_layout = self
            .gpu_pool_claim_tables
            .descriptor_of_the_frame_named(surface_id)
            .map_or(claimed.claimed_frame().image_layout, |frame| {
                frame.image_layout
            });
        self.frame_over_a_pool_slot_import(
            surface_id,
            VulkanLayout(image_layout),
            pool_slot_import,
            HelperSurfaceHeldAgainstProducerReuse::GpuPoolClaim(claimed),
        )
    }

    /// Validate a registration the service answered and import the slot's
    /// IOSurface as whichever backing it names: mapped pages for a pixel
    /// buffer, an image for a texture, pages for a tensor.
    fn import_pool_slot_the_service_handed_over(
        &self,
        surface_id: &str,
        response: &serde_json::Value,
        received_ports: Vec<SurfaceShareTransferredHandle>,
    ) -> PyResult<HelperPoolSlotImport> {
        let resource_type = response
            .get("resource_type")
            .and_then(|value| value.as_str())
            .unwrap_or("pixel_buffer");
        let handle_type = response
            .get("handle_type")
            .and_then(|value| value.as_str())
            .unwrap_or("iosurface");
        if handle_type != "iosurface" {
            return Err(PyRuntimeError::new_err(format!(
                "surface {surface_id:?} is registered as a {resource_type:?} over a \
                 {handle_type:?} handle; a macOS helper imports IOSurface-backed pixel buffers, \
                 textures and tensor storage buffers only"
            )));
        }
        match resource_type {
            "pixel_buffer" => {
                self.import_pixel_buffer_pool_slot(surface_id, response, received_ports)
            }
            "texture" => self.import_texture_pool_slot(surface_id, response, received_ports),
            "storage_buffer" => {
                self.import_storage_buffer_pool_slot(surface_id, response, received_ports)
            }
            _ => Err(PyRuntimeError::new_err(format!(
                "surface {surface_id:?} is registered as a {resource_type:?} over a \
                 {handle_type:?} handle; a macOS helper imports IOSurface-backed pixel buffers, \
                 textures and tensor storage buffers only"
            ))),
        }
    }

    /// A pixel-buffer pool slot's import: the one IOSurface port looked up,
    /// checked against the registered size, and its pages imported. The CPU
    /// reaches the pixels through the import's mapping, which is the
    /// IOSurface's own memory.
    fn import_pixel_buffer_pool_slot(
        &self,
        surface_id: &str,
        response: &serde_json::Value,
        received_ports: Vec<SurfaceShareTransferredHandle>,
    ) -> PyResult<HelperPoolSlotImport> {
        let width = required_positive_u32_check_out_metadata_field(response, surface_id, "width")?;
        let height =
            required_positive_u32_check_out_metadata_field(response, surface_id, "height")?;
        let format_name = response
            .get("format")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        let format = crate::python_processor_context::parse_pixel_format_name(format_name)?;
        let mut received_ports = received_ports.into_iter();
        let (Some(iosurface_port), None) = (received_ports.next(), received_ports.next()) else {
            return Err(PyRuntimeError::new_err(format!(
                "check_out of {surface_id:?} did not carry exactly one IOSurface port"
            )));
        };
        let iosurface =
            iosurface_named_by_the_check_out_port(&format!("{surface_id:?}"), iosurface_port)?;
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
        let vulkan_device = self.consumer_vulkan_device()?;
        let consumer_buffer = ConsumerVulkanBuffer::from_iosurface_pages(
            &vulkan_device,
            &iosurface,
        )
        .map_err(|import_failure| {
            PyRuntimeError::new_err(format!(
                "Vulkan could not import surface {surface_id:?}'s IOSurface: {import_failure}"
            ))
        })?;
        Ok(HelperPoolSlotImport::PixelBuffer(Arc::new(
            HelperIOSurfacePixelBufferPoolSlotImport {
                consumer_buffer,
                width,
                height,
                format,
                bytes_per_row,
            },
        )))
    }

    /// One frame over `pool_slot_import`, held by `held_against_producer_reuse`
    /// and by its own raise of the IOSurface's use count.
    fn frame_over_a_pool_slot_import(
        self: &Arc<Self>,
        surface_id: &str,
        producer_image_layout: VulkanLayout,
        pool_slot_import: HelperPoolSlotImport,
        held_against_producer_reuse: HelperSurfaceHeldAgainstProducerReuse,
    ) -> PyResult<HelperCheckedOutSurface> {
        match pool_slot_import {
            HelperPoolSlotImport::PixelBuffer(pixel_buffer_pool_slot_import) => Ok(
                HelperCheckedOutSurface::PixelBuffer(HelperCheckedOutPixelSurface {
                    surface_id: surface_id.to_string(),
                    iosurface_use_count_claim: HelperIOSurfaceUseCountClaim::claiming(
                        HelperPoolSlotImport::PixelBuffer(Arc::clone(
                            &pixel_buffer_pool_slot_import,
                        )),
                    ),
                    width: pixel_buffer_pool_slot_import.width,
                    height: pixel_buffer_pool_slot_import.height,
                    format: pixel_buffer_pool_slot_import.format,
                    bytes_per_row: pixel_buffer_pool_slot_import.bytes_per_row,
                    iosurface_pool_slot_import: pixel_buffer_pool_slot_import,
                    iosurface_cpu_lock: HelperIOSurfaceCpuLock::default(),
                    release_to_parent: None,
                    held_against_producer_reuse,
                }),
            ),
            HelperPoolSlotImport::Texture(texture_pool_slot_import) => self
                .texture_frame_over_a_pool_slot_import(
                    surface_id,
                    producer_image_layout,
                    texture_pool_slot_import,
                    held_against_producer_reuse,
                )
                .map(HelperCheckedOutSurface::Texture),
            HelperPoolSlotImport::StorageBuffer(storage_buffer_pool_slot_import) => {
                Ok(HelperCheckedOutSurface::StorageBuffer(
                    storage_buffer::storage_buffer_frame_over_a_pool_slot_import(
                        surface_id,
                        storage_buffer_pool_slot_import,
                        held_against_producer_reuse,
                    ),
                ))
            }
        }
    }
}

/// The macOS frame path against a real Mach service: the per-slot IOSurface
/// cache and the use-count claim a held frame raises. Needs a Vulkan device
/// for the import, and says so rather than failing where there is none.
#[cfg(test)]
mod iosurface_pool_slot_import_tests;
