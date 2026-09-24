// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Polyglot escalate-on-behalf IPC for Python subprocess host
//! processors. The subprocess can only see a `GpuContextLimitedAccess`
//! sandbox; when it needs the privileged `GpuContextFullAccess` surface it
//! sends an [`EscalateRequest`] to the host over its stdout, the host
//! executes the operation inside [`GpuContextLimitedAccess::escalate`], and
//! replies with an [`EscalateResponse`] on the subprocess's stdin.
//!
//! Wire format is the existing length-prefixed JSON stdio bridge used for
//! lifecycle commands (see `SubprocessBridge`). Requests and responses are
//! discriminated by `op` and `result` fields respectively. The shape is
//! owned by the types in [`super::subprocess_escalate_wire_types`] — their
//! serde encoding is the agreement with the helper, which builds the same
//! documents as plain Python dicts.

mod acquisition;
mod compute;
mod export_staging;
mod graphics;
pub(super) mod handle_lifecycle;
mod helper_log_record;
mod hex_encoded_wire_bytes;
mod inbound_link_stamp_clock_identity;
#[cfg(target_os = "linux")]
mod kernel_shader_stage_source;
mod processor_owned_window;
mod ray_tracing;
#[cfg(target_os = "linux")]
mod surface_bound_kernel_binding;
#[cfg(test)]
mod tests;

use self::handle_lifecycle::EscalateHandleRegistry;
use super::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestCloseProcessorOwnedWindow, EscalateRequestCopyDeviceExportStagingBackToSurface,
    EscalateRequestDrainProcessorOwnedWindowEvents, EscalateRequestInboundLinkStampClockIdentity,
    EscalateRequestOpenCpuReadbackStaging, EscalateRequestOpenDeviceExportStaging,
    EscalateRequestRefillDeviceExportStaging, EscalateRequestRunCpuReadbackCopy,
    EscalateRequestWaitDeviceIdle,
};
use super::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use super::subprocess_escalate_wire_types::{EscalateRequest, EscalateResponse};
use crate::core::context::GpuContextLimitedAccess;
#[cfg(test)]
use crate::core::error::{Error, Result};
use crate::core::logging::push_polyglot_record;
use crate::core::runtime::mesh::MeshLinkIngressTable;

/// Wire tag marking a message as an escalate request. Bridges demux on this
/// before falling through to lifecycle dispatch.
pub(crate) const ESCALATE_REQUEST_RPC: &str = "escalate_request";

/// Wire tag for responses written back to the subprocess.
pub(crate) const ESCALATE_RESPONSE_RPC: &str = "escalate_response";

/// The `op` tag of the one escalate request answered by nothing,
/// [`EscalateRequest::Log`] — the bridge dispatches it on its reader rather
/// than queueing it behind GPU work.
pub(crate) const ESCALATE_OP_ANSWERED_BY_NOTHING: &str = "log";

