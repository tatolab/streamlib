// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::os::fd::{AsRawFd as _, IntoRawFd as _, OwnedFd};
use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use streamlib::sdk::rhi::PixelFormat;
use streamlib_consumer_rhi::ConsumerVulkanBuffer;
use streamlib_consumer_rhi::ConsumerVulkanTimelineSemaphore;

use crate::python_cuda_pixel_exchange::CudaImportedSurface;
use crate::python_helper_process_pixel_exchange::{
    HelperProcessGpuExchangeClient, escalate_round_trip_to_parent,
    refuse_check_out_the_service_declined,
};

use super::{decimal_string_field, parse_device_uuid, response_field};

/// What the parent answered when asked to open a surface's device export.
///
/// A struct rather than eight positional arguments: `staging_byte_size`
/// and `bytes_per_row` are both `u64` and `width`/`height` are both `u32`,
/// so a transposition compiles clean and lands as a wrong-sized CUDA
/// import or a wrong stride.
struct DeviceExportStagingDescription {
    /// The surface-share id the staging and its timeline are published
    /// under — what the check-out names, not the source surface's id.
    staging_share_id: String,
    staging_byte_size: u64,
    exporting_device_uuid: [u8; 16],
    width: u32,
    height: u32,
    format: PixelFormat,
    bytes_per_row: u64,
    writable: bool,
}

/// The memory type index an OPAQUE_FD staging registration states.
///
/// Mandatory, not defensive: a conforming OPAQUE_FD import binds the
/// index the *exporter* allocated from, and the handle type has no
/// fd-properties query to discover it with. A registration without one
/// would leave the import guessing, and a guess binds the wrong memory
/// type rather than failing — silently, until the pixels are wrong.
fn memory_type_index_stated_by_a_staging_registration(
    staging_kind: &str,
    staging_share_id: &str,
    registration: &serde_json::Value,
) -> PyResult<u32> {
    registration
        .get("vk_memory_type_index")
        .and_then(serde_json::Value::as_u64)
        .and_then(|index| u32::try_from(index).ok())
        .ok_or_else(|| {
            crate::python_processor_context::gpu_operation_error(format!(
                "the {staging_kind} staging {staging_share_id:?} was registered without a usable \
                 vk_memory_type_index; an OPAQUE_FD import must bind the memory type index the \
                 exporter allocated from, and guessing one binds the wrong memory instead of \
                 failing"
            ))
        })
}

/// A published export staging, checked out and validated, with its copy
/// timeline already imported — everything both export arms need before
/// they diverge on how the memory itself is imported.
struct CheckedOutExportStaging {
    /// The memory type index the exporter allocated from, off the
    /// registration. Validated here rather than per arm: it is mandatory
    /// on exactly the OPAQUE_FD flavour this checkout already insisted on,
    /// so one wire contract is checked in one place.
    stated_memory_type_index: u32,
    /// Handed to the memory import, which adopts it; never closed here.
    staging_fd: OwnedFd,
    copy_done: ConsumerVulkanTimelineSemaphore,
}

/// What a helper process holds of a surface's device export: CUDA's
/// import of the parent's staging buffer, and the timeline every refill
/// signals.
///
/// The staging itself belongs to the parent's `GpuContext` and is cached
/// there per surface. This is the consumer half — one import per surface
/// per child, memoised on the exchange client, because
/// `cudaImportExternalMemory` is not a per-frame cost.
pub(crate) struct HelperDeviceExport {
    pub(crate) cuda_import: Arc<CudaImportedSurface>,
    refill_done: ConsumerVulkanTimelineSemaphore,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: PixelFormat,
    pub(crate) bytes_per_row: u64,
    pub(crate) writable: bool,
}

/// What a helper process holds of a surface's CPU-readback export: this
/// child's mapped import of the parent's host-visible staging, and the
/// timeline every copy signals.
///
/// The mapped sibling of [`HelperDeviceExport`] — same staging machinery,
/// same surface-share transport, same per-pool-slot memoisation — holding
/// a `ConsumerVulkanBuffer` where the device arm holds a CUDA import,
/// because the reader here is numpy and the child needs no GPU package to
/// have one. Pixel bytes never cross the escalate socket: this mapping is
/// how they are reached.
pub(crate) struct HelperCpuReadbackExport {
    pub(crate) consumer_buffer: ConsumerVulkanBuffer,
    copy_done: ConsumerVulkanTimelineSemaphore,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: PixelFormat,
    pub(crate) bytes_per_row: u64,
    pub(crate) writable: bool,
}

