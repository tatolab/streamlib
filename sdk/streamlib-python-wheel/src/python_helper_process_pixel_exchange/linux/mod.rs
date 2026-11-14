// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::os::fd::{IntoRawFd as _, OwnedFd, RawFd};
use std::sync::Arc;

use pyo3::exceptions::{PyKeyError, PyRuntimeError};
use pyo3::prelude::*;
use streamlib::sdk::rhi::PixelFormat;
use streamlib_consumer_rhi::{ConsumerVulkanBuffer, VulkanLayout};
use streamlib_surface_client::{
    CheckedOutPixelBuffer, CheckedOutPixelBufferMemory, SURFACE_SHARE_HOLDS_POOL_SLOT_IMPORT_FIELD,
    SURFACE_SHARE_IMPORT_POOL_SLOT_OP, SurfaceShareCheckOutFds,
    refusal_the_surface_share_service_answered, resource_type_of_a_check_out,
};

use super::{
    GpuPoolSlotFrameClaimOfThisImporter, HelperCheckedOutPixelSurface, HelperCheckedOutSurface,
    HelperProcessGpuExchangeClient, HelperSurfaceCheckOutLeaseDebt,
    HelperSurfaceHeldAgainstProducerReuse, refuse_check_out_the_service_declined, response_field,
};
use streamlib_gpu_frame_import_client::{
    pool_slot_of_a_published_frame_id, pool_slots_a_reply_lists_as_retired,
};
use texture::an_acquired_device_texture_carries_no_exportable_fd_error;

mod export_staging;
mod foreign_dma_buf;
mod storage_buffer;
mod texture;

pub(crate) use export_staging::{
    CpuReadbackCopyDirection, HelperCpuReadbackExport, HelperDeviceExport,
};
pub(crate) use foreign_dma_buf::{
    DRM_FORMAT_MOD_LINEAR, ForeignDmaBufTextureWithATiledModifier,
    HelperForeignSurfaceUnregisterDebt, foreign_dma_buf_plane_layout,
};
pub(crate) use storage_buffer::HelperCheckedOutStorageBuffer;
pub(crate) use texture::{HelperAcquiredTexture, HelperCheckedOutTextureSurface};

use crate::python_processor_context::{ExportedVkImageCreationRecipe, OpaqueFdExportContract};

/// A u64 the wire carries as a decimal string, because JSON has no 64-bit
/// integer. Host-side counterpart: `EscalateResponseOk::staging_byte_size`.
fn decimal_string_field(response: &Bound<'_, PyAny>, field: &str) -> PyResult<u64> {
    optional_decimal_string_field(response, field)?.ok_or_else(|| {
        crate::python_processor_context::gpu_operation_error(format!(
            "the parent's response carried no {field}"
        ))
    })
}

/// A decimal-string u64 the parent may leave out of its response; `None`
/// when it did.
fn optional_decimal_string_field(
    response: &Bound<'_, PyAny>,
    field: &str,
) -> PyResult<Option<u64>> {
    let as_written = match response.get_item(field) {
        Ok(as_written) if as_written.is_none() => return Ok(None),
        Ok(as_written) => as_written,
        Err(lookup_failure) if lookup_failure.is_instance_of::<PyKeyError>(response.py()) => {
            return Ok(None);
        }
        Err(lookup_failure) => return Err(lookup_failure),
    };
    let as_written: String = as_written.extract()?;
    as_written.parse().map(Some).map_err(|_| {
        crate::python_processor_context::gpu_operation_error(format!(
            "the parent's {field} was {as_written:?}, which is not a decimal u64"
        ))
    })
}