/// Extract `request_id` from a request/response-shaped op. Returns `None`
/// for fire-and-forget ops ([`EscalateRequest::Log`]), which carry no
/// correlation token because the host never writes a reply.
fn request_id(op: &EscalateRequest) -> Option<&str> {
    match op {
        EscalateRequest::AcquirePixelBuffer(p) => Some(&p.request_id),
        EscalateRequest::AcquireTexture(p) => Some(&p.request_id),
        EscalateRequest::AcquireImage(p) => Some(&p.request_id),
        EscalateRequest::RunCpuReadbackCopy(p) => Some(&p.request_id),
        EscalateRequest::WaitDeviceIdle(p) => Some(&p.request_id),
        EscalateRequest::InboundLinkStampClockIdentity(p) => Some(&p.request_id),
        EscalateRequest::OpenCpuReadbackStaging(p) => Some(&p.request_id),
        EscalateRequest::OpenDeviceExportStaging(p) => Some(&p.request_id),
        EscalateRequest::RefillDeviceExportStaging(p) => Some(&p.request_id),
        EscalateRequest::CopyDeviceExportStagingBackToSurface(p) => Some(&p.request_id),
        EscalateRequest::RegisterComputeKernel(p) => Some(&p.request_id),
        EscalateRequest::RunComputeKernel(p) => Some(&p.request_id),
        EscalateRequest::RunComputeKernelBatch(p) => Some(&p.request_id),
        EscalateRequest::RegisterGraphicsKernel(p) => Some(&p.request_id),
        EscalateRequest::RunGraphicsDraw(p) => Some(&p.request_id),
        EscalateRequest::RegisterAccelerationStructureBlas(p) => Some(&p.request_id),
        EscalateRequest::RegisterAccelerationStructureTlas(p) => Some(&p.request_id),
        EscalateRequest::RegisterRayTracingKernel(p) => Some(&p.request_id),
        EscalateRequest::RunRayTracingKernel(p) => Some(&p.request_id),
        EscalateRequest::ReleaseHandle(p) => Some(&p.request_id),
        EscalateRequest::CreateProcessorOwnedWindow(p) => Some(&p.request_id),
        EscalateRequest::ShowSurfaceOnProcessorOwnedWindow(p) => Some(&p.request_id),
        EscalateRequest::DrainProcessorOwnedWindowEvents(p) => Some(&p.request_id),
        EscalateRequest::CloseProcessorOwnedWindow(p) => Some(&p.request_id),
        EscalateRequest::Log(_) => None,
    }
}

