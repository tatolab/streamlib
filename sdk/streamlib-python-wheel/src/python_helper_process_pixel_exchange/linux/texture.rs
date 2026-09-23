// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::os::fd::{IntoRawFd as _, OwnedFd, RawFd};
use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use streamlib_consumer_rhi::ConsumerVulkanDevice;
use streamlib_consumer_rhi::{
    ConsumerVulkanTexture, ConsumerVulkanTimelineSemaphore, TextureFormat, VulkanLayout,
};

use crate::python_helper_process_pixel_exchange::{
    HelperProcessGpuExchangeClient, HelperSurfaceCheckOutLeaseDebt, HelperSurfaceReleaseDebt,
    escalate_round_trip_to_parent, required_positive_u32_check_out_metadata_field,
};
use crate::python_processor_context::{ExportedVkImageCreationRecipe, OpaqueFdExportContract};

use super::{
    close_single_plane_dup_vulkan_refused, duplicate_first_plane_fd_for_export,
    duplicate_plane_fds_for_import, parse_device_uuid, plane_u64_array_check_out_metadata_field,
    response_field,
};

/// The refusal both export spellings share for a device texture acquired
/// by name: no memory was checked out into this process, so there is no
/// fd to hand out.
pub(super) fn an_acquired_device_texture_carries_no_exportable_fd_error() -> PyErr {
    PyRuntimeError::new_err(
        "this surface is a device texture acquired by name: no memory was checked out \
         into this process, so there is no fd to export. Resolve its surface id to \
         check the texture handle out",
    )
}

/// Absent-defaults for the `vk_image_*` recipe fields, mirroring the
/// surface-share service's documented defaults
/// (`linux/surface_share/state.rs`) — `new_opaque_fd_export`'s hardcoded
/// shape.
const VK_IMAGE_TILING_DEFAULT: i32 = 0; // VK_IMAGE_TILING_OPTIMAL
const VK_IMAGE_MIP_LEVELS_DEFAULT: u32 = 1;
const VK_IMAGE_ARRAY_LAYERS_DEFAULT: u32 = 1;
const VK_IMAGE_SAMPLES_DEFAULT: i32 = 1; // VK_SAMPLE_COUNT_1_BIT
/// `TRANSFER_SRC (0x01) | TRANSFER_DST (0x02) | SAMPLED (0x04) | STORAGE (0x08)`.
const VK_IMAGE_USAGE_DEFAULT: u32 = 0x0F;

/// One `i32` recipe field of a checkout's registration metadata,
/// absent-or-unrepresentable defaulting to the service's documented value.
fn defaulted_i32_check_out_metadata_field(
    response: &serde_json::Value,
    field: &str,
    default: i32,
) -> i32 {
    response
        .get(field)
        .and_then(|value| value.as_i64())
        .and_then(|value| i32::try_from(value).ok())
        .unwrap_or(default)
}

/// The `u32` twin of [`defaulted_i32_check_out_metadata_field`].
fn defaulted_u32_check_out_metadata_field(
    response: &serde_json::Value,
    field: &str,
    default: u32,
) -> u32 {
    response
        .get(field)
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(default)
}

/// Import one timeline edge of a texture checkout, or refuse the checkout
/// naming the edge — a consumer outside the pair is an unsynchronised
/// reader.
fn import_timeline_edge_for_texture_check_out(
    vulkan_device: &Arc<ConsumerVulkanDevice>,
    surface_id: &str,
    edge_name: &str,
    edge_fd: Option<OwnedFd>,
) -> PyResult<Option<ConsumerVulkanTimelineSemaphore>> {
    let Some(edge_fd) = edge_fd else {
        return Ok(None);
    };
    let raw_edge_fd = edge_fd.into_raw_fd();
    match ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd(vulkan_device, raw_edge_fd) {
        Ok(imported_edge) => Ok(Some(imported_edge)),
        Err(import_failure) => {
            // SAFETY: Vulkan takes fd ownership only on success.
            unsafe { libc::close(raw_edge_fd) };
            Err(PyRuntimeError::new_err(format!(
                "texture {surface_id:?}'s {edge_name} timeline would not import \
                 ({import_failure}); a consumer outside the timeline pair is an \
                 unsynchronised reader"
            )))
        }
    }
}