/// Which way one CPU-readback staging copy runs.
///
/// The readback op carries its direction as a wire field, where the device
/// pair spells each direction its own op name.
#[derive(Clone, Copy)]
pub(crate) enum CpuReadbackCopyDirection {
    /// The frame's current pixels into the staging — what entering the CPU
    /// door always runs, a pure write included. Why it is unconditional is
    /// at
    /// [`read_the_frame_into_its_cpu_staging`](crate::python_gpu_surface_pixel_exchange::read_the_frame_into_its_cpu_staging).
    SurfaceIntoStaging,
    /// The staged edit back into the surface's own allocation, so every
    /// other holder observes it.
    StagingBackIntoSurface,
}

impl CpuReadbackCopyDirection {
    fn wire_name(self) -> &'static str {
        match self {
            Self::SurfaceIntoStaging => "image_to_buffer",
            Self::StagingBackIntoSurface => "buffer_to_image",
        }
    }
}

/// Which of the engine's two export-staging residencies a helper is
/// opening — the child-side mirror of the parent's own
/// `SurfaceExportStagingResidency`.
///
/// Carried rather than spelled at each call so the wire op name and the
/// noun refusals use cannot drift apart between the two arms.
#[derive(Clone, Copy)]
pub(crate) enum HelperExportStagingResidency {
    /// Device-local, consumed by an external device API's import.
    DeviceLocal,
    /// Host-visible, mapped into this child and read by the CPU doors.
    HostVisible,
}

impl HelperExportStagingResidency {
    fn escalate_open_op_name(self) -> &'static str {
        match self {
            Self::DeviceLocal => "open_device_export_staging",
            Self::HostVisible => "open_cpu_readback_staging",
        }
    }

    /// How a refusal names the staging it is about.
    fn named_in_refusals(self) -> &'static str {
        match self {
            Self::DeviceLocal => "device-export",
            Self::HostVisible => "readback",
        }
    }
}

/// Bound on the wait for a refill the parent said it signalled. The copy
/// is VRAM→VRAM; reaching this bound means the parent's queue is wedged,
/// not that the copy is slow.
const DEVICE_EXPORT_REFILL_WAIT_TIMEOUT_NS: u64 = 2_000_000_000;

impl HelperProcessGpuExchangeClient {
    /// Open this surface's device export, importing the parent's staging
    /// into CUDA on first ask and memoising it for this child.
    ///
    /// Two channels again, same division as the host path: escalate asks
    /// the parent to allocate and publish the staging, and the
    /// surface-share check-out carries the memory — the staging's
    /// OPAQUE_FD and the refill timeline's fd, in that order.
    pub(crate) fn open_device_export(
        &self,
        python: Python<'_>,
        surface_id: &str,
    ) -> PyResult<Arc<HelperDeviceExport>> {
        // Memoised per pool slot: the parent's staging (and this CUDA
        // import of it) spans every frame the slot publishes, while each
        // refill names — and the parent validates — the specific frame id.
        let source_pool_slot_key = streamlib::sdk::rhi::pool_slot_key_of_surface_id(surface_id);
        if let Some(already_open) = self
            .device_exports_by_surface
            .lock()
            .get(source_pool_slot_key)
        {
            return Ok(Arc::clone(already_open));
        }

        let described = self.ask_the_parent_to_publish_the_export_staging(
            python,
            HelperExportStagingResidency::DeviceLocal,
            surface_id,
            source_pool_slot_key,
        )?;
        let opened = python.detach(|| -> PyResult<Arc<HelperDeviceExport>> {
            Ok(Arc::new(
                self.check_out_and_import_device_export(&described)?,
            ))
        })?;
        Ok(Arc::clone(
            self.device_exports_by_surface
                .lock()
                .entry(source_pool_slot_key.to_string())
                .or_insert(opened),
        ))
    }