/// Dispatch an [`EscalateRequest`] against `sandbox`. Returns
/// `Some(EscalateResponse)` for request/response ops so the bridge can
/// write a reply; returns `None` for fire-and-forget ops
/// ([`EscalateRequest::Log`]) whose effect lands directly in the unified
/// logging pathway and needs no correlated reply.
///
/// Never panics — errors inside `escalate()` become [`EscalateResponse::Err`]
/// with the original request_id preserved so the subprocess can correlate.
///
/// On Linux, acquisition handlers additionally check the freshly-allocated
/// resource in with the surface-share service's [`SurfaceStore`] so the polyglot subprocess
/// can `check_out` the DMA-BUF FD by the same handle_id. The `handle_id`
/// returned to the subprocess is the surface-share service-assigned `surface_id`.
pub(crate) fn handle_escalate_op(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    mesh_link_ingress_table: &MeshLinkIngressTable,
    op: EscalateRequest,
) -> Option<EscalateResponse> {
    let rid = request_id(&op).map(str::to_string).unwrap_or_default();
    match op {
        EscalateRequest::AcquirePixelBuffer(req) => Some(acquisition::handle_acquire_pixel_buffer(
            sandbox, registry, rid, req,
        )),
        EscalateRequest::AcquireTexture(req) => Some(acquisition::handle_acquire_texture(
            sandbox, registry, rid, req,
        )),
        EscalateRequest::AcquireImage(req) => Some(acquisition::handle_acquire_image(
            sandbox, registry, rid, req,
        )),
        EscalateRequest::RunCpuReadbackCopy(EscalateRequestRunCpuReadbackCopy {
            request_id: _,
            surface_id,
            direction,
        }) => Some(export_staging::handle_run_cpu_readback_copy(
            sandbox,
            rid,
            &surface_id,
            direction,
        )),
        EscalateRequest::InboundLinkStampClockIdentity(
            EscalateRequestInboundLinkStampClockIdentity {
                request_id: _,
                inbound_link_name,
            },
        ) => Some(
            inbound_link_stamp_clock_identity::handle_inbound_link_stamp_clock_identity(
                mesh_link_ingress_table,
                rid,
                &inbound_link_name,
            ),
        ),
        EscalateRequest::WaitDeviceIdle(EscalateRequestWaitDeviceIdle { request_id: _ }) => {
            Some(match sandbox.escalate(|full| full.wait_device_idle()) {
                Ok(()) => EscalateResponse::Ok(EscalateResponseOk {
                    request_id: rid,
                    handle_id: String::new(),
                    ..Default::default()
                }),
                Err(failure) => EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: format!("wait_device_idle failed: {failure}"),
                }),
            })
        }
        EscalateRequest::OpenCpuReadbackStaging(EscalateRequestOpenCpuReadbackStaging {
            request_id: _,
            surface_id,
        }) => Some(export_staging::handle_open_cpu_readback_staging(
            sandbox,
            rid,
            &surface_id,
        )),
        EscalateRequest::OpenDeviceExportStaging(EscalateRequestOpenDeviceExportStaging {
            request_id: _,
            surface_id,
        }) => Some(export_staging::handle_open_device_export_staging(
            sandbox,
            rid,
            &surface_id,
        )),
        EscalateRequest::RefillDeviceExportStaging(EscalateRequestRefillDeviceExportStaging {
            request_id: _,
            surface_id,
        }) => Some(export_staging::handle_refill_device_export_staging(
            sandbox,
            rid,
            &surface_id,
        )),
        EscalateRequest::CopyDeviceExportStagingBackToSurface(
            EscalateRequestCopyDeviceExportStagingBackToSurface {
                request_id: _,
                surface_id,
            },
        ) => Some(
            export_staging::handle_copy_device_export_staging_back_to_surface(
                sandbox,
                rid,
                &surface_id,
            ),
        ),
        EscalateRequest::RegisterComputeKernel(req) => {
            Some(compute::handle_register_compute_kernel(sandbox, rid, req))
        }
        EscalateRequest::RunComputeKernel(req) => {
            Some(compute::handle_run_compute_kernel(sandbox, rid, req))
        }
        EscalateRequest::RunComputeKernelBatch(req) => {
            Some(compute::handle_run_compute_kernel_batch(sandbox, rid, req))
        }
        EscalateRequest::RegisterGraphicsKernel(req) => {
            Some(graphics::handle_register_graphics_kernel(sandbox, rid, req))
        }
        EscalateRequest::RunGraphicsDraw(req) => {
            Some(graphics::handle_run_graphics_draw(sandbox, rid, req))
        }
        EscalateRequest::RegisterAccelerationStructureBlas(req) => Some(
            ray_tracing::handle_register_acceleration_structure_blas(sandbox, rid, req),
        ),
        EscalateRequest::RegisterAccelerationStructureTlas(req) => Some(
            ray_tracing::handle_register_acceleration_structure_tlas(sandbox, rid, req),
        ),
        EscalateRequest::RegisterRayTracingKernel(req) => Some(
            ray_tracing::handle_register_ray_tracing_kernel(sandbox, rid, req),
        ),
        EscalateRequest::RunRayTracingKernel(req) => Some(
            ray_tracing::handle_run_ray_tracing_kernel(sandbox, rid, req),
        ),
        EscalateRequest::ReleaseHandle(req) => Some(handle_lifecycle::handle_release_handle(
            sandbox, registry, rid, req,
        )),
        EscalateRequest::CreateProcessorOwnedWindow(req) => Some(
            processor_owned_window::handle_create_processor_owned_window(
                sandbox, registry, rid, req,
            ),
        ),
        EscalateRequest::ShowSurfaceOnProcessorOwnedWindow(req) => Some(
            processor_owned_window::handle_show_surface_on_processor_owned_window(
                sandbox, registry, rid, req,
            ),
        ),
        EscalateRequest::DrainProcessorOwnedWindowEvents(
            EscalateRequestDrainProcessorOwnedWindowEvents {
                request_id: _,
                window_id,
            },
        ) => Some(
            processor_owned_window::handle_drain_processor_owned_window_events(
                registry, rid, window_id,
            ),
        ),
        EscalateRequest::CloseProcessorOwnedWindow(EscalateRequestCloseProcessorOwnedWindow {
            request_id: _,
            window_id,
        }) => Some(processor_owned_window::handle_close_processor_owned_window(
            registry, rid, window_id,
        )),
        EscalateRequest::Log(log_op) => {
            push_polyglot_record(helper_log_record::log_record_from_wire(log_op));
            None
        }
    }
}

/// The refusal an op only Linux implements answers with everywhere else.
#[cfg(not(target_os = "linux"))]
fn escalate_op_only_available_on_linux(request_id: String, op_name: &str) -> EscalateResponse {
    EscalateResponse::Err(EscalateResponseErr {
        request_id,
        message: format!("{op_name} is only available on Linux"),
    })
}