/// The registration metadata a texture checkout must carry, parsed and
/// validated before any fd changes hands — a refusal here leaves no
/// duplicated fd behind.
struct TextureCheckOutRegistrationMetadata {
    width: u32,
    height: u32,
    format: TextureFormat,
    handle_is_opaque_fd: bool,
    allocation_byte_size: u64,
    current_image_layout: VulkanLayout,
    drm_format_modifier: u64,
    plane_offsets: Vec<u64>,
    plane_strides: Vec<u64>,
    vk_image_creation_recipe: ExportedVkImageCreationRecipe,
    /// `Some` on every OPAQUE_FD registration (refused otherwise),
    /// `None` for DMA-BUF flavours, which never carry it.
    opaque_fd_export_contract: Option<OpaqueFdExportContract>,
}

impl TextureCheckOutRegistrationMetadata {
    fn from_check_out_response(surface_id: &str, response: &serde_json::Value) -> PyResult<Self> {
        let width = required_positive_u32_check_out_metadata_field(response, surface_id, "width")?;
        let height =
            required_positive_u32_check_out_metadata_field(response, surface_id, "height")?;
        let format_wire_name = response
            .get("format")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        let format = TextureFormat::from_wire_name(format_wire_name).ok_or_else(|| {
            PyRuntimeError::new_err(format!(
                "texture {surface_id:?} is registered with format {format_wire_name:?}, \
                 which this consumer does not know"
            ))
        })?;
        let handle_is_opaque_fd = match response
            .get("handle_type")
            .and_then(|value| value.as_str())
            .unwrap_or("dma_buf")
        {
            "opaque_fd" => true,
            "dma_buf" => false,
            other => {
                return Err(PyRuntimeError::new_err(format!(
                    "texture {surface_id:?} is registered with handle type {other:?}, which \
                     this consumer does not know"
                )));
            }
        };
        let allocation_byte_size = response
            .get("vk_image_allocation_size")
            .and_then(|value| value.as_u64())
            .filter(|size| *size > 0)
            .ok_or_else(|| {
                PyRuntimeError::new_err(format!(
                    "texture {surface_id:?} was registered without its allocation byte size; \
                     binding imported memory of unknown extent reads past the allocation \
                     instead of failing here"
                ))
            })?;
        let current_image_layout_raw = response
            .get("current_image_layout")
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        let current_image_layout = i32::try_from(current_image_layout_raw)
            .map(VulkanLayout)
            .map_err(|_| {
                PyRuntimeError::new_err(format!(
                    "texture {surface_id:?} reports image layout {current_image_layout_raw}, \
                     which is not a VkImageLayout"
                ))
            })?;
        let drm_format_modifier = response
            .get("drm_format_modifier")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        if !handle_is_opaque_fd && drm_format_modifier == 0 {
            return Err(PyRuntimeError::new_err(format!(
                "texture {surface_id:?} was registered without an explicit DRM modifier: its \
                 image layout is driver-opaque and no other process can reconstruct it. \
                 Acquire the texture with a cross-process-importable flavour (a \
                 render-attachment usage, or a CUDA-mappable format within the OPAQUE_FD \
                 usage set)"
            )));
        }
        let vk_memory_type_index = response
            .get("vk_memory_type_index")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok());
        if handle_is_opaque_fd && vk_memory_type_index.is_none() {
            return Err(PyRuntimeError::new_err(format!(
                "texture {surface_id:?} was registered without its exporter's memory type \
                 index; a conforming OPAQUE_FD import binds a stated memory type, never a \
                 guessed one"
            )));
        }
        let exporting_device_uuid = response
            .get("exporting_device_uuid")
            .and_then(|value| value.as_str())
            .map(parse_device_uuid)
            .transpose()?;
        if handle_is_opaque_fd && exporting_device_uuid.is_none() {
            return Err(PyRuntimeError::new_err(format!(
                "texture {surface_id:?} was registered without its exporting device UUID; \
                 importing onto the wrong GPU of a multi-GPU rig reads the wrong memory \
                 instead of failing here"
            )));
        }
        let opaque_fd_export_contract = vk_memory_type_index.zip(exporting_device_uuid).map(
            |(vk_memory_type_index, exporting_device_uuid)| OpaqueFdExportContract {
                vk_memory_type_index,
                exporting_device_uuid,
            },
        );
        Ok(Self {
            width,
            height,
            format,
            handle_is_opaque_fd,
            allocation_byte_size,
            current_image_layout,
            drm_format_modifier,
            plane_offsets: plane_u64_array_check_out_metadata_field(response, "plane_offsets"),
            plane_strides: plane_u64_array_check_out_metadata_field(response, "plane_strides"),
            vk_image_creation_recipe: ExportedVkImageCreationRecipe {
                vk_image_tiling: defaulted_i32_check_out_metadata_field(
                    response,
                    "vk_image_tiling",
                    VK_IMAGE_TILING_DEFAULT,
                ),
                vk_image_usage_flags: defaulted_u32_check_out_metadata_field(
                    response,
                    "vk_image_usage",
                    VK_IMAGE_USAGE_DEFAULT,
                ),
                vk_image_mip_levels: defaulted_u32_check_out_metadata_field(
                    response,
                    "vk_image_mip_levels",
                    VK_IMAGE_MIP_LEVELS_DEFAULT,
                ),
                vk_image_array_layers: defaulted_u32_check_out_metadata_field(
                    response,
                    "vk_image_array_layers",
                    VK_IMAGE_ARRAY_LAYERS_DEFAULT,
                ),
                vk_image_samples: defaulted_i32_check_out_metadata_field(
                    response,
                    "vk_image_samples",
                    VK_IMAGE_SAMPLES_DEFAULT,
                ),
            },
            opaque_fd_export_contract,
        })
    }
}