    /// Open this surface's CPU-readback export, checking the parent's
    /// host-visible staging out and mapping it on first ask, memoised for
    /// this child.
    ///
    /// The readback twin of [`Self::open_device_export`] — same two
    /// channels, same per-pool-slot memo — reached by every CPU door over
    /// a texture-backed surface. It maps; it runs no copy: the read-in is
    /// the door's, because only the door knows which frame it is about to
    /// hand out.
    pub(crate) fn open_cpu_readback_export(
        &self,
        python: Python<'_>,
        surface_id: &str,
    ) -> PyResult<Arc<HelperCpuReadbackExport>> {
        let source_pool_slot_key = streamlib::sdk::rhi::pool_slot_key_of_surface_id(surface_id);
        if let Some(already_open) = self
            .cpu_readback_exports_by_pool_slot
            .lock()
            .get(source_pool_slot_key)
        {
            return Ok(Arc::clone(already_open));
        }

        let described = self.ask_the_parent_to_publish_the_export_staging(
            python,
            HelperExportStagingResidency::HostVisible,
            surface_id,
            source_pool_slot_key,
        )?;
        let opened = python.detach(|| -> PyResult<Arc<HelperCpuReadbackExport>> {
            Ok(Arc::new(
                self.check_out_and_import_cpu_readback_export(&described)?,
            ))
        })?;
        Ok(Arc::clone(
            self.cpu_readback_exports_by_pool_slot
                .lock()
                .entry(source_pool_slot_key.to_string())
                .or_insert(opened),
        ))
    }

    /// The readback export this child already opened for `surface_id`, or
    /// `None` if no CPU door has opened one.
    ///
    /// A memo read: no round trip, no import, and no GIL — which is what
    /// lets the plane view stay a pure question the accessors can ask
    /// detached.
    pub(crate) fn cpu_readback_export_already_open(
        &self,
        surface_id: &str,
    ) -> Option<Arc<HelperCpuReadbackExport>> {
        self.cpu_readback_exports_by_pool_slot
            .lock()
            .get(streamlib::sdk::rhi::pool_slot_key_of_surface_id(surface_id))
            .map(Arc::clone)
    }

