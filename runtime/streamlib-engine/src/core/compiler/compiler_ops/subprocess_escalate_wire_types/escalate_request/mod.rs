// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Requests a helper process sends to the host over the escalate bridge.
//!
//! Wire contract: the helper builds these documents as plain Python dicts, so
//! serde's encoding of these types is the whole agreement between the two
//! sides. Field names, variant spellings and optional-field omission are not
//! free to change.

use serde::{Deserialize, Serialize};

mod acquisition;
mod compute;
mod device_idle;
mod export_staging;
mod failure_the_node_reported_of_itself;
mod graphics;
mod handle_lifecycle;
mod helper_log_record;
mod node_not_ready_reports;
mod node_store;
mod port_stream_facts;
mod process_raising_the_node_reports;
mod ray_tracing;
mod shared_native_service_lease;
mod surface_color_conversion;
mod surface_copy;

pub(crate) use acquisition::*;
pub(crate) use compute::*;
pub(crate) use device_idle::*;
pub(crate) use export_staging::*;
pub(crate) use failure_the_node_reported_of_itself::*;
pub(crate) use graphics::*;
pub(crate) use handle_lifecycle::*;
pub(crate) use helper_log_record::*;
pub(crate) use node_not_ready_reports::*;
pub(crate) use node_store::*;
pub(crate) use port_stream_facts::*;
pub(crate) use process_raising_the_node_reports::*;
pub(crate) use ray_tracing::*;
pub(crate) use shared_native_service_lease::*;
pub(crate) use surface_color_conversion::*;
pub(crate) use surface_copy::*;

/// Polyglot subprocess escalate-on-behalf request (subprocess → host)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub(crate) enum EscalateRequest {
    #[serde(rename = "acquire_image")]
    AcquireImage(EscalateRequestAcquireImage),

    #[serde(rename = "acquire_shared_native_service_lease")]
    AcquireSharedNativeServiceLease(EscalateRequestAcquireSharedNativeServiceLease),

    #[serde(rename = "acquire_pixel_buffer")]
    AcquirePixelBuffer(EscalateRequestAcquirePixelBuffer),

    #[serde(rename = "acquire_storage_buffer")]
    AcquireStorageBuffer(EscalateRequestAcquireStorageBuffer),

    #[serde(rename = "acquire_texture")]
    AcquireTexture(EscalateRequestAcquireTexture),

    #[serde(rename = "copy_device_export_staging_back_to_surface")]
    CopyDeviceExportStagingBackToSurface(EscalateRequestCopyDeviceExportStagingBackToSurface),

    #[serde(rename = "convert_surface_into")]
    ConvertSurfaceInto(EscalateRequestConvertSurfaceInto),

    #[serde(rename = "copy_surface_to_surface")]
    CopySurfaceToSurface(EscalateRequestCopySurfaceToSurface),

    #[serde(rename = "fail_the_node")]
    FailTheNode(EscalateRequestFailTheNode),

    #[serde(rename = "log")]
    Log(EscalateRequestLog),

    #[serde(rename = "open_cpu_readback_staging")]
    OpenCpuReadbackStaging(EscalateRequestOpenCpuReadbackStaging),

    #[serde(rename = "open_device_export_staging")]
    OpenDeviceExportStaging(EscalateRequestOpenDeviceExportStaging),

    #[serde(rename = "read_shared_native_service_lease_state")]
    ReadSharedNativeServiceLeaseState(EscalateRequestReadSharedNativeServiceLeaseState),

    #[serde(rename = "read_inbound_link_stream_facts")]
    ReadInboundLinkStreamFacts(EscalateRequestReadInboundLinkStreamFacts),

    #[serde(rename = "read_node_store_key")]
    ReadNodeStoreKey(EscalateRequestReadNodeStoreKey),

    #[serde(rename = "refill_device_export_staging")]
    RefillDeviceExportStaging(EscalateRequestRefillDeviceExportStaging),

    #[serde(rename = "register_acceleration_structure_blas")]
    RegisterAccelerationStructureBlas(EscalateRequestRegisterAccelerationStructureBlas),

    #[serde(rename = "register_acceleration_structure_tlas")]
    RegisterAccelerationStructureTlas(EscalateRequestRegisterAccelerationStructureTlas),

    #[serde(rename = "register_compute_kernel")]
    RegisterComputeKernel(EscalateRequestRegisterComputeKernel),

    #[serde(rename = "register_graphics_kernel")]
    RegisterGraphicsKernel(EscalateRequestRegisterGraphicsKernel),

    #[serde(rename = "register_ray_tracing_kernel")]
    RegisterRayTracingKernel(EscalateRequestRegisterRayTracingKernel),

    #[serde(rename = "release_shared_native_service_lease")]
    ReleaseSharedNativeServiceLease(EscalateRequestReleaseSharedNativeServiceLease),

    #[serde(rename = "release_handle")]
    ReleaseHandle(EscalateRequestReleaseHandle),

    #[serde(rename = "remove_node_store_key")]
    RemoveNodeStoreKey(EscalateRequestRemoveNodeStoreKey),

    #[serde(rename = "run_compute_kernel")]
    RunComputeKernel(EscalateRequestRunComputeKernel),

    #[serde(rename = "run_compute_kernel_batch")]
    RunComputeKernelBatch(EscalateRequestRunComputeKernelBatch),

    #[serde(rename = "run_cpu_readback_copy")]
    RunCpuReadbackCopy(EscalateRequestRunCpuReadbackCopy),

    #[serde(rename = "run_graphics_draw")]
    RunGraphicsDraw(EscalateRequestRunGraphicsDraw),

    #[serde(rename = "run_ray_tracing_kernel")]
    RunRayTracingKernel(EscalateRequestRunRayTracingKernel),

    #[serde(rename = "set_process_raising_because")]
    SetProcessRaisingBecause(EscalateRequestSetProcessRaisingBecause),

    #[serde(rename = "set_not_ready_because")]
    SetNotReadyBecause(EscalateRequestSetNotReadyBecause),

    #[serde(rename = "set_output_port_stream_facts")]
    SetOutputPortStreamFacts(EscalateRequestSetOutputPortStreamFacts),

    #[serde(rename = "wait_device_idle")]
    WaitDeviceIdle(EscalateRequestWaitDeviceIdle),

    #[serde(rename = "write_node_store_key")]
    WriteNodeStoreKey(EscalateRequestWriteNodeStoreKey),
}