/// A pooled texture the parent acquired on this child's behalf.
///
/// Deliberately not a [`HelperCheckedOutPixelSurface`]: no fds were checked
/// out and no memory is mapped here. What this carries is the name a dispatch
/// binds and a downstream processor resolves, the debt that hands the pool
/// slot back, and the client its device-tensor scope reaches the parent's
/// export staging through.
pub(crate) struct HelperAcquiredTexture {
    pub(crate) surface_id: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: TextureFormat,
    /// Settled by its own Drop, via the owned memory that carries this
    /// value — so a tensor outliving its handle keeps the pool slot too.
    #[expect(
        dead_code,
        reason = "the field is the release; its Drop pays the parent"
    )]
    pub(crate) release_to_parent: HelperSurfaceReleaseDebt,
    pub(crate) exchange_client: Arc<HelperProcessGpuExchangeClient>,
}

/// A texture-backed surface this helper imported: the engine's tiled image
/// reconstructed as a `VkImage` on this process's own device, plus the
/// timeline edges and the layout cell that keep the crossing coordinated.
///
/// The memory is tiled DEVICE_LOCAL, so this arm maps nothing: what it
/// offers is the image itself for this process's Vulkan work, and the fds it
/// was checked out with for native code that imports memory. The CPU reaches
/// these pixels through the surface's host-visible export staging, which is
/// a different allocation and a different checkout.
pub(crate) struct HelperCheckedOutTextureSurface {
    pub(crate) surface_id: String,
    pub(crate) consumer_texture: ConsumerVulkanTexture,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: TextureFormat,
    /// True when the registration's handle type is OPAQUE_FD — the fds then
    /// import through Vulkan/CUDA external memory, never as DMA-BUFs.
    pub(crate) handle_is_opaque_fd: bool,
    /// The host allocation's byte size, from the registration — what a
    /// native import must pass to its own allocator.
    pub(crate) allocation_byte_size: u64,
    /// The producer's edge of the single-writer-per-edge pair, when the
    /// registration carried one. Nothing signals it today — the escalate
    /// dispatch retires its GPU work before any consumer can learn the id —
    /// so there is no wire value to wait on; the import holds the edge so a
    /// producer that starts signalling finds its consumers already on the
    /// pair.
    #[expect(
        dead_code,
        reason = "no wire value exists to wait on yet; held for the pair's producer side"
    )]
    produce_done_timeline: Option<ConsumerVulkanTimelineSemaphore>,
    /// This side's edge, signalled once at release.
    consume_done_timeline: Option<ConsumerVulkanTimelineSemaphore>,
    /// The layout this side's image sits in — seeded from the checkout's
    /// published cell, republished at release so the next consumer's acquire
    /// barrier names the right source.
    current_image_layout: VulkanLayout,
    /// The checkout lease this surface owes the surface-share service.
    #[expect(
        dead_code,
        reason = "settled by its own Drop; nothing reads it, and that is the point"
    )]
    release_check_out_to_surface_share: HelperSurfaceCheckOutLeaseDebt,
    /// The plane fds this checkout was delivered, kept so `export_dma_buf`
    /// and `export_opaque_fd` can hand the texture itself to native code.
    exported_plane_fds: Vec<OwnedFd>,
    /// The VkImageCreateInfo recipe off the registration — what
    /// `export_opaque_fd` states so a foreign re-import reproduces the
    /// exporter's image byte-for-byte.
    vk_image_creation_recipe: ExportedVkImageCreationRecipe,
    /// `Some` on every OPAQUE_FD checkout (the parse refused otherwise),
    /// `None` for DMA-BUF flavours, which never carry it.
    opaque_fd_export_contract: Option<OpaqueFdExportContract>,
    pub(crate) exchange_client: Arc<HelperProcessGpuExchangeClient>,
}