    /// Whether an edit written back into `surface_id` publishes at all —
    /// the engine's own answer, from the same mint-time derivation the
    /// export stagings carry: a write-back belongs to a pooled frame whose
    /// allocation is its only backing, or to a registered texture that
    /// takes a recorded copy in; a frame its producer still owns answers
    /// no.
    ///
    /// Seeded by the device door when it opens first — `open_device_export`
    /// records the same mint-time answer — and otherwise asked over the
    /// CPU-readback staging op, the residency with no CUDA anywhere in it;
    /// the staging the parent mints to answer is cached engine-side per
    /// pool slot, and the answer is memoised here on the same key, so this
    /// costs at most one round trip per slot lifetime.
    pub(crate) fn surface_can_take_write_back(
        &self,
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
        let op = PyDict::new(python);
        op.set_item("op", "open_cpu_readback_staging")?;
        op.set_item("surface_id", surface_id)?;
        let response = escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)
            .map_err(|failure| {
                PyRuntimeError::new_err(format!(
                    "could not ask the engine whether surface {surface_id:?} takes a \
                         write-back: {failure}"
                ))
            })?;
        let can_take_write_back: bool = response_field(&response, "writable")?.extract()?;
        self.write_back_answers_by_pool_slot
            .lock()
            .insert(source_pool_slot_key.to_string(), can_take_write_back);
        Ok(can_take_write_back)
    }

    /// Ask the parent to run one device-export copy and wait for the
    /// timeline value it answers with.
    ///
    /// The wait is the whole point of the round trip: the parent's own
    /// post-submit wait orders nothing for this process, so a read that
    /// skipped this would race the copy it asked for.
    pub(crate) fn run_device_export_copy(
        &self,
        python: Python<'_>,
        escalate_op: &str,
        surface_id: &str,
        export: &HelperDeviceExport,
    ) -> PyResult<()> {
        let op = PyDict::new(python);
        op.set_item("op", escalate_op)?;
        op.set_item("surface_id", surface_id)?;
        self.send_the_staging_copy_and_wait_for_its_timeline_value(
            python,
            &op,
            escalate_op,
            surface_id,
            &export.refill_done,
        )
    }

    /// Ask the parent to run one CPU-readback staging copy and wait for
    /// the timeline value it answers with.
    ///
    /// The readback twin of [`Self::run_device_export_copy`], differing
    /// only in that one wire op serves both directions, so the direction
    /// travels as a field.
    pub(crate) fn run_cpu_readback_copy(
        &self,
        python: Python<'_>,
        direction: CpuReadbackCopyDirection,
        surface_id: &str,
        export: &HelperCpuReadbackExport,
    ) -> PyResult<()> {
        let op = PyDict::new(python);
        op.set_item("op", "run_cpu_readback_copy")?;
        op.set_item("surface_id", surface_id)?;
        op.set_item("direction", direction.wire_name())?;
        self.send_the_staging_copy_and_wait_for_its_timeline_value(
            python,
            &op,
            "run_cpu_readback_copy",
            surface_id,
            &export.copy_done,
        )
    }

    /// The round trip both staging-copy arms share: send the op, then wait
    /// on this child's imported timeline for the value the parent signalled.
    fn send_the_staging_copy_and_wait_for_its_timeline_value(
        &self,
        python: Python<'_>,
        op: &Bound<'_, PyDict>,
        escalate_op: &str,
        surface_id: &str,
        copy_done: &ConsumerVulkanTimelineSemaphore,
    ) -> PyResult<()> {
        let response = escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, op)?;
        let signalled = decimal_string_field(&response, "timeline_value")?;
        python.detach(|| {
            copy_done
                .wait(signalled, DEVICE_EXPORT_REFILL_WAIT_TIMEOUT_NS)
                .map_err(|wait_failure| {
                    crate::python_processor_context::gpu_operation_error(format!(
                        "waiting for {escalate_op} of surface {surface_id:?} to reach timeline \
                         value {signalled} failed: {wait_failure}"
                    ))
                })
        })
    }

    /// Check the published staging out and import it: the memory into
    /// CUDA, the timeline into this child's Vulkan device.
    fn check_out_and_import_device_export(
        &self,
        described: &DeviceExportStagingDescription,
    ) -> PyResult<HelperDeviceExport> {
        let checked_out = self.check_out_the_published_export_staging(
            described,
            HelperExportStagingResidency::DeviceLocal,
        )?;
        let cuda_import = crate::python_cuda_pixel_exchange::import_opaque_fd_into_cuda(
            checked_out.staging_fd,
            described.staging_byte_size,
            described.exporting_device_uuid,
        )
        .map(Arc::new)
        .map_err(crate::python_processor_context::gpu_operation_error)?;

        Ok(HelperDeviceExport {
            cuda_import,
            refill_done: checked_out.copy_done,
            width: described.width,
            height: described.height,
            format: described.format,
            bytes_per_row: described.bytes_per_row,
            writable: described.writable,
        })
    }

    /// Check the published readback staging out and map it: the memory as
    /// a host-visible `VkBuffer` on this child's device, the timeline
    /// beside it.
    ///
    /// The memory binds the **exporter's** memory type index, off the
    /// registration — a conforming OPAQUE_FD import has no fd-properties
    /// query to discover it, and a first-match guess agrees with the host
    /// only by coincidence once the engine allocates readback stagings
    /// from a cached pool the write paths don't use.
    fn check_out_and_import_cpu_readback_export(
        &self,
        described: &DeviceExportStagingDescription,
    ) -> PyResult<HelperCpuReadbackExport> {
        let staging_share_id = described.staging_share_id.as_str();
        let checked_out = self.check_out_the_published_export_staging(
            described,
            HelperExportStagingResidency::HostVisible,
        )?;

        let vulkan_device = self.consumer_vulkan_device()?;
        let stated_memory_type_index = checked_out.stated_memory_type_index;
        // The staging fd is handed over here and never closed on the error
        // path: ownership passes to the driver inside the call, and the
        // failure arms that keep it are indistinguishable from the ones
        // that do not (see `ConsumerVulkanBuffer::from_opaque_fd`).
        let consumer_buffer = ConsumerVulkanBuffer::from_opaque_fd_at_stated_memory_type_index(
            &vulkan_device,
            checked_out.staging_fd.into_raw_fd(),
            described.staging_byte_size,
            stated_memory_type_index,
        )
        .map_err(|import_failure| {
            crate::python_processor_context::gpu_operation_error(format!(
                "this helper could not map the readback staging {staging_share_id:?} at memory \
                 type index {stated_memory_type_index}: {import_failure}"
            ))
        })?;
        if consumer_buffer.mapped_ptr().is_null() {
            return Err(crate::python_processor_context::gpu_operation_error(
                format!(
                    "the readback staging {staging_share_id:?} imported without a host mapping; \
                     the CPU doors read the mapping, so there is nothing to hand out"
                ),
            ));
        }

        Ok(HelperCpuReadbackExport {
            consumer_buffer,
            copy_done: checked_out.copy_done,
            width: described.width,
            height: described.height,
            format: described.format,
            bytes_per_row: described.bytes_per_row,
            writable: described.writable,
        })
    }

    /// Ask the parent to allocate and publish this surface's export
    /// staging at `residency`, and read the contract it answers with.
    ///
    /// The wire half both export arms share: one op shape, one set of
    /// response fields, one write-back seed — so a field that changes
    /// changes for both, rather than for whichever arm someone remembered.
    fn ask_the_parent_to_publish_the_export_staging(
        &self,
        python: Python<'_>,
        residency: HelperExportStagingResidency,
        surface_id: &str,
        source_pool_slot_key: &str,
    ) -> PyResult<DeviceExportStagingDescription> {
        let op = PyDict::new(python);
        op.set_item("op", residency.escalate_open_op_name())?;
        op.set_item("surface_id", surface_id)?;
        let response =
            escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        let format_name: String = response_field(&response, "format")?.extract()?;
        let exporting_device_uuid: String =
            response_field(&response, "exporting_device_uuid")?.extract()?;
        let described = DeviceExportStagingDescription {
            staging_share_id: response_field(&response, "handle_id")?.extract()?,
            staging_byte_size: decimal_string_field(&response, "staging_byte_size")?,
            exporting_device_uuid: parse_device_uuid(&exporting_device_uuid)?,
            width: response_field(&response, "width")?.extract()?,
            height: response_field(&response, "height")?.extract()?,
            format: crate::python_processor_context::parse_pixel_format_name(&format_name)?,
            bytes_per_row: decimal_string_field(&response, "bytes_per_row")?,
            writable: response_field(&response, "writable")?.extract()?,
        };
        // Seeds the shared answer so a later `surface_can_take_write_back`
        // costs no round trip of its own — the same mint-time answer, from
        // whichever arm opened first.
        self.write_back_answers_by_pool_slot
            .lock()
            .insert(source_pool_slot_key.to_string(), described.writable);
        Ok(described)
    }

    /// The checkout both export arms share: take the published staging,
    /// refuse what the service declined, insist on the OPAQUE_FD flavour,
    /// and import the copy timeline — leaving each arm only the memory
    /// import its own consumer needs.
    fn check_out_the_published_export_staging(
        &self,
        described: &DeviceExportStagingDescription,
        residency: HelperExportStagingResidency,
    ) -> PyResult<CheckedOutExportStaging> {
        let staging_share_id = described.staging_share_id.as_str();
        let staging_kind = residency.named_in_refusals();
        // The staging's own claim is never released: it is memoised for this
        // child's lifetime and is escalate-allocated rather than pool-backed,
        // so the lease pins no producer's slot. A pool-backed staging would
        // owe a debt here.
        let (registration, received_fds) = self.check_out_surface(staging_share_id)?;
        refuse_check_out_the_service_declined(
            format_args!("the {staging_kind} staging {staging_share_id:?}"),
            &registration,
        )?;
        let handle_type = registration
            .get("handle_type")
            .and_then(|value| value.as_str())
            .unwrap_or("dma_buf");
        if handle_type != "opaque_fd" {
            return Err(crate::python_processor_context::gpu_operation_error(
                format!(
                    "the {staging_kind} staging {staging_share_id:?} is registered as \
                 {handle_type:?}; an export staging imports OPAQUE_FD, and importing one \
                 flavour through the other hands the driver a handle of the wrong type"
                ),
            ));
        }
        let stated_memory_type_index = memory_type_index_stated_by_a_staging_registration(
            staging_kind,
            staging_share_id,
            &registration,
        )?;
        // The staging's memory fd, then the copy timeline's — the order
        // the registration published them in.
        let [staging_fd, copy_done_fd] =
            <[OwnedFd; 2]>::try_from(received_fds).map_err(|delivered: Vec<OwnedFd>| {
                crate::python_processor_context::gpu_operation_error(format!(
                    "check_out of the {staging_kind} staging {staging_share_id:?} returned {} \
                     fds; it carries exactly the staging's memory and its copy timeline",
                    delivered.len(),
                ))
            })?;

        let vulkan_device = self.consumer_vulkan_device()?;
        // Both imports adopt their fd on success and leave it with the
        // caller on failure, so each is handed over only at its call.
        let copy_done = match ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd(
            &vulkan_device,
            copy_done_fd.as_raw_fd(),
        ) {
            Ok(imported_timeline) => {
                let _adopted_by_vulkan = copy_done_fd.into_raw_fd();
                imported_timeline
            }
            Err(import_failure) => {
                return Err(crate::python_processor_context::gpu_operation_error(
                    format!(
                        "this helper could not import the copy timeline of \
                     {staging_share_id:?}: {import_failure}"
                    ),
                ));
            }
        };
        Ok(CheckedOutExportStaging {
            stated_memory_type_index,
            staging_fd,
            copy_done,
        })
    }
}

/// The readback staging's wire spellings: the copy direction's token and the
/// exporter's memory-type index a registration must state.
#[cfg(test)]
mod export_staging_wire_tests;