/// Whether a check-out's registration states the timeline fd `flag` names
/// (`has_produce_done_fd`, `has_consume_done_fd`); such fds trail the
/// memory's.
fn registration_states_a_timeline_fd(registration: &serde_json::Value, flag: &str) -> bool {
    registration
        .get(flag)
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

/// The exporting device's UUID, as 32 hex characters.
fn parse_device_uuid(as_hex: &str) -> PyResult<[u8; 16]> {
    streamlib_consumer_rhi::vulkan_device_uuid_from_hex(as_hex).ok_or_else(|| {
        crate::python_processor_context::gpu_operation_error(format!(
            "the parent reported the exporting device UUID as {as_hex:?}, which is not 32 hex \
             characters; importing onto the wrong GPU reads the wrong memory rather than failing"
        ))
    })
}

/// The memory type index an OPAQUE_FD registration states, or the refusal
/// naming `registered_subject`.
///
/// Mandatory, not defensive: a conforming OPAQUE_FD import binds the index
/// the *exporter* allocated from, and the handle type has no fd-properties
/// query to discover it with. A registration without one would leave the
/// import guessing, and a guess binds the wrong memory type rather than
/// failing — silently, until the pixels are wrong.
fn memory_type_index_stated_by_an_opaque_fd_registration(
    registered_subject: std::fmt::Arguments<'_>,
    registration: &serde_json::Value,
) -> PyResult<u32> {
    registration
        .get("vk_memory_type_index")
        .and_then(serde_json::Value::as_u64)
        .and_then(|index| u32::try_from(index).ok())
        .ok_or_else(|| {
            crate::python_processor_context::gpu_operation_error(format!(
                "{registered_subject} was registered without a usable vk_memory_type_index; an \
                 OPAQUE_FD import must bind the memory type index the exporter allocated from, \
                 and guessing one binds the wrong memory instead of failing"
            ))
        })
}

/// The export contract an OPAQUE_FD registration states — the exporter's
/// memory type index and device UUID — or the refusal naming
/// `registered_subject` and the field it lacks.
fn opaque_fd_export_contract_stated_by_a_registration(
    registered_subject: std::fmt::Arguments<'_>,
    registration: &serde_json::Value,
) -> PyResult<OpaqueFdExportContract> {
    let vk_memory_type_index =
        memory_type_index_stated_by_an_opaque_fd_registration(registered_subject, registration)?;
    let exporting_device_uuid = registration
        .get("exporting_device_uuid")
        .and_then(|value| value.as_str())
        .map(parse_device_uuid)
        .transpose()?
        .ok_or_else(|| {
            PyRuntimeError::new_err(format!(
                "{registered_subject} was registered without its exporting device UUID; \
                 importing onto the wrong GPU of a multi-GPU rig reads the wrong memory \
                 instead of failing here"
            ))
        })?;
    Ok(OpaqueFdExportContract {
        vk_memory_type_index,
        exporting_device_uuid,
    })
}

/// What `export_opaque_fd` hands across: the freshly dup'd memory fd —
/// caller-owned from this moment — plus the allocation-stable shape a
/// foreign Vulkan or CUDA external-memory import must reproduce.
pub(crate) struct OpaqueFdAllocationExportDescription {
    pub(crate) exported_memory_fd: OwnedFd,
    pub(crate) allocation_byte_size: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format_wire_name: &'static str,
    /// Present when the allocation backs a texture.
    pub(crate) vk_image_creation_recipe: Option<ExportedVkImageCreationRecipe>,
    pub(crate) dedicated_allocation: bool,
    pub(crate) export_contract: OpaqueFdExportContract,
}

/// A `u64` array field of a checkout's registration metadata; absent or
/// malformed entries collapse to an empty array the caller length-checks.
fn plane_u64_array_check_out_metadata_field(response: &serde_json::Value, field: &str) -> Vec<u64> {
    response
        .get(field)
        .and_then(|value| value.as_array())
        .map(|entries| entries.iter().filter_map(|entry| entry.as_u64()).collect())
        .unwrap_or_default()
}

/// Dup every plane fd for a Vulkan import, leaving the originals with the
/// surface so later exports answer locally. vkAllocateMemory takes fd
/// ownership only on success, so importing the originals would leave their
/// ownership ambiguous on a partial multi-plane failure.
///
/// Returned raw: ownership hands to the very next Vulkan import call. The
/// dups are collected as `OwnedFd` first so a partial duplication failure
/// closes the successes by scope.
fn duplicate_plane_fds_for_import(plane_fds: &[OwnedFd]) -> PyResult<Vec<RawFd>> {
    let duplicated: Vec<OwnedFd> = plane_fds
        .iter()
        .map(|plane_fd| {
            plane_fd.try_clone().map_err(|duplicate_failure| {
                PyRuntimeError::new_err(format!(
                    "could not duplicate a plane fd for import: {duplicate_failure}"
                ))
            })
        })
        .collect::<PyResult<_>>()?;
    Ok(duplicated.into_iter().map(OwnedFd::into_raw_fd).collect())
}

/// Close a dup Vulkan refused ownership of — and this path can run per
/// frame. Single-plane only: a multi-plane failure leaves the tail's
/// ownership ambiguous (already-imported planes were freed by the callee's
/// teardown), and those dups leak, bounded by plane count.
fn close_single_plane_dup_vulkan_refused(dup_raw_fds: &[RawFd]) {
    if let [only_plane_fd] = dup_raw_fds {
        // SAFETY: an fd Vulkan refused ownership of; ours alone.
        unsafe { libc::close(*only_plane_fd) };
    }
}

/// Dup the first plane fd so the caller can hand it to native code without
/// disturbing the surface's own mapping.
fn duplicate_first_plane_fd_for_export(
    exported_plane_fds: &[OwnedFd],
    exported_subject: &str,
) -> PyResult<OwnedFd> {
    let first_plane_fd = exported_plane_fds.first().ok_or_else(|| {
        PyRuntimeError::new_err(format!(
            "this {exported_subject} was checked out with no plane fd to export; nothing to \
             hand to native code"
        ))
    })?;
    duplicate_memory_fd(first_plane_fd, format_args!("this {exported_subject}'s"))
}

/// Dup one memory fd, refusing by `duplicated_owner` (`this surface's`)
/// when the kernel will not.
fn duplicate_memory_fd(
    memory_fd: &OwnedFd,
    duplicated_owner: std::fmt::Arguments<'_>,
) -> PyResult<OwnedFd> {
    memory_fd.try_clone().map_err(|duplicate_failure| {
        PyRuntimeError::new_err(format!(
            "could not duplicate {duplicated_owner} memory fd: {duplicate_failure}"
        ))
    })
}

/// Which handle type a checked-out pixel buffer's memory crossed as.
pub(crate) enum HelperPixelBufferMemoryFlavour {
    /// DMA-BUF planes. The delivered fds are kept so `export_dma_buf`
    /// answers from them: they are the same fds a host-side export would
    /// mint — the check-out is a kernel dup of that export — so the child
    /// answers locally instead of asking for something it already holds.
    DmaBuf { exportable_plane_fds: Vec<OwnedFd> },
    /// One OPAQUE_FD allocation — what a pooled frame is on a device without
    /// DMA-BUF export. The Vulkan import took a dup; the delivered fd is kept
    /// so `export_opaque_fd` answers from it, with the contract the
    /// registration stated.
    OpaqueFd {
        exportable_memory_fd: OwnedFd,
        export_contract: OpaqueFdExportContract,
    },
}

/// One pool slot's memory as this helper imported it, shared by every frame
/// checked out over the slot.
#[derive(Clone)]
pub(crate) enum HelperPoolSlotImport {
    PixelBuffer(Arc<HelperPixelBufferPoolSlotImport>),
    Texture(Arc<texture::HelperTexturePoolSlotImport>),
    StorageBuffer(Arc<storage_buffer::HelperStorageBufferPoolSlotImport>),
}

/// One pixel-buffer pool slot as this helper imported it: the mapped memory
/// and the layout every frame over the slot shares.
pub(crate) struct HelperPixelBufferPoolSlotImport {
    pub(crate) consumer_buffer: ConsumerVulkanBuffer,
    /// Which handle type the memory crossed as, and the fds `export_dma_buf`
    /// and `export_opaque_fd` answer from.
    memory_flavour: HelperPixelBufferMemoryFlavour,
    width: u32,
    height: u32,
    format: PixelFormat,
    bytes_per_row: u64,
}

impl HelperCheckedOutPixelSurface {
    /// A DMA-BUF fd for the first plane, and the plane's byte size.
    ///
    /// The fd is a `dup` of the one this process was handed at check-out,
    /// so the caller owns it and closing it does not disturb this
    /// surface's own mapping. Refused by name for an OPAQUE_FD pixel
    /// buffer, pointing at `export_opaque_fd`, so this name never
    /// mislabels an fd.
    pub(crate) fn export_dma_buf(&self) -> PyResult<(RawFd, u64)> {
        let HelperPixelBufferMemoryFlavour::DmaBuf {
            exportable_plane_fds,
        } = &self.pixel_buffer_pool_slot_import.memory_flavour
        else {
            return Err(PyRuntimeError::new_err(format!(
                "surface {:?} is an OPAQUE_FD pixel buffer — what a pooled frame is on a \
                 device without DMA-BUF export — so it has no DMA-BUF fd to hand out; \
                 `export_opaque_fd` hands its memory fd out under its own name",
                self.surface_id
            )));
        };
        let exported = duplicate_first_plane_fd_for_export(exportable_plane_fds, "surface")?;
        Ok((
            exported.into_raw_fd(),
            self.pixel_buffer_pool_slot_import
                .consumer_buffer
                .plane_size(0),
        ))
    }

    /// The OPAQUE_FD memory fd plus the allocation-stable shape a foreign
    /// import must reproduce; a pixel buffer has no image recipe.
    ///
    /// The mirror of the refusal above: a DMA-BUF pixel buffer is refused
    /// by name, pointing at `export_dma_buf`.
    pub(crate) fn export_opaque_fd(&self) -> PyResult<OpaqueFdAllocationExportDescription> {
        let HelperPixelBufferMemoryFlavour::OpaqueFd {
            exportable_memory_fd,
            export_contract,
        } = &self.pixel_buffer_pool_slot_import.memory_flavour
        else {
            return Err(a_dma_buf_pixel_buffer_exports_no_opaque_fd_error(
                &self.surface_id,
            ));
        };
        Ok(OpaqueFdAllocationExportDescription {
            exported_memory_fd: duplicate_memory_fd(
                exportable_memory_fd,
                format_args!("surface {:?}'s", self.surface_id),
            )?,
            // What this process's own import stated: the size an OPAQUE_FD
            // import must match, which the pool slot's driver requirement
            // can pad past the frame's bytes.
            allocation_byte_size: self
                .pixel_buffer_pool_slot_import
                .consumer_buffer
                .imported_allocation_byte_size(),
            width: self.width,
            height: self.height,
            format_wire_name: self.format.wire_name(),
            vk_image_creation_recipe: None,
            // DEDICATED_MEMORY by construction for the flavour
            // (`HostVulkanBuffer::new_opaque_fd_export`).
            dedicated_allocation: true,
            export_contract: *export_contract,
        })
    }
}

/// The refusal `export_opaque_fd` gives a DMA-BUF pixel buffer, pointing at
/// the door its flavour does have.
fn a_dma_buf_pixel_buffer_exports_no_opaque_fd_error(surface_id: &str) -> PyErr {
    PyRuntimeError::new_err(format!(
        "surface {surface_id:?} is a DMA-BUF pixel buffer: `export_dma_buf` hands its memory \
         fd out under that name. Only an OPAQUE_FD allocation exports under this one"
    ))
}

impl HelperCheckedOutSurface {
    pub(crate) fn exchange_client(&self) -> &Arc<HelperProcessGpuExchangeClient> {
        match self {
            Self::PixelBuffer(pixel_surface) => &pixel_surface.exchange_client,
            Self::Texture(texture_surface) => &texture_surface.exchange_client,
            Self::AcquiredDeviceTexture(acquired_texture) => &acquired_texture.exchange_client,
            Self::StorageBuffer(storage_buffer) => &storage_buffer.exchange_client,
        }
    }

    /// A DMA-BUF fd for the surface's first plane plus its byte size,
    /// whichever backing answers.
    pub(crate) fn export_dma_buf(&self) -> PyResult<(RawFd, u64)> {
        match self {
            Self::PixelBuffer(pixel_surface) => pixel_surface.export_dma_buf(),
            Self::Texture(texture_surface) => texture_surface.export_dma_buf(),
            Self::AcquiredDeviceTexture(_) => {
                Err(an_acquired_device_texture_carries_no_exportable_fd_error())
            }
            Self::StorageBuffer(_) => Err(super::a_tensor_surface_is_not_a_pixel_surface()),
        }
    }

    /// The OPAQUE_FD memory fd plus its allocation-stable shape, or the
    /// refusal naming the right door.
    pub(crate) fn export_opaque_fd(&self) -> PyResult<OpaqueFdAllocationExportDescription> {
        match self {
            Self::PixelBuffer(pixel_surface) => pixel_surface.export_opaque_fd(),
            Self::Texture(texture_surface) => texture_surface.export_opaque_fd(),
            Self::AcquiredDeviceTexture(_) => {
                Err(an_acquired_device_texture_carries_no_exportable_fd_error())
            }
            Self::StorageBuffer(_) => Err(super::a_tensor_surface_is_not_a_pixel_surface()),
        }
    }
}

impl HelperProcessGpuExchangeClient {
    /// One request/response over the cached surface-share connection,
    /// reconnecting lazily. The connection is taken out of the slot for the
    /// exchange and put back only on success, so a stream with half a frame
    /// in it is structurally dropped rather than remembered to be.
    pub(super) fn surface_share_request(
        &self,
        request: &serde_json::Value,
    ) -> PyResult<(serde_json::Value, Vec<OwnedFd>)> {
        self.surface_share_request_with_fds(request, &[])
    }

    /// The same exchange with outbound fds riding the request via
    /// SCM_RIGHTS — the adoption path's crossing. The kernel dups each fd
    /// into the service's table; the caller keeps its own.
    fn surface_share_request_with_fds(
        &self,
        request: &serde_json::Value,
        outbound_fds: &[RawFd],
    ) -> PyResult<(serde_json::Value, Vec<OwnedFd>)> {
        let (response, received_fds, ()) =
            self.surface_share_exchange(request, outbound_fds, |_| ())?;
        Ok((response, received_fds))
    }

    /// The one exchange every surface-share request makes, over this
    /// helper's connection; see
    /// [`SurfaceShareConnectionOfThisImporter::exchange`](streamlib_gpu_frame_import_client::SurfaceShareConnectionOfThisImporter::exchange).
    fn surface_share_exchange<DecidedWhileTheConnectionIsHeld>(
        &self,
        request: &serde_json::Value,
        outbound_fds: &[RawFd],
        while_the_connection_is_held: impl FnOnce(&serde_json::Value) -> DecidedWhileTheConnectionIsHeld,
    ) -> PyResult<(
        serde_json::Value,
        Vec<OwnedFd>,
        DecidedWhileTheConnectionIsHeld,
    )> {
        self.surface_share_connection
            .exchange(request, outbound_fds, while_the_connection_is_held)
            .map_err(PyRuntimeError::new_err)
    }

    /// `check_out` over the surface-share socket, then the import of
    /// whichever backing the registration names — or, for a published frame
    /// over a pool slot this helper already imported, that import.
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
        let (response, received_fds, fresh_import_reservation) = self.surface_share_exchange(
            &serde_json::json!({
                "op": "check_out",
                "surface_id": surface_id,
                SURFACE_SHARE_HOLDS_POOL_SLOT_IMPORT_FIELD: holds_pool_slot_import,
            }),
            &[],
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
        let held_against_producer_reuse =
            HelperSurfaceHeldAgainstProducerReuse::CheckOutLease(HelperSurfaceCheckOutLeaseDebt {
                exchange_client: Arc::clone(self),
                surface_id: surface_id.to_string(),
            });
        let pool_slot_import = match held_pool_slot_import {
            Some(held_pool_slot_import) => held_pool_slot_import,
            None => {
                let imported =
                    self.import_checked_out_pool_slot(surface_id, &response, received_fds)?;
                if let Some(fresh_import_reservation) = fresh_import_reservation {
                    let _ = fresh_import_reservation.set(imported.clone());
                }
                imported
            }
        };
        self.frame_checked_out_over_a_pool_slot_import(
            surface_id,
            ProducerImageLayoutOfAFrame::StatedByTheCheckOut(&response),
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
                let (reply, received_fds, fresh_import_reservation) = self.surface_share_exchange(
                    &serde_json::json!({
                        "op": SURFACE_SHARE_IMPORT_POOL_SLOT_OP,
                        "surface_id": pool_slot_key,
                    }),
                    &[],
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
                let imported =
                    self.import_checked_out_pool_slot(pool_slot_key, &reply, received_fds)?;
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
        self.frame_checked_out_over_a_pool_slot_import(
            surface_id,
            ProducerImageLayoutOfAFrame::StatedByItsDescriptor(VulkanLayout(image_layout)),
            pool_slot_import,
            HelperSurfaceHeldAgainstProducerReuse::GpuPoolClaim(claimed),
        )
    }

    /// Validate the checkout metadata and turn the plane fds into this
    /// process's import of the surface — mapped memory for a pixel buffer, an
    /// imported `VkImage` for a texture, the memory fd CUDA imports for a
    /// tensor. The fds are `OwnedFd`s, so every early return closes them by
    /// scope rather than by remembering to.
    fn import_checked_out_pool_slot(
        self: &Arc<Self>,
        surface_id: &str,
        response: &serde_json::Value,
        received_fds: Vec<OwnedFd>,
    ) -> PyResult<HelperPoolSlotImport> {
        // Trailing timeline-semaphore fds arrive after the plane fds when
        // the registration carried them — texture registrations do; a
        // pixel-buffer checkout carries none today. Peeled rather than
        // assumed absent, so a registration that gains them cannot corrupt
        // the plane list.
        let SurfaceShareCheckOutFds {
            plane_fds,
            produce_done_fd,
            consume_done_fd,
        } = SurfaceShareCheckOutFds::split_off_the_timeline_fds(surface_id, response, received_fds)
            .map_err(PyRuntimeError::new_err)?;

        match resource_type_of_a_check_out(response) {
            "texture" => {
                return self
                    .import_checked_out_texture(
                        surface_id,
                        response,
                        plane_fds,
                        produce_done_fd,
                        consume_done_fd,
                    )
                    .map(|imported| HelperPoolSlotImport::Texture(Arc::new(imported)));
            }
            "storage_buffer" => {
                return self
                    .import_checked_out_storage_buffer(surface_id, response, plane_fds)
                    .map(|imported| HelperPoolSlotImport::StorageBuffer(Arc::new(imported)));
            }
            "pixel_buffer" => {}
            other => {
                return Err(PyRuntimeError::new_err(format!(
                    "surface {surface_id:?} is registered as resource type {other:?}, which \
                     this consumer does not know"
                )));
            }
        }

        let CheckedOutPixelBuffer {
            width,
            height,
            format_wire_name,
            plane_sizes,
            bytes_per_row,
            memory: checked_out_memory,
        } = CheckedOutPixelBuffer::from_check_out(surface_id, response, plane_fds)
            .map_err(PyRuntimeError::new_err)?;
        let format = crate::python_processor_context::parse_pixel_format_name(&format_wire_name)?;
        let plane0_size = plane_sizes.first().copied().unwrap_or(0);

        let vulkan_device = self.consumer_vulkan_device()?;
        let (consumer_buffer, memory_flavour) = match checked_out_memory {
            CheckedOutPixelBufferMemory::DmaBuf { plane_fds } => {
                // Refused before the dups exist: a multi-plane refusal from the
                // RHI leaves its dups unclosed (see `close_single_plane_dup_vulkan_refused`).
                if !vulkan_device.supports_dma_buf_memory_import() {
                    return Err(PyRuntimeError::new_err(format!(
                        "surface {surface_id:?} crosses as dma_buf, and this helper's device \
                         ({}) did not enable VK_EXT_external_memory_dma_buf, so it cannot \
                         import it",
                        vulkan_device.name()
                    )));
                }
                let dup_raw_fds = duplicate_plane_fds_for_import(&plane_fds)?;
                let consumer_buffer = ConsumerVulkanBuffer::from_dma_buf_fds(
                    &vulkan_device,
                    &dup_raw_fds,
                    &plane_sizes,
                )
                .map_err(|import_failure| {
                    close_single_plane_dup_vulkan_refused(&dup_raw_fds);
                    PyRuntimeError::new_err(format!(
                        "Vulkan could not import surface {surface_id:?}'s DMA-BUF planes: \
                         {import_failure}"
                    ))
                })?;
                (
                    consumer_buffer,
                    HelperPixelBufferMemoryFlavour::DmaBuf {
                        exportable_plane_fds: plane_fds,
                    },
                )
            }
            CheckedOutPixelBufferMemory::OpaqueFd {
                memory_fd: opaque_memory_fd,
                vk_memory_type_index: stated_memory_type_index,
                exporting_device_uuid,
            } => {
                let export_contract = OpaqueFdExportContract {
                    vk_memory_type_index: stated_memory_type_index,
                    exporting_device_uuid,
                };
                let duplicated_for_import = duplicate_memory_fd(
                    &opaque_memory_fd,
                    format_args!("opaque_fd pixel buffer {surface_id:?}'s"),
                )?;
                let consumer_buffer =
                    ConsumerVulkanBuffer::from_opaque_fd_at_stated_memory_type_index(
                        &vulkan_device,
                        duplicated_for_import,
                        plane0_size,
                        stated_memory_type_index,
                    )
                    .map_err(|import_failure| {
                        PyRuntimeError::new_err(format!(
                            "Vulkan could not import opaque_fd pixel buffer {surface_id:?} at \
                             memory type index {stated_memory_type_index}: {import_failure}"
                        ))
                    })?;
                if consumer_buffer.mapped_ptr().is_null() {
                    return Err(PyRuntimeError::new_err(format!(
                        "opaque_fd pixel buffer {surface_id:?} imported without a host mapping; \
                         the CPU doors read the mapping, so there is nothing to hand out"
                    )));
                }
                (
                    consumer_buffer,
                    HelperPixelBufferMemoryFlavour::OpaqueFd {
                        exportable_memory_fd: opaque_memory_fd,
                        export_contract,
                    },
                )
            }
        };

        Ok(HelperPoolSlotImport::PixelBuffer(Arc::new(
            HelperPixelBufferPoolSlotImport {
                consumer_buffer,
                memory_flavour,
                width,
                height,
                format,
                bytes_per_row,
            },
        )))
    }

    /// One frame checked out over `pool_slot_import`, owing
    /// `release_check_out_to_surface_share`.
    fn frame_checked_out_over_a_pool_slot_import(
        self: &Arc<Self>,
        surface_id: &str,
        producer_image_layout: ProducerImageLayoutOfAFrame<'_>,
        pool_slot_import: HelperPoolSlotImport,
        held_against_producer_reuse: HelperSurfaceHeldAgainstProducerReuse,
    ) -> PyResult<HelperCheckedOutSurface> {
        match pool_slot_import {
            HelperPoolSlotImport::PixelBuffer(pixel_buffer_pool_slot_import) => Ok(
                HelperCheckedOutSurface::PixelBuffer(HelperCheckedOutPixelSurface {
                    surface_id: surface_id.to_string(),
                    width: pixel_buffer_pool_slot_import.width,
                    height: pixel_buffer_pool_slot_import.height,
                    format: pixel_buffer_pool_slot_import.format,
                    bytes_per_row: pixel_buffer_pool_slot_import.bytes_per_row,
                    pixel_buffer_pool_slot_import,
                    release_to_parent: None,
                    unregister_foreign_from_surface_share: None,
                    held_against_producer_reuse,
                    exchange_client: Arc::clone(self),
                }),
            ),
            HelperPoolSlotImport::Texture(texture_pool_slot_import) => self
                .texture_checked_out_over_a_pool_slot_import(
                    surface_id,
                    match producer_image_layout {
                        ProducerImageLayoutOfAFrame::StatedByTheCheckOut(response) => {
                            texture::current_image_layout_of_a_texture_check_out(
                                surface_id, response,
                            )?
                        }
                        ProducerImageLayoutOfAFrame::StatedByItsDescriptor(image_layout) => {
                            image_layout
                        }
                    },
                    texture_pool_slot_import,
                    held_against_producer_reuse,
                )
                .map(HelperCheckedOutSurface::Texture),
            HelperPoolSlotImport::StorageBuffer(storage_buffer_pool_slot_import) => {
                Ok(HelperCheckedOutSurface::StorageBuffer(
                    self.storage_buffer_checked_out_over_a_pool_slot_import(
                        surface_id,
                        storage_buffer_pool_slot_import,
                        held_against_producer_reuse,
                    ),
                ))
            }
        }
    }
}

/// Where a frame's producer states the layout it left a texture in.
enum ProducerImageLayoutOfAFrame<'reply> {
    StatedByTheCheckOut(&'reply serde_json::Value),
    StatedByItsDescriptor(VulkanLayout),
}

/// The OPAQUE_FD pixel-buffer arm against an engine-exported allocation. Needs
/// a Vulkan device and skips where none comes up — the rig runs it on NVIDIA
/// and on lavapipe.
#[cfg(test)]
mod opaque_fd_pixel_buffer_import_tests;

/// The per-slot import cache against engine-exported pool slots. Needs a
/// Vulkan device and skips where none comes up — the rig runs it on NVIDIA
/// and on lavapipe.
#[cfg(test)]
mod pool_slot_import_once_tests;

/// Frames claimed in their pool's claim table against engine-exported pool
/// slots. Needs a Vulkan device and skips where none comes up — the rig runs
/// it on NVIDIA and on lavapipe.
#[cfg(test)]
mod gpu_pool_claim_table_read_tests;