/// What `export_opaque_fd` hands across: the freshly dup'd memory fd —
/// caller-owned from this moment — plus the allocation-stable shape a
/// foreign Vulkan or CUDA external-memory import must reproduce.
pub(crate) struct OpaqueFdTextureExportDescription {
    pub(crate) exported_memory_fd: OwnedFd,
    pub(crate) allocation_byte_size: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format_wire_name: &'static str,
    pub(crate) vk_image_creation_recipe: ExportedVkImageCreationRecipe,
    pub(crate) dedicated_allocation: bool,
    pub(crate) export_contract: OpaqueFdExportContract,
}

impl HelperCheckedOutTextureSurface {
    /// A DMA-BUF fd for the texture's first plane and the allocation's
    /// byte size — the handle itself, not a linear view of it.
    ///
    /// Refused by name for an OPAQUE_FD-flavoured texture, pointing at
    /// `export_opaque_fd`: that fd is not a DMA-BUF, and handing it out
    /// under this name would fail at the receiver's EGL or V4L2 import
    /// with a driver error instead of here.
    pub(crate) fn export_dma_buf(&self) -> PyResult<(RawFd, u64)> {
        if self.handle_is_opaque_fd {
            return Err(PyRuntimeError::new_err(
                "this texture's memory is OPAQUE_FD-flavoured: it imports through Vulkan or \
                 CUDA external memory, not as a DMA-BUF, and `export_opaque_fd` hands its \
                 handle out under its own name. Only an explicit-DRM-modifier DMA-BUF \
                 texture exports under this one",
            ));
        }
        let exported = duplicate_first_plane_fd_for_export(&self.exported_plane_fds, "texture")?;
        Ok((exported.into_raw_fd(), self.allocation_byte_size))
    }

    /// The OPAQUE_FD memory fd plus the allocation-stable shape a foreign
    /// Vulkan or CUDA external-memory import must reproduce.
    ///
    /// The mirror of the refusal above: a DMA-BUF-flavoured texture is
    /// refused by name, pointing at `export_dma_buf`.
    pub(crate) fn export_opaque_fd(&self) -> PyResult<OpaqueFdTextureExportDescription> {
        if !self.handle_is_opaque_fd {
            return Err(PyRuntimeError::new_err(
                "this texture's memory is DMA-BUF-flavoured: `export_dma_buf` hands its fd \
                 out under that name. Only an OPAQUE_FD texture — the flavour storage-usage \
                 kernel outputs take — exports under this one",
            ));
        }
        let export_contract = self.opaque_fd_export_contract.ok_or_else(|| {
            PyRuntimeError::new_err(
                "this OPAQUE_FD checkout carries no export contract fields; its \
                 registration predates the raw-handle export contract",
            )
        })?;
        let exported_memory_fd =
            duplicate_first_plane_fd_for_export(&self.exported_plane_fds, "texture")?;
        Ok(OpaqueFdTextureExportDescription {
            exported_memory_fd,
            allocation_byte_size: self.allocation_byte_size,
            width: self.width,
            height: self.height,
            format_wire_name: self.format.wire_name(),
            vk_image_creation_recipe: self.vk_image_creation_recipe,
            // DEDICATED_MEMORY by construction for the flavour
            // (`HostVulkanTexture::new_opaque_fd_export`); a Vulkan importer
            // chains `VkMemoryDedicatedAllocateInfo`, a CUDA importer sets
            // `cudaExternalMemoryDedicated`.
            dedicated_allocation: true,
            export_contract,
        })
    }
}