/// The response refusing one escalate request by its frame, carrying `message`
/// — what a bridge answers a request it will not dispatch.
pub(crate) fn refusal_of_an_escalate_request(
    request_frame: &serde_json::Value,
    message: String,
) -> serde_json::Value {
    envelope_response(EscalateResponse::Err(EscalateResponseErr {
        request_id: request_frame
            .get("request_id")
            .and_then(|request_id| request_id.as_str())
            .unwrap_or_default()
            .to_string(),
        message,
    }))
}

/// Wrap an [`EscalateResponse`] in the outer `{ rpc, payload… }` envelope the
/// bridge's escalate worker writes to the subprocess.
pub(crate) fn envelope_response(result: EscalateResponse) -> serde_json::Value {
    let mut obj = serde_json::to_value(&result).unwrap_or(serde_json::Value::Null);
    if let Some(map) = obj.as_object_mut() {
        map.insert(
            "rpc".to_string(),
            serde_json::Value::String(ESCALATE_RESPONSE_RPC.to_string()),
        );
    }
    obj
}

/// Try to parse an incoming bridge message as an [`EscalateRequest`].
/// Returns `None` when the message isn't an escalate request (lifecycle
/// traffic). Returns `Some(Err(...))` when the message was tagged as an
/// escalate request but the payload couldn't be decoded — the bridge still
/// replies with an `Err` response keyed by `request_id` if possible.
pub(crate) fn try_parse_escalate_request(
    value: &serde_json::Value,
) -> Option<std::result::Result<EscalateRequest, EscalateParseError>> {
    let rpc = value.get("rpc").and_then(|v| v.as_str())?;
    if rpc != ESCALATE_REQUEST_RPC {
        return None;
    }
    let request_id = value
        .get("request_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    // The `rpc` field is the bridge-layer envelope tag, not part of the
    // typed escalate schema. Strip it before deserializing so the generated
    // variant structs (which carry `#[serde(deny_unknown_fields)]`) don't
    // reject it.
    let mut inner = value.clone();
    if let Some(obj) = inner.as_object_mut() {
        obj.remove("rpc");
    }
    match serde_json::from_value::<EscalateRequest>(inner) {
        Ok(op) => Some(Ok(op)),
        Err(e) => Some(Err(EscalateParseError {
            request_id,
            message: format!("failed to decode escalate_request: {e}"),
        })),
    }
}

/// Error detail for a malformed escalate request. The bridge converts this
/// into an [`EscalateResponse::Err`] response so the subprocess doesn't
/// block forever waiting on a correlated response.
pub(crate) struct EscalateParseError {
    pub(crate) request_id: Option<String>,
    pub(crate) message: String,
}

impl EscalateParseError {
    pub(crate) fn into_response(self) -> EscalateResponse {
        EscalateResponse::Err(EscalateResponseErr {
            request_id: self.request_id.unwrap_or_default(),
            message: self.message,
        })
    }
}

/// Convenience wrapper used by host processors: parse, dispatch, envelope.
/// Anything the subprocess sends that carries `rpc: escalate_request` flows
/// through this single function; lifecycle traffic is handled by the caller.
pub(crate) fn process_bridge_message(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    mesh_link_ingress_table: &MeshLinkIngressTable,
    value: &serde_json::Value,
) -> Option<serde_json::Value> {
    let parsed = try_parse_escalate_request(value)?;
    let response = match parsed {
        // Fire-and-forget ops (log) return `None` from the handler — no
        // reply is written back to the subprocess.
        Ok(op) => handle_escalate_op(sandbox, registry, mesh_link_ingress_table, op)?,
        Err(err) => err.into_response(),
    };
    Some(envelope_response(response))
}

/// Public view of a failure to unwrap a response envelope. Hoisted so tests
/// can assert on the error text without stringly comparisons against
/// serde_json diagnostics.
#[cfg(test)]
pub(crate) fn parse_op_for_tests(value: &serde_json::Value) -> Result<EscalateRequest> {
    try_parse_escalate_request(value)
        .ok_or_else(|| Error::Runtime("not an escalate_request".to_string()))?
        .map_err(|e| Error::Runtime(e.message))
}