impl Drop for HelperCheckedOutTextureSurface {
    /// The release half of the crossing: signal this side's consume edge,
    /// hand queue-family ownership back, republish the layout. Best-effort
    /// throughout — a parent that is gone reclaims everything with the
    /// connection — and the lease debt field settles after this body.
    ///
    /// The QFOT release is a fence wait and the publish is a socket round
    /// trip, so both run detached — this can be a capsule deleter running
    /// under the child's GIL, the same hazard every debt Drop here names.
    fn drop(&mut self) {
        Python::attach(|python| {
            let release_failures = python.detach(|| {
                let mut release_failures: Vec<String> = Vec::new();
                if let Some(consume_done) = &self.consume_done_timeline {
                    let signalled = consume_done
                        .current_value()
                        .and_then(|value| consume_done.signal_host(value + 1));
                    if let Err(signal_failure) = signalled {
                        release_failures
                            .push(format!("consume_done signal failed: {signal_failure}"));
                    }
                }
                if self.current_image_layout != VulkanLayout::UNDEFINED {
                    if let Err(barrier_failure) = self.consumer_texture.release_to_foreign_layout(
                        self.current_image_layout,
                        self.current_image_layout,
                    ) {
                        release_failures.push(format!("QFOT release failed: {barrier_failure}"));
                    }
                    if let Err(publish_failure) =
                        self.exchange_client.publish_image_layout_to_surface_share(
                            &self.surface_id,
                            self.current_image_layout.0,
                        )
                    {
                        release_failures.push(format!("layout publish failed: {publish_failure}"));
                    }
                }
                release_failures
            });
            if !release_failures.is_empty() {
                tracing::warn!(
                    "releasing texture surface {} left the crossing uncoordinated ({}); the \
                     service reclaims the claim when this helper's connection closes",
                    self.surface_id,
                    release_failures.join("; ")
                );
            }
        });
    }
}

impl HelperProcessGpuExchangeClient {
    /// Acquire a pooled texture, and take back the surface id the parent
    /// minted for it plus the extent it actually allocated.
    ///
    /// Nothing is imported: the id is what a kernel dispatch binds and what a
    /// downstream processor resolves. Mapping the texture's memory into this
    /// process is a separate capability.
    pub(crate) fn acquire_texture(
        self: &Arc<Self>,
        python: Python<'_>,
        width: u32,
        height: u32,
        wire_format_name: &str,
        usage: &[String],
    ) -> PyResult<HelperAcquiredTexture> {
        let op = PyDict::new(python);
        op.set_item("op", "acquire_texture")?;
        op.set_item("width", width)?;
        op.set_item("height", height)?;
        op.set_item("format", wire_format_name)?;
        op.set_item("usage", usage)?;
        let response =
            escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        let surface_id: String = response_field(&response, "handle_id")?.extract()?;
        // The debt exists from the moment the parent allocated: dropping it
        // hands the pool slot back rather than stranding it. Bound before the
        // metadata extraction below, so a malformed response still pays the
        // release — the same ordering `acquire_pixel_buffer` documents.
        let release_to_parent = HelperSurfaceReleaseDebt {
            release_to_parent_without_waiting: self
                .release_to_parent_without_waiting
                .clone_ref(python),
            handle_id: surface_id.clone(),
        };
        let format_wire_name: String = response_field(&response, "format")?.extract()?;
        let format = TextureFormat::from_wire_name(&format_wire_name).ok_or_else(|| {
            PyRuntimeError::new_err(format!(
                "the parent's acquire_texture response named an unknown format \
                 {format_wire_name:?}"
            ))
        })?;
        Ok(HelperAcquiredTexture {
            width: response_field(&response, "width")?.extract()?,
            height: response_field(&response, "height")?.extract()?,
            format,
            release_to_parent,
            exchange_client: Arc::clone(self),
            surface_id,
        })
    }

    /// The texture arm of a checkout: parse and validate the registration,
    /// rebuild the engine's tiled image on this process's device from the
    /// flavour it carries, join the timeline pair, and take the published
    /// layout as this side's starting point.
    pub(super) fn import_checked_out_texture(
        self: &Arc<Self>,
        surface_id: &str,
        response: &serde_json::Value,
        plane_fds: Vec<OwnedFd>,
        produce_done_fd: Option<OwnedFd>,
        consume_done_fd: Option<OwnedFd>,
    ) -> PyResult<HelperCheckedOutTextureSurface> {
        let metadata =
            TextureCheckOutRegistrationMetadata::from_check_out_response(surface_id, response)?;
        // Both flavours travel as exactly one memory fd — OPAQUE_FD is the
        // whole allocation, and the engine's DMA-BUF texture export is one
        // fd with per-plane offsets. Refused before any dup exists to leak:
        // the import consumes only the first fd, so a foreign registration
        // shipping more would leak one dup per extra plane per checkout.
        if plane_fds.len() != 1 {
            return Err(PyRuntimeError::new_err(format!(
                "texture {surface_id:?} arrived with {} fds; a texture checkout carries \
                 its memory as exactly one",
                plane_fds.len()
            )));
        }

        let vulkan_device = self.consumer_vulkan_device()?;
        let dup_raw_fds = duplicate_plane_fds_for_import(&plane_fds)?;
        let imported = match (metadata.handle_is_opaque_fd, dup_raw_fds.as_slice()) {
            (true, &[opaque_memory_fd]) => ConsumerVulkanTexture::from_opaque_fd(
                &vulkan_device,
                opaque_memory_fd,
                metadata.width,
                metadata.height,
                metadata.format,
                metadata.allocation_byte_size,
            ),
            // Unreachable past the arity refusal above; destructured rather
            // than indexed so a broken invariant refuses instead of panicking.
            (true, _) => {
                close_single_plane_dup_vulkan_refused(&dup_raw_fds);
                return Err(PyRuntimeError::new_err(format!(
                    "opaque_fd texture {surface_id:?} duplicated {} fds where its whole \
                     allocation travels as exactly one",
                    dup_raw_fds.len()
                )));
            }
            (false, _) => ConsumerVulkanTexture::import_render_target_dma_buf(
                &vulkan_device,
                &dup_raw_fds,
                &metadata.plane_offsets,
                &metadata.plane_strides,
                metadata.drm_format_modifier,
                metadata.width,
                metadata.height,
                metadata.format,
                metadata.allocation_byte_size,
            ),
        };
        let consumer_texture = imported.map_err(|import_failure| {
            close_single_plane_dup_vulkan_refused(&dup_raw_fds);
            PyRuntimeError::new_err(format!(
                "Vulkan could not import texture {surface_id:?}: {import_failure}"
            ))
        })?;

        let produce_done_timeline = import_timeline_edge_for_texture_check_out(
            &vulkan_device,
            surface_id,
            "produce_done",
            produce_done_fd,
        )?;
        let consume_done_timeline = import_timeline_edge_for_texture_check_out(
            &vulkan_device,
            surface_id,
            "consume_done",
            consume_done_fd,
        )?;

        // The consumer-side acquire barrier per the layout protocol: start
        // this side's layout tracking from the layout the producer
        // published. A no-op when the published layout is UNDEFINED — which
        // it is until a producer publishes one; see the field's doc.
        consumer_texture
            .acquire_from_foreign_layout(metadata.current_image_layout)
            .map_err(|acquire_failure| {
                PyRuntimeError::new_err(format!(
                    "the QFOT acquire barrier for texture {surface_id:?} failed \
                     ({acquire_failure}); an image whose layout tracking never started is \
                     not usable"
                ))
            })?;

        Ok(HelperCheckedOutTextureSurface {
            surface_id: surface_id.to_string(),
            consumer_texture,
            width: metadata.width,
            height: metadata.height,
            format: metadata.format,
            handle_is_opaque_fd: metadata.handle_is_opaque_fd,
            allocation_byte_size: metadata.allocation_byte_size,
            produce_done_timeline,
            consume_done_timeline,
            current_image_layout: metadata.current_image_layout,
            release_check_out_to_surface_share: HelperSurfaceCheckOutLeaseDebt {
                exchange_client: Arc::clone(self),
                surface_id: surface_id.to_string(),
            },
            exported_plane_fds: plane_fds,
            vk_image_creation_recipe: metadata.vk_image_creation_recipe,
            opaque_fd_export_contract: metadata.opaque_fd_export_contract,
            exchange_client: Arc::clone(self),
        })
    }
}

/// The checkout parse's raw-handle-export-contract arms. CI-protected:
/// the wheel's device tests are `requires_gpu`, so the refusal texts and
/// the flavour scoping have to hold here or they are not protected
/// anywhere.
#[cfg(test)]
mod texture_check_out_registration_metadata_tests;
