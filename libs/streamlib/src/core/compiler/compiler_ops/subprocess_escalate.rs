// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Polyglot escalate-on-behalf IPC for Python and Deno subprocess host
//! processors. The subprocess can only see a `GpuContextLimitedAccess`
//! sandbox; when it needs the privileged `GpuContextFullAccess` surface it
//! sends an [`EscalateRequest`] to the host over its stdout, the host
//! executes the operation inside [`GpuContextLimitedAccess::escalate`], and
//! replies with an [`EscalateResponse`] on the subprocess's stdin.
//!
//! Wire format is the existing length-prefixed JSON stdio bridge used for
//! lifecycle commands (see `SubprocessBridge`). Requests and responses are
//! discriminated by `op` and `result` fields respectively; the shape is
//! owned by `schemas/com.streamlib.escalate_{request,response}@1.0.0.yaml`
//! and the generated Rust types live in `_generated_`.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use uuid::Uuid;

use crate::_generated_::com_streamlib_escalate_request::{
    EscalateRequestAcquireImage, EscalateRequestAcquirePixelBuffer, EscalateRequestAcquireTexture,
    EscalateRequestLog, EscalateRequestLogLevel, EscalateRequestLogSource,
    EscalateRequestRegisterAccelerationStructureBlas,
    EscalateRequestRegisterAccelerationStructureTlas,
    EscalateRequestRegisterComputeKernel, EscalateRequestRegisterGraphicsKernel,
    EscalateRequestRegisterGraphicsKernelBindingKind,
    EscalateRequestRegisterGraphicsKernelPipelineState,
    EscalateRequestRegisterGraphicsKernelPipelineStateAttachmentDepthFormat,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateDepthCompareOp,
    EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState,
    EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode,
    EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace,
    EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode,
    EscalateRequestRegisterGraphicsKernelPipelineStateTopology,
    EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttributeFormat,
    EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBindingInputRate,
    EscalateRequestRegisterRayTracingKernel, EscalateRequestRegisterRayTracingKernelBindingKind,
    EscalateRequestRegisterRayTracingKernelGroupKind,
    EscalateRequestRegisterRayTracingKernelStageStage, EscalateRequestReleaseHandle,
    EscalateRequestRunComputeKernel, EscalateRequestRunCpuReadbackCopy,
    EscalateRequestRunCpuReadbackCopyDirection, EscalateRequestRunGraphicsDraw,
    EscalateRequestRunGraphicsDrawBindingKind, EscalateRequestRunGraphicsDrawDrawKind,
    EscalateRequestRunGraphicsDrawIndexBufferIndexType, EscalateRequestRunRayTracingKernel,
    EscalateRequestRunRayTracingKernelBindingKind, EscalateRequestTryRunCpuReadbackCopy,
    EscalateRequestTryRunCpuReadbackCopyDirection,
};
use crate::_generated_::com_streamlib_escalate_response::{
    EscalateResponseContended, EscalateResponseErr, EscalateResponseOk,
};
use crate::_generated_::{EscalateRequest, EscalateResponse};
use crate::core::context::{PooledTextureHandle, TexturePoolDescriptor};
use crate::core::context::GpuContextLimitedAccess;
#[cfg(target_os = "linux")]
use crate::core::context::{
    BlasRegisterDecl, BlendFactorWire, BlendOpWire, ComputeKernelBridge, CpuReadbackBridge,
    CpuReadbackCopyDirection, CullModeWire, DepthCompareOpWire, DepthFormatWire,
    DynamicStateWire, FrontFaceWire, GraphicsBindingDecl, GraphicsBindingKindWire,
    GraphicsBindingValue, GraphicsDrawSpec, GraphicsIndexBufferBinding, GraphicsKernelBridge,
    GraphicsKernelRegisterDecl, GraphicsKernelRunDraw, GraphicsPipelineStateWire,
    GraphicsVertexBufferBinding, IndexTypeWire, PolygonModeWire, PrimitiveTopologyWire,
    RayTracingBindingDecl, RayTracingBindingKindWire, RayTracingBindingValue,
    RayTracingKernelBridge, RayTracingKernelRegisterDecl, RayTracingKernelRunDispatch,
    RayTracingShaderGroupWire, RayTracingShaderStageWire, RayTracingStageDecl,
    ScissorRectWire, TlasInstanceDeclWire, TlasRegisterDecl, VertexAttributeFormatWire,
    VertexInputAttributeDecl, VertexInputBindingDecl, VertexInputRateWire, ViewportWire,
    RAY_TRACING_STAGE_INDEX_NONE,
};
use crate::core::logging::{push_polyglot_record, LogLevel, LogRecord, Source};
use crate::core::rhi::{PixelFormat, RhiPixelBuffer, TextureFormat, TextureUsages};

#[cfg(test)]
use crate::core::error::{Result, StreamError};

/// Wire tag marking a message as an escalate request. Bridges demux on this
/// before falling through to lifecycle dispatch.
pub(crate) const ESCALATE_REQUEST_RPC: &str = "escalate_request";

/// Wire tag for responses written back to the subprocess.
pub(crate) const ESCALATE_RESPONSE_RPC: &str = "escalate_response";

/// Extract `request_id` from a request/response-shaped op. Returns `None`
/// for fire-and-forget ops ([`EscalateRequest::Log`]), which carry no
/// correlation token because the host never writes a reply.
fn request_id(op: &EscalateRequest) -> Option<&str> {
    match op {
        EscalateRequest::AcquirePixelBuffer(p) => Some(&p.request_id),
        EscalateRequest::AcquireTexture(p) => Some(&p.request_id),
        EscalateRequest::AcquireImage(p) => Some(&p.request_id),
        EscalateRequest::RunCpuReadbackCopy(p) => Some(&p.request_id),
        EscalateRequest::TryRunCpuReadbackCopy(p) => Some(&p.request_id),
        EscalateRequest::RegisterComputeKernel(p) => Some(&p.request_id),
        EscalateRequest::RunComputeKernel(p) => Some(&p.request_id),
        EscalateRequest::RegisterGraphicsKernel(p) => Some(&p.request_id),
        EscalateRequest::RunGraphicsDraw(p) => Some(&p.request_id),
        EscalateRequest::RegisterAccelerationStructureBlas(p) => Some(&p.request_id),
        EscalateRequest::RegisterAccelerationStructureTlas(p) => Some(&p.request_id),
        EscalateRequest::RegisterRayTracingKernel(p) => Some(&p.request_id),
        EscalateRequest::RunRayTracingKernel(p) => Some(&p.request_id),
        EscalateRequest::ReleaseHandle(p) => Some(&p.request_id),
        EscalateRequest::Log(_) => None,
    }
}

/// Resource kept alive on behalf of a subprocess by
/// [`EscalateHandleRegistry`]. The fields are only read via the `Drop`
/// side-effect that releases them back to the host pool when removed
/// from the registry — the map keeps the resource live, the resource's
/// destructor does the release on removal.
///
/// Post-#562: cpu-readback no longer registers per-acquire handles.
/// Staging buffers + timeline are pre-registered with surface-share
/// at startup and the subprocess imports them once via
/// `streamlib-consumer-rhi`; per-acquire IPC reduces to a thin
/// `run_cpu_readback_copy` trigger that returns a timeline value.
pub(crate) enum RegisteredHandle {
    #[allow(dead_code)]
    PixelBuffer(RhiPixelBuffer),
    #[allow(dead_code)]
    Texture(PooledTextureHandle),
}

/// Tracks resources acquired on behalf of a subprocess so `release_handle` —
/// or subprocess death — can drop the host's strong reference. Resources stay
/// alive for the duration of the host pool; this map simply prevents the
/// resource from being immediately recycled while the subprocess still
/// references it by ID. Dropping a [`PooledTextureHandle`] releases the pool
/// slot; dropping an [`RhiPixelBuffer`] releases its refcount.
#[derive(Default)]
pub(crate) struct EscalateHandleRegistry {
    handles: Mutex<HashMap<String, RegisteredHandle>>,
}

impl EscalateHandleRegistry {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn insert_buffer(&self, handle_id: String, buffer: RhiPixelBuffer) {
        let mut map = self.handles.lock().expect("poisoned");
        map.insert(handle_id, RegisteredHandle::PixelBuffer(buffer));
    }

    pub(crate) fn insert_texture(&self, handle_id: String, texture: PooledTextureHandle) {
        let mut map = self.handles.lock().expect("poisoned");
        map.insert(handle_id, RegisteredHandle::Texture(texture));
    }

    /// Remove a handle by id. Returns `true` when an entry was found
    /// and removed; `false` when the id was unknown. Used by the
    /// escalate `release_handle` path.
    pub(crate) fn remove_handle(&self, handle_id: &str) -> bool {
        let mut map = self.handles.lock().expect("poisoned");
        map.remove(handle_id).is_some()
    }

    pub(crate) fn clear(&self) {
        let mut map = self.handles.lock().expect("poisoned");
        map.clear();
    }

    /// Number of currently-held handles; visible for tests.
    #[cfg(test)]
    pub(crate) fn handle_count(&self) -> usize {
        self.handles.lock().expect("poisoned").len()
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
    op: EscalateRequest,
) -> Option<EscalateResponse> {
    let rid = request_id(&op).map(str::to_string).unwrap_or_default();
    match op {
        EscalateRequest::AcquirePixelBuffer(EscalateRequestAcquirePixelBuffer {
            request_id: _,
            width,
            height,
            format,
        }) => Some(match parse_pixel_format(&format) {
            Ok(parsed) => {
                let acquired = sandbox.escalate(|full| {
                    let (pool_id, buffer) = full.acquire_pixel_buffer(width, height, parsed)?;
                    let handle_id = assign_buffer_handle_id(full, &pool_id, &buffer)?;
                    Ok((handle_id, buffer))
                });
                match acquired {
                    Ok((handle_id, buffer)) => {
                        registry.insert_buffer(handle_id.clone(), buffer);
                        EscalateResponse::Ok(EscalateResponseOk {
                            request_id: rid,
                            handle_id,
                            width: Some(width),
                            height: Some(height),
                            format: Some(pixel_format_to_wire(parsed).to_string()),
                            usage: None,
                            timeline_value: None,
                        })
                    }
                    Err(e) => EscalateResponse::Err(EscalateResponseErr {
                        request_id: rid,
                        message: format!("acquire_pixel_buffer failed: {e}"),
                    }),
                }
            }
            Err(e) => EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: e,
            }),
        }),
        EscalateRequest::AcquireTexture(EscalateRequestAcquireTexture {
            request_id: _,
            width,
            height,
            format,
            usage,
        }) => {
            let parsed_format = match parse_texture_format(&format) {
                Ok(f) => f,
                Err(e) => {
                    return Some(EscalateResponse::Err(EscalateResponseErr {
                        request_id: rid,
                        message: e,
                    }));
                }
            };
            let parsed_usage = match parse_texture_usages(&usage) {
                Ok(u) => u,
                Err(e) => {
                    return Some(EscalateResponse::Err(EscalateResponseErr {
                        request_id: rid,
                        message: e,
                    }));
                }
            };
            let desc = TexturePoolDescriptor::new(width, height, parsed_format)
                .with_usage(parsed_usage);
            let acquired = sandbox.escalate(|full| {
                let texture = full.acquire_texture(&desc)?;
                let handle_id = assign_texture_handle_id(full, &texture)?;
                Ok((handle_id, texture))
            });
            Some(match acquired {
                Ok((handle_id, texture)) => {
                    registry.insert_texture(handle_id.clone(), texture);
                    EscalateResponse::Ok(EscalateResponseOk {
                        request_id: rid,
                        handle_id,
                        width: Some(width),
                        height: Some(height),
                        format: Some(texture_format_to_wire(parsed_format).to_string()),
                        usage: Some(texture_usages_to_wire(parsed_usage)),
                        timeline_value: None,
                    })
                }
                Err(e) => EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: format!("acquire_texture failed: {e}"),
                }),
            })
        }
        EscalateRequest::AcquireImage(EscalateRequestAcquireImage {
            request_id: _,
            width,
            height,
            format,
        }) => {
            #[cfg(target_os = "linux")]
            {
                let parsed_format = match parse_texture_format(&format) {
                    Ok(f) => f,
                    Err(e) => {
                        return Some(EscalateResponse::Err(EscalateResponseErr {
                            request_id: rid,
                            message: e,
                        }));
                    }
                };
                // Render-target images carry their own usage signature —
                // they can be sampled, copied, AND used as render attachments.
                // The wire op deliberately does not take a usage list (that's
                // an acquire_texture concern); here the host knows the exact
                // set because the consumer is always a render-target adapter.
                let acquired = sandbox.escalate(|full| {
                    let texture = full.acquire_render_target_dma_buf_image(
                        width,
                        height,
                        parsed_format,
                    )?;
                    let handle_id = assign_image_handle_id(full, &texture)?;
                    Ok((handle_id, texture))
                });
                Some(match acquired {
                    Ok((handle_id, _texture)) => EscalateResponse::Ok(EscalateResponseOk {
                        request_id: rid,
                        handle_id,
                        width: Some(width),
                        height: Some(height),
                        format: Some(texture_format_to_wire(parsed_format).to_string()),
                        usage: Some(vec![
                            "render_attachment".to_string(),
                            "texture_binding".to_string(),
                            "copy_src".to_string(),
                        ]),
                        timeline_value: None,
                    }),
                    Err(e) => EscalateResponse::Err(EscalateResponseErr {
                        request_id: rid,
                        message: format!("acquire_image failed: {e}"),
                    }),
                })
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (width, height, format);
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "acquire_image is only available on Linux (DMA-BUF render-target path)".to_string(),
                }))
            }
        }
        EscalateRequest::RunCpuReadbackCopy(EscalateRequestRunCpuReadbackCopy {
            request_id: _,
            surface_id,
            direction,
        }) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_run_cpu_readback_copy(sandbox, rid, &surface_id, direction))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (surface_id, direction);
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "run_cpu_readback_copy is only available on Linux".to_string(),
                }))
            }
        }
        EscalateRequest::TryRunCpuReadbackCopy(EscalateRequestTryRunCpuReadbackCopy {
            request_id: _,
            surface_id,
            direction,
        }) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_try_run_cpu_readback_copy(
                    sandbox,
                    rid,
                    &surface_id,
                    direction,
                ))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (surface_id, direction);
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "try_run_cpu_readback_copy is only available on Linux".to_string(),
                }))
            }
        }
        EscalateRequest::RegisterComputeKernel(EscalateRequestRegisterComputeKernel {
            request_id: _,
            spv_hex,
            push_constant_size,
        }) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_register_compute_kernel(
                    sandbox,
                    rid,
                    &spv_hex,
                    push_constant_size,
                ))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (spv_hex, push_constant_size);
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "register_compute_kernel is only available on Linux".to_string(),
                }))
            }
        }
        EscalateRequest::RunComputeKernel(EscalateRequestRunComputeKernel {
            request_id: _,
            kernel_id,
            surface_uuid,
            push_constants_hex,
            group_count_x,
            group_count_y,
            group_count_z,
        }) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_run_compute_kernel(
                    sandbox,
                    rid,
                    &kernel_id,
                    &surface_uuid,
                    &push_constants_hex,
                    group_count_x,
                    group_count_y,
                    group_count_z,
                ))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (
                    kernel_id,
                    surface_uuid,
                    push_constants_hex,
                    group_count_x,
                    group_count_y,
                    group_count_z,
                );
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "run_compute_kernel is only available on Linux".to_string(),
                }))
            }
        }
        EscalateRequest::RegisterGraphicsKernel(req) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_register_graphics_kernel(sandbox, rid, req))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = req;
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "register_graphics_kernel is only available on Linux".to_string(),
                }))
            }
        }
        EscalateRequest::RunGraphicsDraw(req) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_run_graphics_draw(sandbox, rid, req))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = req;
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "run_graphics_draw is only available on Linux".to_string(),
                }))
            }
        }
        EscalateRequest::RegisterAccelerationStructureBlas(req) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_register_acceleration_structure_blas(sandbox, rid, req))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = req;
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message:
                        "register_acceleration_structure_blas is only available on Linux"
                            .to_string(),
                }))
            }
        }
        EscalateRequest::RegisterAccelerationStructureTlas(req) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_register_acceleration_structure_tlas(sandbox, rid, req))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = req;
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message:
                        "register_acceleration_structure_tlas is only available on Linux"
                            .to_string(),
                }))
            }
        }
        EscalateRequest::RegisterRayTracingKernel(req) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_register_ray_tracing_kernel(sandbox, rid, req))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = req;
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "register_ray_tracing_kernel is only available on Linux"
                        .to_string(),
                }))
            }
        }
        EscalateRequest::RunRayTracingKernel(req) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_run_ray_tracing_kernel(sandbox, rid, req))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = req;
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "run_ray_tracing_kernel is only available on Linux".to_string(),
                }))
            }
        }
        EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
            request_id: _,
            handle_id,
        }) => {
            let removed = registry.remove_handle(&handle_id);
            Some(if removed {
                // Pixel-buffer / texture / image acquires were
                // checked into the surface-share service under the
                // returned handle_id; pair the registry eviction
                // with the matching service release.
                release_surface_share_surface(sandbox, &handle_id);
                EscalateResponse::Ok(EscalateResponseOk {
                    request_id: rid,
                    handle_id,
                    width: None,
                    height: None,
                    format: None,
                    usage: None,
                    timeline_value: None,
                })
            } else {
                EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: format!("handle_id '{handle_id}' not found in registry"),
                })
            })
        }
        EscalateRequest::Log(log_op) => {
            push_polyglot_record(log_record_from_wire(log_op));
            None
        }
    }
}

/// Convert a wire-format [`EscalateRequestLog`] into a host-side
/// [`LogRecord`]. Stamps `host_ts` at the moment of receipt — the
/// subprocess-supplied `source_ts` is advisory only and never used for
/// ordering. Parses `source_seq` from its string wire encoding (JTD has
/// no native u64); silently drops the value on parse failure so a
/// malformed subprocess can't block log delivery.
fn log_record_from_wire(log: EscalateRequestLog) -> LogRecord {
    let source = match log.source {
        EscalateRequestLogSource::Python => Source::Python,
        EscalateRequestLogSource::Deno => Source::Deno,
    };
    let level = match log.level {
        EscalateRequestLogLevel::Trace => LogLevel::Trace,
        EscalateRequestLogLevel::Debug => LogLevel::Debug,
        EscalateRequestLogLevel::Info => LogLevel::Info,
        EscalateRequestLogLevel::Warn => LogLevel::Warn,
        EscalateRequestLogLevel::Error => LogLevel::Error,
    };
    let target = match source {
        Source::Python => "streamlib::polyglot::python",
        Source::Deno => "streamlib::polyglot::deno",
        Source::Rust => "streamlib::polyglot",
    };
    let source_seq = log.source_seq.parse::<u64>().ok();
    let attrs: BTreeMap<String, serde_json::Value> = log
        .attrs
        .into_iter()
        .map(|(k, v)| (k, v.unwrap_or(serde_json::Value::Null)))
        .collect();

    LogRecord {
        host_ts: now_ns(),
        level,
        target: target.to_string(),
        message: log.message,
        pipeline_id: log.pipeline_id,
        processor_id: log.processor_id,
        rhi_op: None,
        intercepted: log.intercepted,
        channel: log.channel,
        attrs,
        source: Some(source),
        source_ts: Some(log.source_ts),
        source_seq,
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Resolve the `handle_id` returned to the subprocess for a pixel buffer.
///
/// On Linux, the buffer is checked in with the surface-share service so the polyglot
/// subprocess shim can later `check_out` the DMA-BUF FD; the surface-share service-assigned
/// `surface_id` becomes the handle_id. On other platforms the pool id stays
/// as-is (macOS uses its own XPC `check_in_surface` path via the native lib
/// directly; see `streamlib-python-native/src/lib.rs` surface-share service_macos).
#[allow(unused_variables)]
fn assign_buffer_handle_id(
    full: &crate::core::context::GpuContextFullAccess,
    pool_id: &crate::core::rhi::PixelBufferPoolId,
    buffer: &RhiPixelBuffer,
) -> crate::core::error::Result<String> {
    #[cfg(target_os = "linux")]
    {
        if let Some(store) = full.surface_store() {
            return store.check_in(buffer);
        }
    }
    Ok(pool_id.as_str().to_string())
}

/// Resolve the `handle_id` returned to the subprocess for a pooled texture.
///
/// On Linux, register the texture's DMA-BUF with the surface-share service under a fresh UUID
/// so the subprocess can `check_out` it; on other platforms just mint a UUID.
#[allow(unused_variables)]
fn assign_texture_handle_id(
    full: &crate::core::context::GpuContextFullAccess,
    texture: &PooledTextureHandle,
) -> crate::core::error::Result<String> {
    let handle_id = Uuid::new_v4().to_string();
    #[cfg(target_os = "linux")]
    {
        if let Some(store) = full.surface_store() {
            // No timeline semaphore — escalate-IPC consumers (CPU-readback
            // bridge) handle sync via the per-acquire response, not via a
            // shared host timeline.
            //
            // UNDEFINED at registration: pooled textures sit in the
            // texture pool unowned until the first acquire. The host
            // adapter or escalate-IPC bridge transitions to its
            // workload-specific layout on first use; subsequent
            // releases publish the post-release layout via
            // `update_image_layout`.
            store.register_texture(
                &handle_id,
                texture.texture(),
                None,
                streamlib_consumer_rhi::VulkanLayout::UNDEFINED,
            )?;
        }
    }
    Ok(handle_id)
}

/// Resolve the `handle_id` for a render-target DMA-BUF image.
///
/// On Linux, register the image's DMA-BUF (with the chosen DRM modifier and
/// per-plane row pitches) with the surface-share service under a fresh UUID
/// so the subprocess can `check_out` it; the surface-share registration
/// carries the modifier and strides the consumer-side EGL import requires.
#[cfg(target_os = "linux")]
fn assign_image_handle_id(
    full: &crate::core::context::GpuContextFullAccess,
    texture: &crate::core::rhi::StreamTexture,
) -> crate::core::error::Result<String> {
    let handle_id = Uuid::new_v4().to_string();
    if let Some(store) = full.surface_store() {
        // Render-target images are freshly allocated and unwritten at
        // registration time — declare UNDEFINED and let the first
        // producer publish their post-release layout via
        // `update_image_layout` once they've issued their QFOT
        // release barrier (#633).
        store.register_texture(
            &handle_id,
            texture,
            None,
            streamlib_consumer_rhi::VulkanLayout::UNDEFINED,
        )?;
    }
    Ok(handle_id)
}

/// Map a wire-format `run_cpu_readback_copy` request through the
/// registered [`CpuReadbackBridge`].
///
/// Post-#562: the bridge runs the host-side GPU copy on its queue,
/// signals a new value on the surface's shared timeline at end-of-
/// submit, and returns that value. Staging buffers + timeline are
/// already imported by the subprocess (registered with surface-share
/// at startup), so this op carries no FDs — only the surface_id, the
/// direction, and the timeline value the host signaled.
///
/// Failure modes (each surfaced as an [`EscalateResponse::Err`] keyed
/// by the original request_id):
/// 1. `surface_id` doesn't parse as a `u64` — wire format is decimal.
/// 2. No bridge is registered — the host runtime didn't wire a
///    cpu-readback adapter into [`crate::core::context::GpuContext::set_cpu_readback_bridge`].
/// 3. Bridge `run_copy` returned an error — typically "surface not
///    registered" or a Vulkan submit failure inside the adapter.
#[cfg(target_os = "linux")]
fn handle_run_cpu_readback_copy(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    surface_id_str: &str,
    direction: EscalateRequestRunCpuReadbackCopyDirection,
) -> EscalateResponse {
    let bridge_dir = match direction {
        EscalateRequestRunCpuReadbackCopyDirection::ImageToBuffer => {
            CpuReadbackCopyDirection::ImageToBuffer
        }
        EscalateRequestRunCpuReadbackCopyDirection::BufferToImage => {
            CpuReadbackCopyDirection::BufferToImage
        }
    };
    dispatch_run_cpu_readback_copy(
        sandbox,
        rid,
        surface_id_str,
        "run_cpu_readback_copy",
        |bridge, surface_id| match bridge.run_copy(surface_id, bridge_dir) {
            Ok(v) => Ok(Some(v)),
            Err(msg) => Err(msg),
        },
    )
}

/// Map a wire-format `try_run_cpu_readback_copy` request. Behaviour
/// matches [`handle_run_cpu_readback_copy`] on success and on hard
/// error, but surfaces an [`EscalateResponse::Contended`] response
/// when the bridge reports `Ok(None)` — i.e. a competing reader/writer
/// is holding the surface on the host side.
#[cfg(target_os = "linux")]
fn handle_try_run_cpu_readback_copy(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    surface_id_str: &str,
    direction: EscalateRequestTryRunCpuReadbackCopyDirection,
) -> EscalateResponse {
    let bridge_dir = match direction {
        EscalateRequestTryRunCpuReadbackCopyDirection::ImageToBuffer => {
            CpuReadbackCopyDirection::ImageToBuffer
        }
        EscalateRequestTryRunCpuReadbackCopyDirection::BufferToImage => {
            CpuReadbackCopyDirection::BufferToImage
        }
    };
    dispatch_run_cpu_readback_copy(
        sandbox,
        rid,
        surface_id_str,
        "try_run_cpu_readback_copy",
        |bridge, surface_id| bridge.try_run_copy(surface_id, bridge_dir),
    )
}

/// Shared dispatch path for blocking and non-blocking
/// `run_cpu_readback_copy`. `op_label` is the wire op name used in
/// error messages. `bridge_call` returns:
///   - `Ok(Some(timeline_value))` → produce an [`EscalateResponse::Ok`];
///   - `Ok(None)`                  → produce an [`EscalateResponse::Contended`];
///   - `Err(msg)`                  → produce an [`EscalateResponse::Err`].
#[cfg(target_os = "linux")]
fn dispatch_run_cpu_readback_copy<F>(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    surface_id_str: &str,
    op_label: &str,
    bridge_call: F,
) -> EscalateResponse
where
    F: FnOnce(
        &dyn CpuReadbackBridge,
        streamlib_adapter_abi::SurfaceId,
    ) -> std::result::Result<Option<u64>, String>,
{
    use std::sync::Arc;

    let surface_id: streamlib_adapter_abi::SurfaceId = match surface_id_str.parse() {
        Ok(v) => v,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!(
                    "{op_label}: surface_id '{surface_id_str}' is not a u64 decimal: {e}"
                ),
            });
        }
    };

    let bridge: Arc<dyn CpuReadbackBridge> = match sandbox.escalate(|full| {
        full.cpu_readback_bridge().ok_or_else(|| {
            crate::core::error::StreamError::Configuration(format!(
                "{op_label}: no CpuReadbackBridge registered on GpuContext"
            ))
        })
    }) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: e.to_string(),
            });
        }
    };

    let signaled = match bridge_call(bridge.as_ref(), surface_id) {
        Ok(Some(v)) => v,
        Ok(None) => {
            return EscalateResponse::Contended(EscalateResponseContended {
                request_id: rid,
            });
        }
        Err(msg) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("{op_label} bridge call failed: {msg}"),
            });
        }
    };

    EscalateResponse::Ok(EscalateResponseOk {
        request_id: rid,
        // Handle ID is meaningless for cpu-readback after Path E — the
        // subprocess holds no host-side resource lifetime; the staging
        // buffers are pre-registered surface-share entries that live
        // for the surface's lifetime, not per acquire. Keep the field
        // populated with the surface_id for symmetry, but the
        // subprocess never has anything to release for cpu-readback.
        handle_id: surface_id_str.to_string(),
        width: None,
        height: None,
        format: None,
        usage: None,
        timeline_value: Some(signaled.to_string()),
    })
}

/// Map a wire-format `register_compute_kernel` request through the
/// registered [`ComputeKernelBridge`].
///
/// The bridge derives the kernel's binding shape from `rspirv-reflect`,
/// builds the `VulkanComputeKernel` (with on-disk pipeline cache
/// persistence keyed by SHA-256 of the SPIR-V), and returns the same
/// hash hex back as `kernel_id`. Subsequent identical SPIR-V hits the
/// host-side cache and returns the same id without re-reflecting.
///
/// Failure modes (each surfaced as an [`EscalateResponse::Err`] keyed
/// by the original request_id):
/// 1. `spv_hex` doesn't decode as hex bytes.
/// 2. No bridge is registered — the host runtime didn't wire a
///    compute-kernel bridge into [`crate::core::context::GpuContext::set_compute_kernel_bridge`].
/// 3. Bridge `register` returned an error — typically reflection
///    failure, push-constant size mismatch, or pipeline build failure.
#[cfg(target_os = "linux")]
fn handle_register_compute_kernel(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    spv_hex: &str,
    push_constant_size: u32,
) -> EscalateResponse {
    use std::sync::Arc;

    let spv = match decode_hex(spv_hex) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("register_compute_kernel: spv_hex decode: {e}"),
            });
        }
    };

    let bridge: Arc<dyn ComputeKernelBridge> = match sandbox.escalate(|full| {
        full.compute_kernel_bridge().ok_or_else(|| {
            crate::core::error::StreamError::Configuration(
                "register_compute_kernel: no ComputeKernelBridge registered on GpuContext"
                    .to_string(),
            )
        })
    }) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: e.to_string(),
            });
        }
    };

    match bridge.register(&spv, push_constant_size) {
        Ok(kernel_id) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id: kernel_id,
            width: None,
            height: None,
            format: None,
            usage: None,
            timeline_value: None,
        }),
        Err(msg) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("register_compute_kernel bridge call failed: {msg}"),
        }),
    }
}

/// Map a wire-format `run_compute_kernel` request through the
/// registered [`ComputeKernelBridge`].
///
/// Compute dispatch on the host is synchronous: the bridge's `run`
/// blocks on the kernel's fence before returning, so by the time this
/// function emits an `Ok` response, the GPU work has retired and the
/// host's writes to the surface's `VkImage` are visible to any
/// subsequent submission against the same VkDevice. The subprocess can
/// safely advance its surface-share timeline on receipt of the `ok`.
///
/// Failure modes (each surfaced as an [`EscalateResponse::Err`] keyed
/// by the original request_id):
/// 1. `push_constants_hex` doesn't decode as hex bytes.
/// 2. No bridge is registered.
/// 3. Bridge `run` returned an error — typically unrecognized
///    `kernel_id`, surface lookup failure, or Vulkan submit failure.
#[cfg(target_os = "linux")]
fn handle_run_compute_kernel(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    kernel_id: &str,
    surface_uuid: &str,
    push_constants_hex: &str,
    group_count_x: u32,
    group_count_y: u32,
    group_count_z: u32,
) -> EscalateResponse {
    use std::sync::Arc;

    let push_constants = match decode_hex(push_constants_hex) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("run_compute_kernel: push_constants_hex decode: {e}"),
            });
        }
    };

    let bridge: Arc<dyn ComputeKernelBridge> = match sandbox.escalate(|full| {
        full.compute_kernel_bridge().ok_or_else(|| {
            crate::core::error::StreamError::Configuration(
                "run_compute_kernel: no ComputeKernelBridge registered on GpuContext"
                    .to_string(),
            )
        })
    }) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: e.to_string(),
            });
        }
    };

    match bridge.run(
        kernel_id,
        surface_uuid,
        &push_constants,
        group_count_x,
        group_count_y,
        group_count_z,
    ) {
        Ok(()) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            // Echo the kernel_id back — compute is sync host-side, no
            // separate handle is allocated per dispatch.
            handle_id: kernel_id.to_string(),
            width: None,
            height: None,
            format: None,
            usage: None,
            timeline_value: None,
        }),
        Err(msg) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("run_compute_kernel bridge call failed: {msg}"),
        }),
    }
}

/// Map a wire-format `register_graphics_kernel` request through the
/// registered [`GraphicsKernelBridge`].
///
/// Decodes the per-stage SPIR-V hex blobs, translates the wire-format
/// pipeline-state enums into the bridge's typed [`GraphicsPipelineStateWire`],
/// and asks the bridge to register the kernel. The bridge returns a
/// stable `kernel_id` (recommended: SHA-256 over a canonical
/// representation of all register-time inputs); identical re-registration
/// hits the bridge's cache and returns the same id.
///
/// Failure modes (each surfaced as an [`EscalateResponse::Err`] keyed
/// by the original request_id):
/// 1. `vertex_spv_hex` / `fragment_spv_hex` doesn't decode as hex bytes.
/// 2. No bridge is registered.
/// 3. Bridge `register` returned an error — typically reflection
///    failure, push-constant size mismatch, pipeline-state validation
///    failure, or pipeline build failure.
#[cfg(target_os = "linux")]
fn handle_register_graphics_kernel(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRegisterGraphicsKernel,
) -> EscalateResponse {
    use std::sync::Arc;

    let vertex_spv = match decode_hex(&req.vertex_spv_hex) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("register_graphics_kernel: vertex_spv_hex decode: {e}"),
            });
        }
    };
    let fragment_spv = match decode_hex(&req.fragment_spv_hex) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("register_graphics_kernel: fragment_spv_hex decode: {e}"),
            });
        }
    };

    let bridge: Arc<dyn GraphicsKernelBridge> = match sandbox.escalate(|full| {
        full.graphics_kernel_bridge().ok_or_else(|| {
            crate::core::error::StreamError::Configuration(
                "register_graphics_kernel: no GraphicsKernelBridge registered on GpuContext"
                    .to_string(),
            )
        })
    }) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: e.to_string(),
            });
        }
    };

    let bindings: Vec<GraphicsBindingDecl> = req
        .bindings
        .into_iter()
        .map(|b| GraphicsBindingDecl {
            binding: b.binding,
            kind: graphics_register_binding_kind_from_wire(b.kind),
            stages: b.stages,
        })
        .collect();

    let pipeline_state = match graphics_pipeline_state_from_wire(req.pipeline_state) {
        Ok(p) => p,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("register_graphics_kernel: pipeline_state: {e}"),
            });
        }
    };

    let decl = GraphicsKernelRegisterDecl {
        label: req.label,
        vertex_spv,
        fragment_spv,
        vertex_entry_point: req.vertex_entry_point,
        fragment_entry_point: req.fragment_entry_point,
        bindings,
        push_constant_size: req.push_constant_size,
        push_constant_stages: req.push_constant_stages,
        descriptor_sets_in_flight: req.descriptor_sets_in_flight,
        pipeline_state,
    };

    match bridge.register(&decl) {
        Ok(kernel_id) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id: kernel_id,
            width: None,
            height: None,
            format: None,
            usage: None,
            timeline_value: None,
        }),
        Err(msg) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("register_graphics_kernel bridge call failed: {msg}"),
        }),
    }
}

/// Map a wire-format `run_graphics_draw` request through the registered
/// [`GraphicsKernelBridge`].
///
/// Graphics dispatch on the host is synchronous (the bridge calls
/// [`crate::vulkan::rhi::VulkanGraphicsKernel::offscreen_render`] which
/// submits + waits on its own command buffer + fence), so by the time
/// this function returns `Ok`, the GPU work has retired and the host's
/// writes to the color attachments are visible.
///
/// Failure modes (each surfaced as an [`EscalateResponse::Err`] keyed
/// by the original request_id):
/// 1. `push_constants_hex` doesn't decode as hex bytes.
/// 2. Vertex/index buffer offset doesn't parse as decimal u64.
/// 3. No bridge is registered.
/// 4. Bridge `run_draw` returned an error — typically unrecognized
///    `kernel_id`, surface lookup failure, or Vulkan submit failure.
#[cfg(target_os = "linux")]
fn handle_run_graphics_draw(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRunGraphicsDraw,
) -> EscalateResponse {
    use std::sync::Arc;

    let push_constants = match decode_hex(&req.push_constants_hex) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("run_graphics_draw: push_constants_hex decode: {e}"),
            });
        }
    };

    let bindings: Vec<GraphicsBindingValue> = req
        .bindings
        .into_iter()
        .map(|b| GraphicsBindingValue {
            binding: b.binding,
            kind: graphics_run_binding_kind_from_wire(b.kind),
            surface_uuid: b.surface_uuid,
        })
        .collect();

    let mut vertex_buffers: Vec<GraphicsVertexBufferBinding> =
        Vec::with_capacity(req.vertex_buffers.len());
    for vb in req.vertex_buffers.into_iter() {
        let offset = match vb.offset.parse::<u64>() {
            Ok(v) => v,
            Err(e) => {
                return EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: format!(
                        "run_graphics_draw: vertex_buffer.offset '{}' is not a decimal u64: {e}",
                        vb.offset
                    ),
                });
            }
        };
        vertex_buffers.push(GraphicsVertexBufferBinding {
            binding: vb.binding,
            surface_uuid: vb.surface_uuid,
            offset,
        });
    }

    let index_buffer = if let Some(ib) = req.index_buffer {
        let offset = match ib.offset.parse::<u64>() {
            Ok(v) => v,
            Err(e) => {
                return EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: format!(
                        "run_graphics_draw: index_buffer.offset '{}' is not a decimal u64: {e}",
                        ib.offset
                    ),
                });
            }
        };
        Some(GraphicsIndexBufferBinding {
            surface_uuid: ib.surface_uuid,
            offset,
            index_type: match ib.index_type {
                EscalateRequestRunGraphicsDrawIndexBufferIndexType::Uint16 => IndexTypeWire::Uint16,
                EscalateRequestRunGraphicsDrawIndexBufferIndexType::Uint32 => IndexTypeWire::Uint32,
            },
        })
    } else {
        None
    };

    let viewport = req.viewport.map(|v| ViewportWire {
        x: v.x,
        y: v.y,
        width: v.width,
        height: v.height,
        min_depth: v.min_depth,
        max_depth: v.max_depth,
    });
    let scissor = req.scissor.map(|s| ScissorRectWire {
        x: s.x,
        y: s.y,
        width: s.width,
        height: s.height,
    });

    let draw = match req.draw.kind {
        EscalateRequestRunGraphicsDrawDrawKind::Draw => GraphicsDrawSpec::Draw {
            vertex_count: req.draw.vertex_count,
            instance_count: req.draw.instance_count,
            first_vertex: req.draw.first_vertex,
            first_instance: req.draw.first_instance,
        },
        EscalateRequestRunGraphicsDrawDrawKind::DrawIndexed => GraphicsDrawSpec::DrawIndexed {
            index_count: req.draw.index_count,
            instance_count: req.draw.instance_count,
            first_index: req.draw.first_index,
            vertex_offset: req.draw.vertex_offset,
            first_instance: req.draw.first_instance,
        },
    };

    let kernel_id = req.kernel_id;
    let domain = GraphicsKernelRunDraw {
        kernel_id: kernel_id.clone(),
        frame_index: req.frame_index,
        bindings,
        vertex_buffers,
        index_buffer,
        color_target_uuids: req.color_target_uuids,
        depth_target_uuid: req.depth_target_uuid,
        extent: (req.extent_width, req.extent_height),
        push_constants,
        viewport,
        scissor,
        draw,
    };

    let bridge: Arc<dyn GraphicsKernelBridge> = match sandbox.escalate(|full| {
        full.graphics_kernel_bridge().ok_or_else(|| {
            crate::core::error::StreamError::Configuration(
                "run_graphics_draw: no GraphicsKernelBridge registered on GpuContext".to_string(),
            )
        })
    }) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: e.to_string(),
            });
        }
    };

    match bridge.run_draw(&domain) {
        Ok(()) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id: kernel_id,
            width: None,
            height: None,
            format: None,
            usage: None,
            timeline_value: None,
        }),
        Err(msg) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("run_graphics_draw bridge call failed: {msg}"),
        }),
    }
}

/// Map a wire-format `register_acceleration_structure_blas` request
/// through the registered [`RayTracingKernelBridge`].
///
/// Decodes the hex-encoded vertex (`f32` triples) and index (`u32`
/// triples) blobs, validates triangle-shape consistency, and asks the
/// bridge to build a triangle BLAS. Returns the bridge-assigned
/// `as_id` on success.
///
/// Failure modes (each surfaced as an [`EscalateResponse::Err`] keyed
/// by the original request_id):
/// 1. `vertices_hex` / `indices_hex` doesn't decode as hex bytes.
/// 2. Vertex blob length is not a multiple of 12 (one f32 = 4 bytes;
///    one vertex = 3 floats = 12 bytes).
/// 3. Index blob length is not a multiple of 12 (one u32 = 4 bytes;
///    one triangle = 3 indices = 12 bytes).
/// 4. No bridge is registered.
/// 5. Bridge `register_blas` returned an error — typically empty
///    geometry, missing RT extensions, or AS-build submit failure.
#[cfg(target_os = "linux")]
fn handle_register_acceleration_structure_blas(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRegisterAccelerationStructureBlas,
) -> EscalateResponse {
    use std::sync::Arc;

    let vertex_bytes = match decode_hex(&req.vertices_hex) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!(
                    "register_acceleration_structure_blas: vertices_hex decode: {e}"
                ),
            });
        }
    };
    if vertex_bytes.len() % 12 != 0 {
        return EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!(
                "register_acceleration_structure_blas: vertex blob length {} is not a \
                 multiple of 12 bytes (one vertex = 3 × f32)",
                vertex_bytes.len()
            ),
        });
    }
    let vertices: Vec<f32> = vertex_bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    let index_bytes = match decode_hex(&req.indices_hex) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!(
                    "register_acceleration_structure_blas: indices_hex decode: {e}"
                ),
            });
        }
    };
    if index_bytes.len() % 12 != 0 {
        return EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!(
                "register_acceleration_structure_blas: index blob length {} is not a \
                 multiple of 12 bytes (one triangle = 3 × u32)",
                index_bytes.len()
            ),
        });
    }
    let indices: Vec<u32> = index_bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    let bridge: Arc<dyn RayTracingKernelBridge> = match sandbox.escalate(|full| {
        full.ray_tracing_kernel_bridge().ok_or_else(|| {
            crate::core::error::StreamError::Configuration(
                "register_acceleration_structure_blas: no RayTracingKernelBridge \
                 registered on GpuContext"
                    .to_string(),
            )
        })
    }) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: e.to_string(),
            });
        }
    };

    let decl = BlasRegisterDecl {
        label: req.label,
        vertices,
        indices,
    };
    match bridge.register_blas(&decl) {
        Ok(as_id) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id: as_id,
            width: None,
            height: None,
            format: None,
            usage: None,
            timeline_value: None,
        }),
        Err(msg) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!(
                "register_acceleration_structure_blas bridge call failed: {msg}"
            ),
        }),
    }
}

/// Map a wire-format `register_acceleration_structure_tlas` request
/// through the registered [`RayTracingKernelBridge`].
///
/// Validates each instance's transform layout (exactly 12 floats —
/// row-major 3×4) and 8-bit mask, then asks the bridge to build a
/// TLAS. The bridge resolves each `blas_id` against its own map.
///
/// Failure modes (each surfaced as an [`EscalateResponse::Err`] keyed
/// by the original request_id):
/// 1. Instance `transform` length isn't 12 floats.
/// 2. Instance `mask` exceeds 0xff (JTD has no native u8).
/// 3. No bridge is registered.
/// 4. Bridge `register_tlas` returned an error — typically empty
///    instance list, unknown blas_id, kind mismatch (a TLAS appearing
///    as a BLAS reference), or AS-build submit failure.
#[cfg(target_os = "linux")]
fn handle_register_acceleration_structure_tlas(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRegisterAccelerationStructureTlas,
) -> EscalateResponse {
    use std::sync::Arc;

    let mut instances: Vec<TlasInstanceDeclWire> = Vec::with_capacity(req.instances.len());
    for (idx, inst) in req.instances.into_iter().enumerate() {
        if inst.transform.len() != 12 {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!(
                    "register_acceleration_structure_tlas: instance {idx} transform has \
                     {} floats, expected exactly 12 (row-major 3×4)",
                    inst.transform.len()
                ),
            });
        }
        if inst.mask > 0xff {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!(
                    "register_acceleration_structure_tlas: instance {idx} mask {} > 0xff \
                     (mask is 8-bit; wire form is uint32)",
                    inst.mask
                ),
            });
        }
        let t = &inst.transform;
        let transform = [
            [t[0], t[1], t[2], t[3]],
            [t[4], t[5], t[6], t[7]],
            [t[8], t[9], t[10], t[11]],
        ];
        instances.push(TlasInstanceDeclWire {
            blas_id: inst.blas_id,
            transform,
            custom_index: inst.custom_index,
            mask: inst.mask as u8,
            sbt_record_offset: inst.sbt_record_offset,
            flags: inst.flags,
        });
    }

    let bridge: Arc<dyn RayTracingKernelBridge> = match sandbox.escalate(|full| {
        full.ray_tracing_kernel_bridge().ok_or_else(|| {
            crate::core::error::StreamError::Configuration(
                "register_acceleration_structure_tlas: no RayTracingKernelBridge \
                 registered on GpuContext"
                    .to_string(),
            )
        })
    }) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: e.to_string(),
            });
        }
    };

    let decl = TlasRegisterDecl {
        label: req.label,
        instances,
    };
    match bridge.register_tlas(&decl) {
        Ok(as_id) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id: as_id,
            width: None,
            height: None,
            format: None,
            usage: None,
            timeline_value: None,
        }),
        Err(msg) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!(
                "register_acceleration_structure_tlas bridge call failed: {msg}"
            ),
        }),
    }
}

/// Map a wire-format `register_ray_tracing_kernel` request through
/// the registered [`RayTracingKernelBridge`].
///
/// Decodes per-stage SPIR-V hex blobs, translates the wire-format
/// stage / group / binding kinds into the bridge's typed mirrors, and
/// asks the bridge to register the kernel. The bridge returns a
/// stable `kernel_id` (typically SHA-256 over a canonical
/// representation of all register-time inputs); identical
/// re-registration hits the bridge's cache and returns the same id.
///
/// Failure modes (each surfaced as an [`EscalateResponse::Err`] keyed
/// by the original request_id):
/// 1. Any stage's `spv_hex` doesn't decode as hex bytes.
/// 2. No bridge is registered.
/// 3. Bridge `register_kernel` returned an error — typically
///    reflection failure, push-constant size mismatch, group/stage
///    inconsistency, or pipeline build failure.
#[cfg(target_os = "linux")]
fn handle_register_ray_tracing_kernel(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRegisterRayTracingKernel,
) -> EscalateResponse {
    use std::sync::Arc;

    let mut stages: Vec<RayTracingStageDecl> = Vec::with_capacity(req.stages.len());
    for (idx, st) in req.stages.into_iter().enumerate() {
        let spv = match decode_hex(&st.spv_hex) {
            Ok(b) => b,
            Err(e) => {
                return EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: format!(
                        "register_ray_tracing_kernel: stages[{idx}].spv_hex decode: {e}"
                    ),
                });
            }
        };
        stages.push(RayTracingStageDecl {
            stage: ray_tracing_stage_from_wire(st.stage),
            spv,
            entry_point: st.entry_point,
        });
    }

    let mut groups: Vec<RayTracingShaderGroupWire> = Vec::with_capacity(req.groups.len());
    for (idx, g) in req.groups.into_iter().enumerate() {
        let group = match g.kind {
            EscalateRequestRegisterRayTracingKernelGroupKind::General => {
                RayTracingShaderGroupWire::General {
                    general_stage: g.general_stage,
                }
            }
            EscalateRequestRegisterRayTracingKernelGroupKind::TrianglesHit => {
                RayTracingShaderGroupWire::TrianglesHit {
                    closest_hit_stage: optional_stage(g.closest_hit_stage),
                    any_hit_stage: optional_stage(g.any_hit_stage),
                }
            }
            EscalateRequestRegisterRayTracingKernelGroupKind::ProceduralHit => {
                if g.intersection_stage == RAY_TRACING_STAGE_INDEX_NONE {
                    return EscalateResponse::Err(EscalateResponseErr {
                        request_id: rid,
                        message: format!(
                            "register_ray_tracing_kernel: groups[{idx}] procedural_hit \
                             must set intersection_stage (got {RAY_TRACING_STAGE_INDEX_NONE} \
                             which is the absent-sentinel)"
                        ),
                    });
                }
                RayTracingShaderGroupWire::ProceduralHit {
                    intersection_stage: g.intersection_stage,
                    closest_hit_stage: optional_stage(g.closest_hit_stage),
                    any_hit_stage: optional_stage(g.any_hit_stage),
                }
            }
        };
        groups.push(group);
    }

    let bindings: Vec<RayTracingBindingDecl> = req
        .bindings
        .into_iter()
        .map(|b| RayTracingBindingDecl {
            binding: b.binding,
            kind: ray_tracing_register_binding_kind_from_wire(b.kind),
            stages: b.stages,
        })
        .collect();

    let bridge: Arc<dyn RayTracingKernelBridge> = match sandbox.escalate(|full| {
        full.ray_tracing_kernel_bridge().ok_or_else(|| {
            crate::core::error::StreamError::Configuration(
                "register_ray_tracing_kernel: no RayTracingKernelBridge registered on \
                 GpuContext"
                    .to_string(),
            )
        })
    }) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: e.to_string(),
            });
        }
    };

    let decl = RayTracingKernelRegisterDecl {
        label: req.label,
        stages,
        groups,
        bindings,
        push_constant_size: req.push_constant_size,
        push_constant_stages: req.push_constant_stages,
        max_recursion_depth: req.max_recursion_depth,
    };

    match bridge.register_kernel(&decl) {
        Ok(kernel_id) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id: kernel_id,
            width: None,
            height: None,
            format: None,
            usage: None,
            timeline_value: None,
        }),
        Err(msg) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("register_ray_tracing_kernel bridge call failed: {msg}"),
        }),
    }
}

/// Map a wire-format `run_ray_tracing_kernel` request through the
/// registered [`RayTracingKernelBridge`].
///
/// RT dispatch on the host is synchronous (the bridge calls
/// [`crate::vulkan::rhi::VulkanRayTracingKernel::trace_rays`] which
/// submits + waits on its own command buffer + fence), so by the time
/// this function returns `Ok`, the GPU work has retired and the
/// host's writes to the storage image are visible.
///
/// Failure modes (each surfaced as an [`EscalateResponse::Err`] keyed
/// by the original request_id):
/// 1. `push_constants_hex` doesn't decode as hex bytes.
/// 2. No bridge is registered.
/// 3. Bridge `run_kernel` returned an error — typically unrecognized
///    `kernel_id`, target lookup failure (binding `target_id` doesn't
///    resolve in the bridge's surface / AS map), or Vulkan submit
///    failure.
#[cfg(target_os = "linux")]
fn handle_run_ray_tracing_kernel(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRunRayTracingKernel,
) -> EscalateResponse {
    use std::sync::Arc;

    let push_constants = match decode_hex(&req.push_constants_hex) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("run_ray_tracing_kernel: push_constants_hex decode: {e}"),
            });
        }
    };

    let bindings: Vec<RayTracingBindingValue> = req
        .bindings
        .into_iter()
        .map(|b| RayTracingBindingValue {
            binding: b.binding,
            kind: ray_tracing_run_binding_kind_from_wire(b.kind),
            target_id: b.target_id,
        })
        .collect();

    let bridge: Arc<dyn RayTracingKernelBridge> = match sandbox.escalate(|full| {
        full.ray_tracing_kernel_bridge().ok_or_else(|| {
            crate::core::error::StreamError::Configuration(
                "run_ray_tracing_kernel: no RayTracingKernelBridge registered on \
                 GpuContext"
                    .to_string(),
            )
        })
    }) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: e.to_string(),
            });
        }
    };

    let kernel_id = req.kernel_id;
    let dispatch = RayTracingKernelRunDispatch {
        kernel_id: kernel_id.clone(),
        bindings,
        push_constants,
        width: req.width,
        height: req.height,
        depth: req.depth,
    };

    match bridge.run_kernel(&dispatch) {
        Ok(()) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id: kernel_id,
            width: None,
            height: None,
            format: None,
            usage: None,
            timeline_value: None,
        }),
        Err(msg) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("run_ray_tracing_kernel bridge call failed: {msg}"),
        }),
    }
}

/// Convert a sentinel-encoded wire stage index back into an
/// `Option<u32>`. The wire form uses `0xFFFFFFFF` to mean "absent"
/// because JTD has no `Option<uint32>`.
#[cfg(target_os = "linux")]
fn optional_stage(idx: u32) -> Option<u32> {
    if idx == RAY_TRACING_STAGE_INDEX_NONE {
        None
    } else {
        Some(idx)
    }
}

#[cfg(target_os = "linux")]
fn ray_tracing_stage_from_wire(
    stage: EscalateRequestRegisterRayTracingKernelStageStage,
) -> RayTracingShaderStageWire {
    use EscalateRequestRegisterRayTracingKernelStageStage as W;
    match stage {
        W::RayGen => RayTracingShaderStageWire::RayGen,
        W::Miss => RayTracingShaderStageWire::Miss,
        W::ClosestHit => RayTracingShaderStageWire::ClosestHit,
        W::AnyHit => RayTracingShaderStageWire::AnyHit,
        W::Intersection => RayTracingShaderStageWire::Intersection,
        W::Callable => RayTracingShaderStageWire::Callable,
    }
}

#[cfg(target_os = "linux")]
fn ray_tracing_register_binding_kind_from_wire(
    kind: EscalateRequestRegisterRayTracingKernelBindingKind,
) -> RayTracingBindingKindWire {
    use EscalateRequestRegisterRayTracingKernelBindingKind as W;
    match kind {
        W::StorageBuffer => RayTracingBindingKindWire::StorageBuffer,
        W::UniformBuffer => RayTracingBindingKindWire::UniformBuffer,
        W::SampledTexture => RayTracingBindingKindWire::SampledTexture,
        W::StorageImage => RayTracingBindingKindWire::StorageImage,
        W::AccelerationStructure => RayTracingBindingKindWire::AccelerationStructure,
    }
}

#[cfg(target_os = "linux")]
fn ray_tracing_run_binding_kind_from_wire(
    kind: EscalateRequestRunRayTracingKernelBindingKind,
) -> RayTracingBindingKindWire {
    use EscalateRequestRunRayTracingKernelBindingKind as W;
    match kind {
        W::StorageBuffer => RayTracingBindingKindWire::StorageBuffer,
        W::UniformBuffer => RayTracingBindingKindWire::UniformBuffer,
        W::SampledTexture => RayTracingBindingKindWire::SampledTexture,
        W::StorageImage => RayTracingBindingKindWire::StorageImage,
        W::AccelerationStructure => RayTracingBindingKindWire::AccelerationStructure,
    }
}

#[cfg(target_os = "linux")]
fn graphics_register_binding_kind_from_wire(
    kind: EscalateRequestRegisterGraphicsKernelBindingKind,
) -> GraphicsBindingKindWire {
    match kind {
        EscalateRequestRegisterGraphicsKernelBindingKind::SampledTexture => {
            GraphicsBindingKindWire::SampledTexture
        }
        EscalateRequestRegisterGraphicsKernelBindingKind::StorageBuffer => {
            GraphicsBindingKindWire::StorageBuffer
        }
        EscalateRequestRegisterGraphicsKernelBindingKind::UniformBuffer => {
            GraphicsBindingKindWire::UniformBuffer
        }
        EscalateRequestRegisterGraphicsKernelBindingKind::StorageImage => {
            GraphicsBindingKindWire::StorageImage
        }
    }
}

#[cfg(target_os = "linux")]
fn graphics_run_binding_kind_from_wire(
    kind: EscalateRequestRunGraphicsDrawBindingKind,
) -> GraphicsBindingKindWire {
    match kind {
        EscalateRequestRunGraphicsDrawBindingKind::SampledTexture => {
            GraphicsBindingKindWire::SampledTexture
        }
        EscalateRequestRunGraphicsDrawBindingKind::StorageBuffer => {
            GraphicsBindingKindWire::StorageBuffer
        }
        EscalateRequestRunGraphicsDrawBindingKind::UniformBuffer => {
            GraphicsBindingKindWire::UniformBuffer
        }
        EscalateRequestRunGraphicsDrawBindingKind::StorageImage => {
            GraphicsBindingKindWire::StorageImage
        }
    }
}

#[cfg(target_os = "linux")]
fn graphics_pipeline_state_from_wire(
    p: EscalateRequestRegisterGraphicsKernelPipelineState,
) -> std::result::Result<GraphicsPipelineStateWire, String> {
    let topology = match p.topology {
        EscalateRequestRegisterGraphicsKernelPipelineStateTopology::PointList => {
            PrimitiveTopologyWire::PointList
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateTopology::LineList => {
            PrimitiveTopologyWire::LineList
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateTopology::LineStrip => {
            PrimitiveTopologyWire::LineStrip
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateTopology::TriangleList => {
            PrimitiveTopologyWire::TriangleList
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateTopology::TriangleStrip => {
            PrimitiveTopologyWire::TriangleStrip
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateTopology::TriangleFan => {
            PrimitiveTopologyWire::TriangleFan
        }
    };
    let vertex_input_bindings = p
        .vertex_input_bindings
        .into_iter()
        .map(|b| VertexInputBindingDecl {
            binding: b.binding,
            stride: b.stride,
            input_rate: match b.input_rate {
                EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBindingInputRate::Vertex => {
                    VertexInputRateWire::Vertex
                }
                EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBindingInputRate::Instance => {
                    VertexInputRateWire::Instance
                }
            },
        })
        .collect::<Vec<_>>();
    let vertex_input_attributes = p
        .vertex_input_attributes
        .into_iter()
        .map(|a| {
            Ok::<_, String>(VertexInputAttributeDecl {
                location: a.location,
                binding: a.binding,
                format: vertex_attribute_format_from_wire(a.format),
                offset: a.offset,
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let rasterization_polygon_mode = match p.rasterization_polygon_mode {
        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode::Fill => {
            PolygonModeWire::Fill
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode::Line => {
            PolygonModeWire::Line
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode::Point => {
            PolygonModeWire::Point
        }
    };
    let rasterization_cull_mode = match p.rasterization_cull_mode {
        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode::None => {
            CullModeWire::None
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode::Front => {
            CullModeWire::Front
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode::Back => {
            CullModeWire::Back
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode::FrontAndBack => {
            CullModeWire::FrontAndBack
        }
    };
    let rasterization_front_face = match p.rasterization_front_face {
        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace::CounterClockwise => {
            FrontFaceWire::CounterClockwise
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace::Clockwise => {
            FrontFaceWire::Clockwise
        }
    };
    let depth_compare_op = depth_compare_op_from_wire(p.depth_compare_op);
    let color_blend_src_color_factor = blend_factor_from_wire_src_color(p.color_blend_src_color_factor);
    let color_blend_dst_color_factor = blend_factor_from_wire_dst_color(p.color_blend_dst_color_factor);
    let color_blend_color_op = blend_op_from_wire_color(p.color_blend_color_op);
    let color_blend_src_alpha_factor = blend_factor_from_wire_src_alpha(p.color_blend_src_alpha_factor);
    let color_blend_dst_alpha_factor = blend_factor_from_wire_dst_alpha(p.color_blend_dst_alpha_factor);
    let color_blend_alpha_op = blend_op_from_wire_alpha(p.color_blend_alpha_op);
    let attachment_depth_format = p.attachment_depth_format.map(|d| match d {
        EscalateRequestRegisterGraphicsKernelPipelineStateAttachmentDepthFormat::D16Unorm => {
            DepthFormatWire::D16Unorm
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateAttachmentDepthFormat::D32Sfloat => {
            DepthFormatWire::D32Sfloat
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateAttachmentDepthFormat::D24UnormS8Uint => {
            DepthFormatWire::D24UnormS8Uint
        }
    });
    let dynamic_state = match p.dynamic_state {
        EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState::None => {
            DynamicStateWire::None
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState::ViewportScissor => {
            DynamicStateWire::ViewportScissor
        }
    };

    Ok(GraphicsPipelineStateWire {
        topology,
        vertex_input_bindings,
        vertex_input_attributes,
        rasterization_polygon_mode,
        rasterization_cull_mode,
        rasterization_front_face,
        rasterization_line_width: p.rasterization_line_width,
        multisample_samples: p.multisample_samples,
        depth_stencil_enabled: p.depth_stencil_enabled,
        depth_compare_op,
        depth_write: p.depth_write,
        color_blend_enabled: p.color_blend_enabled,
        color_write_mask: p.color_write_mask,
        color_blend_src_color_factor,
        color_blend_dst_color_factor,
        color_blend_color_op,
        color_blend_src_alpha_factor,
        color_blend_dst_alpha_factor,
        color_blend_alpha_op,
        attachment_color_formats: p.attachment_color_formats,
        attachment_depth_format,
        dynamic_state,
    })
}

#[cfg(target_os = "linux")]
fn vertex_attribute_format_from_wire(
    fmt: EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttributeFormat,
) -> VertexAttributeFormatWire {
    use EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttributeFormat as W;
    match fmt {
        W::R32Float => VertexAttributeFormatWire::R32Float,
        W::Rg32Float => VertexAttributeFormatWire::Rg32Float,
        W::Rgb32Float => VertexAttributeFormatWire::Rgb32Float,
        W::Rgba32Float => VertexAttributeFormatWire::Rgba32Float,
        W::R32Uint => VertexAttributeFormatWire::R32Uint,
        W::Rg32Uint => VertexAttributeFormatWire::Rg32Uint,
        W::Rgb32Uint => VertexAttributeFormatWire::Rgb32Uint,
        W::Rgba32Uint => VertexAttributeFormatWire::Rgba32Uint,
        W::R32Sint => VertexAttributeFormatWire::R32Sint,
        W::Rg32Sint => VertexAttributeFormatWire::Rg32Sint,
        W::Rgb32Sint => VertexAttributeFormatWire::Rgb32Sint,
        W::Rgba32Sint => VertexAttributeFormatWire::Rgba32Sint,
        W::Rgba8Unorm => VertexAttributeFormatWire::Rgba8Unorm,
        W::Rgba8Snorm => VertexAttributeFormatWire::Rgba8Snorm,
    }
}

#[cfg(target_os = "linux")]
fn depth_compare_op_from_wire(
    op: EscalateRequestRegisterGraphicsKernelPipelineStateDepthCompareOp,
) -> DepthCompareOpWire {
    use EscalateRequestRegisterGraphicsKernelPipelineStateDepthCompareOp as W;
    match op {
        W::Never => DepthCompareOpWire::Never,
        W::Less => DepthCompareOpWire::Less,
        W::Equal => DepthCompareOpWire::Equal,
        W::LessOrEqual => DepthCompareOpWire::LessOrEqual,
        W::Greater => DepthCompareOpWire::Greater,
        W::NotEqual => DepthCompareOpWire::NotEqual,
        W::GreaterOrEqual => DepthCompareOpWire::GreaterOrEqual,
        W::Always => DepthCompareOpWire::Always,
    }
}

#[cfg(target_os = "linux")]
macro_rules! blend_factor_match {
    ($enum:ident, $val:expr) => {{
        use $enum as W;
        match $val {
            W::Zero => BlendFactorWire::Zero,
            W::One => BlendFactorWire::One,
            W::SrcColor => BlendFactorWire::SrcColor,
            W::OneMinusSrcColor => BlendFactorWire::OneMinusSrcColor,
            W::DstColor => BlendFactorWire::DstColor,
            W::OneMinusDstColor => BlendFactorWire::OneMinusDstColor,
            W::SrcAlpha => BlendFactorWire::SrcAlpha,
            W::OneMinusSrcAlpha => BlendFactorWire::OneMinusSrcAlpha,
            W::DstAlpha => BlendFactorWire::DstAlpha,
            W::OneMinusDstAlpha => BlendFactorWire::OneMinusDstAlpha,
            W::ConstantColor => BlendFactorWire::ConstantColor,
            W::OneMinusConstantColor => BlendFactorWire::OneMinusConstantColor,
            W::ConstantAlpha => BlendFactorWire::ConstantAlpha,
            W::OneMinusConstantAlpha => BlendFactorWire::OneMinusConstantAlpha,
            W::SrcAlphaSaturate => BlendFactorWire::SrcAlphaSaturate,
        }
    }};
}

#[cfg(target_os = "linux")]
macro_rules! blend_op_match {
    ($enum:ident, $val:expr) => {{
        use $enum as W;
        match $val {
            W::Add => BlendOpWire::Add,
            W::Subtract => BlendOpWire::Subtract,
            W::ReverseSubtract => BlendOpWire::ReverseSubtract,
            W::Min => BlendOpWire::Min,
            W::Max => BlendOpWire::Max,
        }
    }};
}

#[cfg(target_os = "linux")]
fn blend_factor_from_wire_src_color(
    f: EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor,
) -> BlendFactorWire {
    blend_factor_match!(EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor, f)
}
#[cfg(target_os = "linux")]
fn blend_factor_from_wire_dst_color(
    f: EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor,
) -> BlendFactorWire {
    blend_factor_match!(EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor, f)
}
#[cfg(target_os = "linux")]
fn blend_factor_from_wire_src_alpha(
    f: EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor,
) -> BlendFactorWire {
    blend_factor_match!(EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor, f)
}
#[cfg(target_os = "linux")]
fn blend_factor_from_wire_dst_alpha(
    f: EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor,
) -> BlendFactorWire {
    blend_factor_match!(EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor, f)
}
#[cfg(target_os = "linux")]
fn blend_op_from_wire_color(
    o: EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp,
) -> BlendOpWire {
    blend_op_match!(EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp, o)
}
#[cfg(target_os = "linux")]
fn blend_op_from_wire_alpha(
    o: EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp,
) -> BlendOpWire {
    blend_op_match!(EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp, o)
}

/// Decode lowercase hex into bytes, returning a clean error message on
/// any malformed character or odd-length input. Empty string decodes to
/// an empty Vec — the caller validates push-constant size separately
/// against the kernel's declaration.
fn decode_hex(s: &str) -> std::result::Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err(format!(
            "expected even-length hex string, got {} characters",
            s.len()
        ));
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let nibble = |b: u8| -> std::result::Result<u8, String> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            b'A'..=b'F' => Ok(b - b'A' + 10),
            _ => Err(format!(
                "non-hex character {:?} at byte position",
                b as char
            )),
        }
    };
    for pair in bytes.chunks_exact(2) {
        out.push((nibble(pair[0])? << 4) | nibble(pair[1])?);
    }
    Ok(out)
}

/// Best-effort surface-share service release paired with registry eviction on Linux.
///
/// The registry drop alone releases the host's strong refcount on the
/// underlying resource, but the surface-share service still holds a dup of the DMA-BUF FD
/// until we explicitly call `release`. Errors here are logged, not returned —
/// the subprocess is not waiting on the surface-share service handshake at this point.
#[allow(unused_variables)]
fn release_surface_share_surface(sandbox: &GpuContextLimitedAccess, handle_id: &str) {
    #[cfg(target_os = "linux")]
    {
        if let Some(store) = sandbox.surface_store() {
            if let Err(e) = store.release(handle_id) {
                tracing::debug!(
                    "[escalate] surface-share service release for '{}' returned error: {}",
                    handle_id,
                    e
                );
            }
        }
    }
}

/// Wrap an [`EscalateResponse`] in the outer `{ rpc, payload… }` envelope the
/// bridge reader writes to the subprocess stdin.
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

/// Parse a wire-format pixel-format string into a [`PixelFormat`] enum.
///
/// The wire format uses lowercase snake-case names (`bgra32`,
/// `nv12_video_range`, etc.) so Python / Deno callers don't have to know
/// FourCC codes. Also accepts the mnemonic `"bgra"` for
/// [`PixelFormat::Bgra32`], matching the existing
/// `NativeGpu.acquire_surface(format="bgra")` default on the Python side.
fn parse_pixel_format(s: &str) -> std::result::Result<PixelFormat, String> {
    let normalized = s.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "bgra" | "bgra32" => Ok(PixelFormat::Bgra32),
        "rgba" | "rgba32" => Ok(PixelFormat::Rgba32),
        "argb" | "argb32" => Ok(PixelFormat::Argb32),
        "rgba64" => Ok(PixelFormat::Rgba64),
        "nv12" | "nv12_video_range" => Ok(PixelFormat::Nv12VideoRange),
        "nv12_full_range" => Ok(PixelFormat::Nv12FullRange),
        "uyvy" | "uyvy422" => Ok(PixelFormat::Uyvy422),
        "yuyv" | "yuyv422" => Ok(PixelFormat::Yuyv422),
        "gray" | "gray8" => Ok(PixelFormat::Gray8),
        other => Err(format!("unknown pixel format '{other}'")),
    }
}

fn pixel_format_to_wire(fmt: PixelFormat) -> &'static str {
    match fmt {
        PixelFormat::Bgra32 => "bgra32",
        PixelFormat::Rgba32 => "rgba32",
        PixelFormat::Argb32 => "argb32",
        PixelFormat::Rgba64 => "rgba64",
        PixelFormat::Nv12VideoRange => "nv12_video_range",
        PixelFormat::Nv12FullRange => "nv12_full_range",
        PixelFormat::Uyvy422 => "uyvy422",
        PixelFormat::Yuyv422 => "yuyv422",
        PixelFormat::Gray8 => "gray8",
        PixelFormat::Unknown => "unknown",
    }
}

/// Parse a wire-format texture format string into a [`TextureFormat`].
///
/// Lowercase snake-case matches the variant name. Kept separate from
/// [`parse_pixel_format`] so the vocabularies can evolve independently —
/// pixel formats include video-specific YUV variants that textures don't
/// expose, and texture formats include float variants that pixel buffers
/// don't.
fn parse_texture_format(s: &str) -> std::result::Result<TextureFormat, String> {
    let normalized = s.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "rgba8_unorm" => Ok(TextureFormat::Rgba8Unorm),
        "rgba8_unorm_srgb" => Ok(TextureFormat::Rgba8UnormSrgb),
        "bgra8_unorm" => Ok(TextureFormat::Bgra8Unorm),
        "bgra8_unorm_srgb" => Ok(TextureFormat::Bgra8UnormSrgb),
        "rgba16_float" => Ok(TextureFormat::Rgba16Float),
        "rgba32_float" => Ok(TextureFormat::Rgba32Float),
        "nv12" => Ok(TextureFormat::Nv12),
        other => Err(format!("unknown texture format '{other}'")),
    }
}

fn texture_format_to_wire(fmt: TextureFormat) -> &'static str {
    match fmt {
        TextureFormat::Rgba8Unorm => "rgba8_unorm",
        TextureFormat::Rgba8UnormSrgb => "rgba8_unorm_srgb",
        TextureFormat::Bgra8Unorm => "bgra8_unorm",
        TextureFormat::Bgra8UnormSrgb => "bgra8_unorm_srgb",
        TextureFormat::Rgba16Float => "rgba16_float",
        TextureFormat::Rgba32Float => "rgba32_float",
        TextureFormat::Nv12 => "nv12",
    }
}

/// Parse an array of usage tokens into a combined [`TextureUsages`] bitmask.
///
/// An empty list is rejected — a texture must have at least one usage or the
/// RHI can't create it. Unknown tokens surface as an error so typos fail
/// loudly on the wire rather than silently dropping flags.
fn parse_texture_usages(tokens: &[String]) -> std::result::Result<TextureUsages, String> {
    if tokens.is_empty() {
        return Err("texture usage list must not be empty".to_string());
    }
    let mut out = TextureUsages::NONE;
    for token in tokens {
        let normalized = token.trim().to_ascii_lowercase();
        let flag = match normalized.as_str() {
            "copy_src" => TextureUsages::COPY_SRC,
            "copy_dst" => TextureUsages::COPY_DST,
            "texture_binding" => TextureUsages::TEXTURE_BINDING,
            "storage_binding" => TextureUsages::STORAGE_BINDING,
            "render_attachment" => TextureUsages::RENDER_ATTACHMENT,
            other => return Err(format!("unknown texture usage '{other}'")),
        };
        out |= flag;
    }
    Ok(out)
}

fn texture_usages_to_wire(usage: TextureUsages) -> Vec<String> {
    let mut out = Vec::new();
    if usage.contains(TextureUsages::COPY_SRC) {
        out.push("copy_src".to_string());
    }
    if usage.contains(TextureUsages::COPY_DST) {
        out.push("copy_dst".to_string());
    }
    if usage.contains(TextureUsages::TEXTURE_BINDING) {
        out.push("texture_binding".to_string());
    }
    if usage.contains(TextureUsages::STORAGE_BINDING) {
        out.push("storage_binding".to_string());
    }
    if usage.contains(TextureUsages::RENDER_ATTACHMENT) {
        out.push("render_attachment".to_string());
    }
    out
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
    value: &serde_json::Value,
) -> Option<serde_json::Value> {
    let parsed = try_parse_escalate_request(value)?;
    let response = match parsed {
        // Fire-and-forget ops (log) return `None` from the handler — no
        // reply is written back to the subprocess.
        Ok(op) => handle_escalate_op(sandbox, registry, op)?,
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
        .ok_or_else(|| StreamError::Runtime("not an escalate_request".to_string()))?
        .map_err(|e| StreamError::Runtime(e.message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pixel_format_accepts_common_aliases() {
        assert_eq!(parse_pixel_format("bgra"), Ok(PixelFormat::Bgra32));
        assert_eq!(parse_pixel_format("BGRA32"), Ok(PixelFormat::Bgra32));
        assert_eq!(parse_pixel_format("nv12"), Ok(PixelFormat::Nv12VideoRange));
        assert_eq!(
            parse_pixel_format("nv12_full_range"),
            Ok(PixelFormat::Nv12FullRange)
        );
        assert_eq!(parse_pixel_format("gray8"), Ok(PixelFormat::Gray8));
    }

    #[test]
    fn parse_pixel_format_rejects_unknown() {
        assert!(parse_pixel_format("xyz").is_err());
    }

    #[test]
    fn decode_hex_round_trips_lowercase_and_mixed_case() {
        assert_eq!(decode_hex("").unwrap(), Vec::<u8>::new());
        assert_eq!(decode_hex("00").unwrap(), vec![0u8]);
        assert_eq!(decode_hex("ff").unwrap(), vec![0xff]);
        assert_eq!(decode_hex("DeAdBeEf").unwrap(), vec![0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(
            decode_hex("0123456789abcdef").unwrap(),
            vec![0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]
        );
    }

    #[test]
    fn decode_hex_rejects_odd_length() {
        let err = decode_hex("abc").err().expect("expected odd-length error");
        assert!(err.contains("even-length"), "got: {err}");
    }

    #[test]
    fn decode_hex_rejects_non_hex_character() {
        let err = decode_hex("abxy").err().expect("expected non-hex error");
        assert!(err.contains("non-hex"), "got: {err}");
    }

    #[test]
    fn try_parse_rejects_lifecycle_traffic() {
        let lifecycle = serde_json::json!({"rpc": "ready"});
        assert!(try_parse_escalate_request(&lifecycle).is_none());
    }

    #[test]
    fn try_parse_accepts_acquire_pixel_buffer() {
        let msg = serde_json::json!({
            "rpc": "escalate_request",
            "op": "acquire_pixel_buffer",
            "request_id": "r-1",
            "width": 640,
            "height": 480,
            "format": "bgra",
        });
        let op = parse_op_for_tests(&msg).expect("decodes");
        match op {
            EscalateRequest::AcquirePixelBuffer(p) => {
                assert_eq!(p.request_id, "r-1");
                assert_eq!(p.width, 640);
                assert_eq!(p.height, 480);
                assert_eq!(p.format, "bgra");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn try_parse_accepts_release_handle() {
        let msg = serde_json::json!({
            "rpc": "escalate_request",
            "op": "release_handle",
            "request_id": "r-2",
            "handle_id": "h-abc",
        });
        let op = parse_op_for_tests(&msg).expect("decodes");
        match op {
            EscalateRequest::ReleaseHandle(p) => {
                assert_eq!(p.request_id, "r-2");
                assert_eq!(p.handle_id, "h-abc");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn try_parse_surfaces_error_with_request_id() {
        let msg = serde_json::json!({
            "rpc": "escalate_request",
            "op": "acquire_pixel_buffer",
            "request_id": "r-3",
            // missing width / height / format
        });
        let parsed = try_parse_escalate_request(&msg).expect("escalate-shaped");
        let err = parsed.expect_err("missing fields");
        assert_eq!(err.request_id.as_deref(), Some("r-3"));
        assert!(err.message.contains("failed to decode"));
    }

    #[test]
    fn log_frame_parses_as_escalate_request_log_variant() {
        // Parser-shape assertion: the wire-format `log` frame must carry
        // `rpc == "escalate_request"` and decode as `EscalateRequest::Log`.
        // This locks the JTD discriminator tag — the actual "bridge does not
        // forward log frames to the lifecycle channel" contract is locked by
        // `subprocess_bridge::tests::log_frame_does_not_leak_to_lifecycle_channel`,
        // which drives a real reader_loop over a socketpair.
        let log_frame = serde_json::json!({
            "rpc": "escalate_request",
            "op": "log",
            "source": "python",
            "source_seq": "1",
            "source_ts": "1970-01-01T00:00:00Z",
            "level": "info",
            "message": "hello from subprocess",
            "intercepted": false,
            "channel": serde_json::Value::Null,
            "pipeline_id": serde_json::Value::Null,
            "processor_id": "p-1",
            "attrs": {},
        });
        assert_eq!(
            log_frame.get("rpc").and_then(|v| v.as_str()),
            Some(ESCALATE_REQUEST_RPC),
            "log frames must carry the escalate-request rpc tag"
        );
        let parsed = match try_parse_escalate_request(&log_frame).expect("escalate-shaped") {
            Ok(op) => op,
            Err(e) => panic!("log frame must decode: {}", e.message),
        };
        assert!(matches!(parsed, EscalateRequest::Log(_)));
    }

    #[test]
    fn envelope_response_tags_rpc() {
        let resp = EscalateResponse::Ok(EscalateResponseOk {
            request_id: "r-1".into(),
            handle_id: "h-1".into(),
            width: Some(16),
            height: Some(16),
            format: Some("bgra32".into()),
            usage: None,
            timeline_value: None,
        });
        let env = envelope_response(resp);
        assert_eq!(
            env.get("rpc").and_then(|v| v.as_str()),
            Some("escalate_response")
        );
        assert_eq!(env.get("result").and_then(|v| v.as_str()), Some("ok"));
        assert_eq!(env.get("width").and_then(|v| v.as_u64()), Some(16));
    }

    #[test]
    fn release_handle_flags_unknown_handle() {
        // Registry-level release of an unknown handle. A full
        // integration test that exercises [`handle_escalate_op`]
        // against a real `GpuContextLimitedAccess` lives in the
        // `handle_escalate_op_end_to_end` test below — it is gated
        // on [`GpuContext::init_for_platform`] succeeding so CI
        // machines without a GPU still build+run the rest of the
        // suite.
        let registry = EscalateHandleRegistry::new();
        assert_eq!(registry.handle_count(), 0);
        assert!(!registry.remove_handle("missing"));
    }

    #[test]
    fn parse_texture_format_roundtrips_known_variants() {
        assert_eq!(
            parse_texture_format("bgra8_unorm"),
            Ok(TextureFormat::Bgra8Unorm)
        );
        assert_eq!(
            parse_texture_format("RGBA16_FLOAT"),
            Ok(TextureFormat::Rgba16Float)
        );
        assert_eq!(parse_texture_format("nv12"), Ok(TextureFormat::Nv12));
        assert!(parse_texture_format("xyz").is_err());
    }

    #[test]
    fn parse_texture_usages_combines_tokens() {
        let usage = parse_texture_usages(&[
            "texture_binding".to_string(),
            "copy_src".to_string(),
        ])
        .expect("known tokens");
        assert!(usage.contains(TextureUsages::TEXTURE_BINDING));
        assert!(usage.contains(TextureUsages::COPY_SRC));
        assert!(!usage.contains(TextureUsages::STORAGE_BINDING));
    }

    #[test]
    fn parse_texture_usages_rejects_empty_and_unknown() {
        assert!(parse_texture_usages(&[]).is_err());
        assert!(parse_texture_usages(&["bogus".to_string()]).is_err());
    }

    #[test]
    fn texture_usages_to_wire_is_stable_order() {
        let usage = TextureUsages::STORAGE_BINDING
            | TextureUsages::COPY_SRC
            | TextureUsages::TEXTURE_BINDING;
        assert_eq!(
            texture_usages_to_wire(usage),
            vec![
                "copy_src".to_string(),
                "texture_binding".to_string(),
                "storage_binding".to_string()
            ]
        );
    }

    /// `RunCpuReadbackCopy` with no bridge registered must surface a
    /// clean error response (not a panic) so the subprocess can
    /// translate it into a Python/Deno exception.
    #[cfg(target_os = "linux")]
    #[test]
    fn run_cpu_readback_copy_without_bridge_returns_err() {
        use crate::core::context::{GpuContext, GpuContextLimitedAccess};

        let gpu = match GpuContext::init_for_platform_sync() {
            Ok(g) => g,
            Err(_) => {
                println!(
                    "run_cpu_readback_copy_without_bridge_returns_err: no GPU device — skipping"
                );
                return;
            }
        };
        let sandbox = GpuContextLimitedAccess::new(gpu);
        let registry = EscalateHandleRegistry::new();

        let req = EscalateRequest::RunCpuReadbackCopy(EscalateRequestRunCpuReadbackCopy {
            request_id: "req-cpu-1".to_string(),
            surface_id: "42".to_string(),
            direction: EscalateRequestRunCpuReadbackCopyDirection::ImageToBuffer,
        });
        let response = handle_escalate_op(&sandbox, &registry, req)
            .expect("run_cpu_readback_copy always produces a response");
        match response {
            EscalateResponse::Err(err) => {
                assert_eq!(err.request_id, "req-cpu-1");
                assert!(
                    err.message.contains("CpuReadbackBridge"),
                    "expected error to mention bridge, got: {}",
                    err.message
                );
            }
            EscalateResponse::Ok(_) => {
                panic!("run_cpu_readback_copy must fail when no bridge is registered")
            }
            EscalateResponse::Contended(_) => {
                panic!("blocking run_cpu_readback_copy must never return Contended")
            }
        }
        assert_eq!(
            registry.handle_count(),
            0,
            "no handle should be registered on the failure path"
        );
    }

    /// `TryRunCpuReadbackCopy` parse / no-bridge / contended dispatch
    /// path.
    #[cfg(target_os = "linux")]
    mod try_run_cpu_readback_copy_dispatch {
        use super::super::*;
        use super::EscalateHandleRegistry;
        use std::sync::Arc;

        use crate::core::context::{
            CpuReadbackBridge, CpuReadbackCopyDirection, GpuContext, GpuContextLimitedAccess,
        };
        use streamlib_adapter_abi::SurfaceId;

        struct AlwaysContendedBridge;
        impl CpuReadbackBridge for AlwaysContendedBridge {
            fn run_copy(
                &self,
                _surface_id: SurfaceId,
                _direction: CpuReadbackCopyDirection,
            ) -> std::result::Result<u64, String> {
                Err("AlwaysContendedBridge does not implement blocking run_copy".to_string())
            }
            fn try_run_copy(
                &self,
                _surface_id: SurfaceId,
                _direction: CpuReadbackCopyDirection,
            ) -> std::result::Result<Option<u64>, String> {
                Ok(None)
            }
        }

        struct AlwaysErrBridge;
        impl CpuReadbackBridge for AlwaysErrBridge {
            fn run_copy(
                &self,
                _surface_id: SurfaceId,
                _direction: CpuReadbackCopyDirection,
            ) -> std::result::Result<u64, String> {
                Err("blocking path not exercised in this test".into())
            }
            fn try_run_copy(
                &self,
                _surface_id: SurfaceId,
                _direction: CpuReadbackCopyDirection,
            ) -> std::result::Result<Option<u64>, String> {
                Err("synthetic adapter failure for test".into())
            }
        }

        fn make_sandbox_with_bridge(
            bridge: Option<Arc<dyn CpuReadbackBridge>>,
        ) -> Option<GpuContextLimitedAccess> {
            let gpu = match GpuContext::init_for_platform_sync() {
                Ok(g) => g,
                Err(_) => return None,
            };
            if let Some(b) = bridge {
                gpu.set_cpu_readback_bridge(b);
            }
            Some(GpuContextLimitedAccess::new(gpu))
        }

        /// Bridge `Ok(None)` → `EscalateResponse::Contended`.
        #[test]
        fn contended_response_when_bridge_returns_none() {
            let Some(sandbox) = make_sandbox_with_bridge(Some(Arc::new(AlwaysContendedBridge)))
            else {
                println!("contended_response_when_bridge_returns_none: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();

            let req = EscalateRequest::TryRunCpuReadbackCopy(EscalateRequestTryRunCpuReadbackCopy {
                request_id: "req-try-contended".into(),
                surface_id: "1".into(),
                direction: EscalateRequestTryRunCpuReadbackCopyDirection::ImageToBuffer,
            });
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("try_run_cpu_readback_copy always produces a response");
            match response {
                EscalateResponse::Contended(c) => {
                    assert_eq!(c.request_id, "req-try-contended");
                }
                other => panic!("expected Contended response, got {other:?}"),
            }
            assert_eq!(registry.handle_count(), 0);
        }

        /// Bridge `Err(_)` → `EscalateResponse::Err`, NOT `Contended`.
        #[test]
        fn err_response_when_bridge_returns_err() {
            let Some(sandbox) = make_sandbox_with_bridge(Some(Arc::new(AlwaysErrBridge))) else {
                println!("err_response_when_bridge_returns_err: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();

            let req = EscalateRequest::TryRunCpuReadbackCopy(EscalateRequestTryRunCpuReadbackCopy {
                request_id: "req-try-err".into(),
                surface_id: "1".into(),
                direction: EscalateRequestTryRunCpuReadbackCopyDirection::ImageToBuffer,
            });
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("try_run_cpu_readback_copy always produces a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-try-err");
                    assert!(
                        err.message.contains("synthetic adapter failure"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err response, got {other:?}"),
            }
        }

        /// `try_run_cpu_readback_copy` with no bridge installed
        /// surfaces the same Configuration error shape as the blocking
        /// variant.
        #[test]
        fn err_when_no_bridge_registered() {
            let Some(sandbox) = make_sandbox_with_bridge(None) else {
                println!("err_when_no_bridge_registered: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();

            let req = EscalateRequest::TryRunCpuReadbackCopy(EscalateRequestTryRunCpuReadbackCopy {
                request_id: "req-try-no-bridge".into(),
                surface_id: "1".into(),
                direction: EscalateRequestTryRunCpuReadbackCopyDirection::ImageToBuffer,
            });
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("try_run_cpu_readback_copy always produces a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-try-no-bridge");
                    assert!(err.message.contains("CpuReadbackBridge"), "got: {}", err.message);
                }
                other => panic!("expected Err response, got {other:?}"),
            }
        }

        /// Malformed `surface_id` must report a parse error.
        #[test]
        fn err_when_surface_id_malformed() {
            let Some(sandbox) = make_sandbox_with_bridge(Some(Arc::new(AlwaysContendedBridge)))
            else {
                println!("err_when_surface_id_malformed: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();

            let req = EscalateRequest::TryRunCpuReadbackCopy(EscalateRequestTryRunCpuReadbackCopy {
                request_id: "req-try-bad-id".into(),
                surface_id: "abc".into(),
                direction: EscalateRequestTryRunCpuReadbackCopyDirection::ImageToBuffer,
            });
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("try_run_cpu_readback_copy always produces a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-try-bad-id");
                    assert!(
                        err.message.contains("not a u64") || err.message.contains("invalid"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err response, got {other:?}"),
            }
        }
    }

    /// Compute-kernel handler tests: cover the IPC dispatch paths
    /// `register_compute_kernel` / `run_compute_kernel` end-to-end
    /// (bridge missing, invalid hex, kernel_id stability, unregistered
    /// kernel_id). The bridge implementation lives in application setup
    /// glue (`examples/polyglot-vulkan-compute/src/main.rs`); these
    /// tests use a synthetic in-test bridge so the contract holds even
    /// when no GPU is available.
    #[cfg(target_os = "linux")]
    mod compute_kernel_dispatch {
        use super::super::*;
        use super::EscalateHandleRegistry;
        use std::sync::{Arc, Mutex};

        use crate::core::context::{
            ComputeKernelBridge, GpuContext, GpuContextLimitedAccess,
        };

        /// Synthetic bridge: returns SHA-256(spv) hex on register and
        /// records the (kernel_id, surface_uuid, push, dispatch) tuple
        /// per `run` call so the test can assert the wire shape.
        struct RecordingBridge {
            registered: Mutex<std::collections::HashMap<String, u32>>,
            runs: Mutex<Vec<RecordedRun>>,
        }

        #[derive(Clone, Debug)]
        struct RecordedRun {
            kernel_id: String,
            surface_uuid: String,
            push_len: usize,
            groups: (u32, u32, u32),
        }

        impl RecordingBridge {
            fn new() -> Arc<Self> {
                Arc::new(Self {
                    registered: Mutex::new(std::collections::HashMap::new()),
                    runs: Mutex::new(Vec::new()),
                })
            }

            fn registered_count(&self) -> usize {
                self.registered.lock().unwrap().len()
            }

            fn runs(&self) -> Vec<RecordedRun> {
                self.runs.lock().unwrap().clone()
            }
        }

        impl ComputeKernelBridge for RecordingBridge {
            fn register(
                &self,
                spv: &[u8],
                push_constant_size: u32,
            ) -> std::result::Result<String, String> {
                use sha2::{Digest, Sha256};
                let mut h = Sha256::new();
                h.update(spv);
                let id = format!("{:x}", h.finalize());
                self.registered
                    .lock()
                    .unwrap()
                    .insert(id.clone(), push_constant_size);
                Ok(id)
            }

            fn run(
                &self,
                kernel_id: &str,
                surface_uuid: &str,
                push_constants: &[u8],
                group_count_x: u32,
                group_count_y: u32,
                group_count_z: u32,
            ) -> std::result::Result<(), String> {
                if !self.registered.lock().unwrap().contains_key(kernel_id) {
                    return Err(format!(
                        "kernel_id '{kernel_id}' not registered with this bridge"
                    ));
                }
                self.runs.lock().unwrap().push(RecordedRun {
                    kernel_id: kernel_id.to_string(),
                    surface_uuid: surface_uuid.to_string(),
                    push_len: push_constants.len(),
                    groups: (group_count_x, group_count_y, group_count_z),
                });
                Ok(())
            }
        }

        fn make_sandbox_with_bridge(
            bridge: Option<Arc<dyn ComputeKernelBridge>>,
        ) -> Option<GpuContextLimitedAccess> {
            let gpu = match GpuContext::init_for_platform_sync() {
                Ok(g) => g,
                Err(_) => return None,
            };
            if let Some(b) = bridge {
                gpu.set_compute_kernel_bridge(b);
            }
            Some(GpuContextLimitedAccess::new(gpu))
        }

        /// `register_compute_kernel` with no bridge registered must
        /// surface a typed `Err`, not a panic. Mirrors
        /// `run_cpu_readback_copy_without_bridge_returns_err`.
        #[test]
        fn register_without_bridge_returns_err() {
            let sandbox = match make_sandbox_with_bridge(None) {
                Some(s) => s,
                None => {
                    println!("register_without_bridge_returns_err: no GPU — skipping");
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RegisterComputeKernel(
                EscalateRequestRegisterComputeKernel {
                    request_id: "req-reg-1".to_string(),
                    spv_hex: "deadbeef".to_string(),
                    push_constant_size: 0,
                },
            );
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-reg-1");
                    assert!(
                        err.message.contains("ComputeKernelBridge"),
                        "expected bridge-not-registered error, got: {}",
                        err.message
                    );
                }
                other => panic!(
                    "expected Err when no bridge registered, got {other:?}"
                ),
            }
        }

        /// `run_compute_kernel` with no bridge registered must surface a
        /// typed `Err`, not a panic.
        #[test]
        fn run_without_bridge_returns_err() {
            let sandbox = match make_sandbox_with_bridge(None) {
                Some(s) => s,
                None => {
                    println!("run_without_bridge_returns_err: no GPU — skipping");
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RunComputeKernel(
                EscalateRequestRunComputeKernel {
                    request_id: "req-run-1".to_string(),
                    kernel_id: "abc".to_string(),
                    surface_uuid: "uuid-1".to_string(),
                    push_constants_hex: "".to_string(),
                    group_count_x: 1,
                    group_count_y: 1,
                    group_count_z: 1,
                },
            );
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-run-1");
                    assert!(
                        err.message.contains("ComputeKernelBridge"),
                        "expected bridge-not-registered error, got: {}",
                        err.message
                    );
                }
                other => panic!(
                    "expected Err when no bridge registered, got {other:?}"
                ),
            }
        }

        /// Malformed `spv_hex` must surface as a clean parse error
        /// keyed by `request_id`, not a panic.
        #[test]
        fn register_with_invalid_hex_returns_err() {
            let bridge = RecordingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!("register_with_invalid_hex_returns_err: no GPU — skipping");
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RegisterComputeKernel(
                EscalateRequestRegisterComputeKernel {
                    request_id: "req-reg-bad".to_string(),
                    spv_hex: "xyz123".to_string(), // non-hex character
                    push_constant_size: 0,
                },
            );
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-reg-bad");
                    assert!(
                        err.message.contains("spv_hex"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err for malformed hex, got {other:?}"),
            }
            assert_eq!(
                bridge.registered_count(),
                0,
                "bridge.register must not have been called on the parse-error path"
            );
        }

        /// Malformed `push_constants_hex` must surface as a clean parse
        /// error keyed by `request_id`.
        #[test]
        fn run_with_invalid_push_constants_hex_returns_err() {
            let bridge = RecordingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "run_with_invalid_push_constants_hex_returns_err: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RunComputeKernel(
                EscalateRequestRunComputeKernel {
                    request_id: "req-run-bad".to_string(),
                    kernel_id: "abc".to_string(),
                    surface_uuid: "uuid-1".to_string(),
                    push_constants_hex: "xyz".to_string(), // odd-length + non-hex
                    group_count_x: 1,
                    group_count_y: 1,
                    group_count_z: 1,
                },
            );
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-run-bad");
                    assert!(
                        err.message.contains("push_constants_hex"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err for malformed hex, got {other:?}"),
            }
            assert!(
                bridge.runs().is_empty(),
                "bridge.run must not have been called on the parse-error path"
            );
        }

        /// Registering identical SPIR-V twice must return the same
        /// `kernel_id`. Reflects the issue body's "kernel_id stability"
        /// requirement and the host-side cache-hit semantics.
        #[test]
        fn register_returns_stable_kernel_id_for_identical_spirv() {
            let bridge = RecordingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "register_returns_stable_kernel_id_for_identical_spirv: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let make_req = |rid: &str| {
                EscalateRequest::RegisterComputeKernel(
                    EscalateRequestRegisterComputeKernel {
                        request_id: rid.to_string(),
                        spv_hex: "deadbeefcafebabe".to_string(),
                        push_constant_size: 0,
                    },
                )
            };
            let first = handle_escalate_op(&sandbox, &registry, make_req("r1"))
                .expect("first register must produce a response");
            let second = handle_escalate_op(&sandbox, &registry, make_req("r2"))
                .expect("second register must produce a response");
            let id1 = match first {
                EscalateResponse::Ok(ok) => {
                    assert_eq!(ok.request_id, "r1");
                    ok.handle_id
                }
                other => panic!("first register expected Ok, got {other:?}"),
            };
            let id2 = match second {
                EscalateResponse::Ok(ok) => {
                    assert_eq!(ok.request_id, "r2");
                    ok.handle_id
                }
                other => panic!("second register expected Ok, got {other:?}"),
            };
            assert_eq!(
                id1, id2,
                "identical SPIR-V must produce the same kernel_id"
            );
        }

        /// Different SPIR-V must produce different `kernel_id`s.
        #[test]
        fn register_returns_distinct_kernel_ids_for_different_spirv() {
            let bridge = RecordingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "register_returns_distinct_kernel_ids_for_different_spirv: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req_a = EscalateRequest::RegisterComputeKernel(
                EscalateRequestRegisterComputeKernel {
                    request_id: "ra".to_string(),
                    spv_hex: "deadbeef".to_string(),
                    push_constant_size: 0,
                },
            );
            let req_b = EscalateRequest::RegisterComputeKernel(
                EscalateRequestRegisterComputeKernel {
                    request_id: "rb".to_string(),
                    spv_hex: "cafebabe".to_string(),
                    push_constant_size: 0,
                },
            );
            let resp_a = match handle_escalate_op(&sandbox, &registry, req_a)
                .expect("must produce a response")
            {
                EscalateResponse::Ok(ok) => ok.handle_id,
                other => panic!("expected Ok, got {other:?}"),
            };
            let resp_b = match handle_escalate_op(&sandbox, &registry, req_b)
                .expect("must produce a response")
            {
                EscalateResponse::Ok(ok) => ok.handle_id,
                other => panic!("expected Ok, got {other:?}"),
            };
            assert_ne!(
                resp_a, resp_b,
                "different SPIR-V must produce different kernel_ids"
            );
        }

        /// Dispatching a kernel_id the bridge has never seen must
        /// surface as a typed `Err`, not a panic. Reflects the issue
        /// body's "Negative test: dispatch with an unregistered
        /// kernel_id returns a typed `EscalateError`, not a panic"
        /// requirement.
        #[test]
        fn run_with_unregistered_kernel_id_returns_err() {
            let bridge = RecordingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "run_with_unregistered_kernel_id_returns_err: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RunComputeKernel(
                EscalateRequestRunComputeKernel {
                    request_id: "req-run-bad-id".to_string(),
                    kernel_id: "never-registered-id".to_string(),
                    surface_uuid: "uuid-1".to_string(),
                    push_constants_hex: "".to_string(),
                    group_count_x: 1,
                    group_count_y: 1,
                    group_count_z: 1,
                },
            );
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-run-bad-id");
                    assert!(
                        err.message.contains("not registered")
                            || err.message.contains("never-registered-id"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!(
                    "expected Err for unregistered kernel_id, got {other:?}"
                ),
            }
        }

        /// Successful `run_compute_kernel` echoes the kernel_id back as
        /// `handle_id` (no separate handle is allocated per dispatch —
        /// compute is sync host-side) and forwards push-constants /
        /// dispatch dimensions to the bridge unchanged.
        #[test]
        fn run_forwards_payload_to_bridge_and_echoes_kernel_id() {
            let bridge = RecordingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "run_forwards_payload_to_bridge_and_echoes_kernel_id: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();

            // Register first so the bridge has the kernel_id cached.
            let reg = EscalateRequest::RegisterComputeKernel(
                EscalateRequestRegisterComputeKernel {
                    request_id: "reg".to_string(),
                    spv_hex: "abcdef0123456789".to_string(),
                    push_constant_size: 8,
                },
            );
            let kernel_id =
                match handle_escalate_op(&sandbox, &registry, reg).unwrap() {
                    EscalateResponse::Ok(ok) => ok.handle_id,
                    other => panic!("register expected Ok, got {other:?}"),
                };

            let run = EscalateRequest::RunComputeKernel(
                EscalateRequestRunComputeKernel {
                    request_id: "run".to_string(),
                    kernel_id: kernel_id.clone(),
                    surface_uuid: "surface-xyz".to_string(),
                    push_constants_hex: "00112233aabbccdd".to_string(),
                    group_count_x: 4,
                    group_count_y: 5,
                    group_count_z: 6,
                },
            );
            let response = handle_escalate_op(&sandbox, &registry, run).unwrap();
            match response {
                EscalateResponse::Ok(ok) => {
                    assert_eq!(ok.request_id, "run");
                    assert_eq!(
                        ok.handle_id, kernel_id,
                        "run response handle_id must echo the kernel_id"
                    );
                    assert!(
                        ok.timeline_value.is_none(),
                        "run_compute_kernel responses carry no timeline"
                    );
                }
                other => panic!("run expected Ok, got {other:?}"),
            }
            let runs = bridge.runs();
            assert_eq!(runs.len(), 1, "bridge.run must have been called once");
            let r = &runs[0];
            assert_eq!(r.kernel_id, kernel_id);
            assert_eq!(r.surface_uuid, "surface-xyz");
            assert_eq!(r.push_len, 8, "push_constants_hex decoded to 8 bytes");
            assert_eq!(r.groups, (4, 5, 6));
        }
    }

    /// Host-Rust unit tests for the `register_graphics_kernel` /
    /// `run_graphics_draw` escalate handlers. Mirrors the
    /// `compute_kernel_dispatch` shape — the synthetic
    /// `RecordingGraphicsBridge` keeps tests independent of a working
    /// VkDevice, so handler-shape regressions surface even on
    /// machines without a GPU.
    #[cfg(target_os = "linux")]
    mod graphics_kernel_dispatch {
        use super::super::*;
        use super::EscalateHandleRegistry;
        use std::sync::{Arc, Mutex};

        use crate::_generated_::com_streamlib_escalate_request::{
            EscalateRequestRegisterGraphicsKernelBinding,
            EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttribute,
            EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBinding,
            EscalateRequestRunGraphicsDrawBinding, EscalateRequestRunGraphicsDrawDraw,
            EscalateRequestRunGraphicsDrawIndexBuffer, EscalateRequestRunGraphicsDrawScissor,
            EscalateRequestRunGraphicsDrawVertexBuffer, EscalateRequestRunGraphicsDrawViewport,
        };
        use crate::core::context::{
            GpuContext, GpuContextLimitedAccess, GraphicsKernelBridge,
            GraphicsKernelRegisterDecl, GraphicsKernelRunDraw,
        };

        /// Synthetic bridge — registers any caller-provided vertex+fragment
        /// SPIR-V (no SPV reflection or pipeline build), keys the kernel id by
        /// SHA-256 over the canonicalized inputs so identical descriptors
        /// hit the cache, and records each `run_draw` for later assertion.
        struct RecordingGraphicsBridge {
            registered: Mutex<std::collections::HashMap<String, GraphicsKernelRegisterDecl>>,
            runs: Mutex<Vec<GraphicsKernelRunDraw>>,
        }

        impl RecordingGraphicsBridge {
            fn new() -> Arc<Self> {
                Arc::new(Self {
                    registered: Mutex::new(std::collections::HashMap::new()),
                    runs: Mutex::new(Vec::new()),
                })
            }

            fn registered_count(&self) -> usize {
                self.registered.lock().unwrap().len()
            }

            fn last_registered(&self) -> Option<GraphicsKernelRegisterDecl> {
                // The tests register at most one descriptor each so
                // returning a snapshot of the first entry is enough.
                self.registered.lock().unwrap().values().next().cloned()
            }

            fn runs(&self) -> Vec<GraphicsKernelRunDraw> {
                self.runs.lock().unwrap().clone()
            }

            fn key(decl: &GraphicsKernelRegisterDecl) -> String {
                use sha2::{Digest, Sha256};
                let mut h = Sha256::new();
                h.update(b"v=");
                h.update(&decl.vertex_spv);
                h.update(b"|f=");
                h.update(&decl.fragment_spv);
                h.update(b"|ve=");
                h.update(decl.vertex_entry_point.as_bytes());
                h.update(b"|fe=");
                h.update(decl.fragment_entry_point.as_bytes());
                h.update(b"|pcs=");
                h.update(&decl.push_constant_size.to_le_bytes());
                h.update(b"|pcst=");
                h.update(&decl.push_constant_stages.to_le_bytes());
                h.update(b"|dsi=");
                h.update(&decl.descriptor_sets_in_flight.to_le_bytes());
                h.update(b"|nb=");
                h.update(&(decl.bindings.len() as u32).to_le_bytes());
                format!("{:x}", h.finalize())
            }
        }

        impl GraphicsKernelBridge for RecordingGraphicsBridge {
            fn register(
                &self,
                decl: &GraphicsKernelRegisterDecl,
            ) -> std::result::Result<String, String> {
                let id = Self::key(decl);
                self.registered
                    .lock()
                    .unwrap()
                    .entry(id.clone())
                    .or_insert_with(|| decl.clone());
                Ok(id)
            }

            fn run_draw(
                &self,
                draw: &GraphicsKernelRunDraw,
            ) -> std::result::Result<(), String> {
                if !self.registered.lock().unwrap().contains_key(&draw.kernel_id) {
                    return Err(format!(
                        "kernel_id '{}' not registered with this bridge",
                        draw.kernel_id
                    ));
                }
                self.runs.lock().unwrap().push(draw.clone());
                Ok(())
            }
        }

        fn make_sandbox_with_bridge(
            bridge: Option<Arc<dyn GraphicsKernelBridge>>,
        ) -> Option<GpuContextLimitedAccess> {
            let gpu = match GpuContext::init_for_platform_sync() {
                Ok(g) => g,
                Err(_) => return None,
            };
            if let Some(b) = bridge {
                gpu.set_graphics_kernel_bridge(b);
            }
            Some(GpuContextLimitedAccess::new(gpu))
        }

        /// Build a baseline `register_graphics_kernel` request — vertex
        /// + fragment SPIR-V hex, default-shaped TriangleList pipeline
        /// state with no blending and no depth. Tests that need a
        /// specific shape mutate fields after calling.
        fn make_register_req(
            request_id: &str,
            vertex_hex: &str,
            fragment_hex: &str,
        ) -> EscalateRequestRegisterGraphicsKernel {
            EscalateRequestRegisterGraphicsKernel {
                request_id: request_id.to_string(),
                label: "test-graphics".to_string(),
                vertex_spv_hex: vertex_hex.to_string(),
                fragment_spv_hex: fragment_hex.to_string(),
                vertex_entry_point: "main".to_string(),
                fragment_entry_point: "main".to_string(),
                bindings: Vec::new(),
                push_constant_size: 0,
                push_constant_stages: 0,
                descriptor_sets_in_flight: 2,
                pipeline_state: EscalateRequestRegisterGraphicsKernelPipelineState {
                    topology: EscalateRequestRegisterGraphicsKernelPipelineStateTopology::TriangleList,
                    vertex_input_bindings: Vec::new(),
                    vertex_input_attributes: Vec::new(),
                    rasterization_polygon_mode:
                        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode::Fill,
                    rasterization_cull_mode:
                        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode::None,
                    rasterization_front_face:
                        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace::CounterClockwise,
                    rasterization_line_width: 1.0,
                    multisample_samples: 1,
                    depth_stencil_enabled: false,
                    depth_compare_op:
                        EscalateRequestRegisterGraphicsKernelPipelineStateDepthCompareOp::Always,
                    depth_write: false,
                    color_blend_enabled: false,
                    color_write_mask: 0b1111,
                    color_blend_src_color_factor:
                        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor::One,
                    color_blend_dst_color_factor:
                        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor::Zero,
                    color_blend_color_op:
                        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp::Add,
                    color_blend_src_alpha_factor:
                        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor::One,
                    color_blend_dst_alpha_factor:
                        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor::Zero,
                    color_blend_alpha_op:
                        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp::Add,
                    attachment_color_formats: vec!["rgba8_unorm".to_string()],
                    dynamic_state:
                        EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState::ViewportScissor,
                    attachment_depth_format: None,
                },
            }
        }

        /// Baseline `run_graphics_draw` request — vertex-fabricating
        /// (no vertex buffers, no index buffer), single color target,
        /// 320x240 extent, simple Draw of 3 vertices.
        fn make_run_req(
            request_id: &str,
            kernel_id: &str,
            surface_uuid: &str,
        ) -> EscalateRequestRunGraphicsDraw {
            EscalateRequestRunGraphicsDraw {
                request_id: request_id.to_string(),
                kernel_id: kernel_id.to_string(),
                frame_index: 0,
                bindings: Vec::new(),
                vertex_buffers: Vec::new(),
                color_target_uuids: vec![surface_uuid.to_string()],
                extent_width: 320,
                extent_height: 240,
                push_constants_hex: String::new(),
                draw: EscalateRequestRunGraphicsDrawDraw {
                    kind: EscalateRequestRunGraphicsDrawDrawKind::Draw,
                    vertex_count: 3,
                    index_count: 0,
                    instance_count: 1,
                    first_vertex: 0,
                    first_instance: 0,
                    first_index: 0,
                    vertex_offset: 0,
                },
                index_buffer: None,
                depth_target_uuid: None,
                viewport: None,
                scissor: None,
            }
        }

        #[test]
        fn register_without_bridge_returns_err() {
            let sandbox = match make_sandbox_with_bridge(None) {
                Some(s) => s,
                None => {
                    println!("register_without_bridge_returns_err: no GPU — skipping");
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RegisterGraphicsKernel(make_register_req(
                "req-reg-1", "deadbeef", "cafebabe",
            ));
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-reg-1");
                    assert!(
                        err.message.contains("GraphicsKernelBridge"),
                        "expected bridge-not-registered error, got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err when no bridge registered, got {other:?}"),
            }
        }

        #[test]
        fn run_without_bridge_returns_err() {
            let sandbox = match make_sandbox_with_bridge(None) {
                Some(s) => s,
                None => {
                    println!("run_without_bridge_returns_err: no GPU — skipping");
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RunGraphicsDraw(make_run_req(
                "req-run-1", "kernel-x", "surface-y",
            ));
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-run-1");
                    assert!(
                        err.message.contains("GraphicsKernelBridge"),
                        "expected bridge-not-registered error, got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err when no bridge registered, got {other:?}"),
            }
        }

        #[test]
        fn register_with_invalid_vertex_hex_returns_err() {
            let bridge = RecordingGraphicsBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!("register_with_invalid_vertex_hex_returns_err: no GPU — skipping");
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RegisterGraphicsKernel(make_register_req(
                "req-bad-v", "xyz123", "cafebabe",
            ));
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-bad-v");
                    assert!(
                        err.message.contains("vertex_spv_hex"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err for malformed vertex hex, got {other:?}"),
            }
            assert_eq!(
                bridge.registered_count(),
                0,
                "bridge.register must not have been called on the parse-error path"
            );
        }

        #[test]
        fn register_with_invalid_fragment_hex_returns_err() {
            let bridge = RecordingGraphicsBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!("register_with_invalid_fragment_hex_returns_err: no GPU — skipping");
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RegisterGraphicsKernel(make_register_req(
                "req-bad-f", "deadbeef", "qq",
            ));
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-bad-f");
                    assert!(
                        err.message.contains("fragment_spv_hex"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err for malformed fragment hex, got {other:?}"),
            }
            assert_eq!(bridge.registered_count(), 0);
        }

        #[test]
        fn run_with_invalid_push_constants_hex_returns_err() {
            let bridge = RecordingGraphicsBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "run_with_invalid_push_constants_hex_returns_err: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let mut req = make_run_req("req-bad-push", "kernel-x", "surface-y");
            req.push_constants_hex = "xyz".to_string();
            let response = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::RunGraphicsDraw(req),
            )
            .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-bad-push");
                    assert!(
                        err.message.contains("push_constants_hex"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err for malformed push hex, got {other:?}"),
            }
            assert!(bridge.runs().is_empty());
        }

        #[test]
        fn run_with_malformed_vertex_buffer_offset_returns_err() {
            let bridge = RecordingGraphicsBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "run_with_malformed_vertex_buffer_offset_returns_err: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let mut req = make_run_req("req-bad-vb", "kernel-x", "surface-y");
            req.vertex_buffers = vec![EscalateRequestRunGraphicsDrawVertexBuffer {
                binding: 0,
                surface_uuid: "vb-uuid".to_string(),
                offset: "not-a-number".to_string(),
            }];
            let response = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::RunGraphicsDraw(req),
            )
            .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-bad-vb");
                    assert!(
                        err.message.contains("vertex_buffer.offset"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err for malformed vb.offset, got {other:?}"),
            }
            assert!(bridge.runs().is_empty());
        }

        #[test]
        fn register_returns_stable_kernel_id_for_identical_descriptor() {
            let bridge = RecordingGraphicsBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "register_returns_stable_kernel_id_for_identical_descriptor: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let make_req = |rid: &str| {
                EscalateRequest::RegisterGraphicsKernel(make_register_req(
                    rid,
                    "deadbeefcafebabe",
                    "00112233445566778899aabbccddeeff",
                ))
            };
            let id1 = match handle_escalate_op(&sandbox, &registry, make_req("a")).unwrap() {
                EscalateResponse::Ok(ok) => ok.handle_id,
                other => panic!("first register expected Ok, got {other:?}"),
            };
            let id2 = match handle_escalate_op(&sandbox, &registry, make_req("b")).unwrap() {
                EscalateResponse::Ok(ok) => ok.handle_id,
                other => panic!("second register expected Ok, got {other:?}"),
            };
            assert_eq!(id1, id2, "identical descriptor must produce the same kernel_id");
        }

        #[test]
        fn register_returns_distinct_kernel_ids_for_different_spirv() {
            let bridge = RecordingGraphicsBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "register_returns_distinct_kernel_ids_for_different_spirv: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req_a = EscalateRequest::RegisterGraphicsKernel(make_register_req(
                "a", "deadbeef", "cafebabe",
            ));
            let req_b = EscalateRequest::RegisterGraphicsKernel(make_register_req(
                "b", "11223344", "cafebabe",
            ));
            let id_a = match handle_escalate_op(&sandbox, &registry, req_a).unwrap() {
                EscalateResponse::Ok(ok) => ok.handle_id,
                other => panic!("expected Ok, got {other:?}"),
            };
            let id_b = match handle_escalate_op(&sandbox, &registry, req_b).unwrap() {
                EscalateResponse::Ok(ok) => ok.handle_id,
                other => panic!("expected Ok, got {other:?}"),
            };
            assert_ne!(id_a, id_b, "different vertex SPIR-V must produce different kernel_ids");
        }

        /// Lock in the wire→domain pipeline-state translation. Mentally
        /// reverting any single arm of `graphics_pipeline_state_from_wire`
        /// (e.g. swapping `BlendOpWire::Add ↔ Subtract`) must fail this
        /// test — the synthetic `RecordingGraphicsBridge` accepts the
        /// translated `GraphicsPipelineStateWire` value but doesn't itself
        /// validate any arm, so without this test the ~200 lines of enum
        /// mapping in the handler would have no regression coverage.
        #[test]
        fn pipeline_state_translates_every_enum_arm() {
            use crate::core::context::{
                BlendFactorWire, BlendOpWire, CullModeWire, DepthCompareOpWire,
                DepthFormatWire, DynamicStateWire, FrontFaceWire, GraphicsBindingKindWire,
                PolygonModeWire, PrimitiveTopologyWire, VertexAttributeFormatWire,
                VertexInputRateWire,
            };

            let bridge = RecordingGraphicsBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "pipeline_state_translates_every_enum_arm: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();

            // Build a request that uses non-default values for every
            // pipeline-state arm we want to lock down. Each value is
            // chosen to be DIFFERENT from the matching default so a
            // wrong arm in the translation would land in the wrong
            // wire-mirror variant and the assertion would fail.
            let mut req = make_register_req(
                "req-translate", "deadbeef", "cafebabe",
            );
            req.bindings = vec![EscalateRequestRegisterGraphicsKernelBinding {
                binding: 7,
                kind: EscalateRequestRegisterGraphicsKernelBindingKind::UniformBuffer,
                stages: 3, // VERTEX | FRAGMENT
            }];
            req.pipeline_state.topology =
                EscalateRequestRegisterGraphicsKernelPipelineStateTopology::TriangleStrip;
            req.pipeline_state.vertex_input_bindings = vec![
                EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBinding {
                    binding: 2,
                    stride: 28,
                    input_rate:
                        EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBindingInputRate::Instance,
                },
            ];
            req.pipeline_state.vertex_input_attributes = vec![
                EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttribute {
                    location: 5,
                    binding: 2,
                    format:
                        EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttributeFormat::Rgb32Float,
                    offset: 12,
                },
            ];
            req.pipeline_state.rasterization_polygon_mode =
                EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode::Line;
            req.pipeline_state.rasterization_cull_mode =
                EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode::Back;
            req.pipeline_state.rasterization_front_face =
                EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace::Clockwise;
            req.pipeline_state.rasterization_line_width = 2.5;
            req.pipeline_state.depth_stencil_enabled = true;
            req.pipeline_state.depth_compare_op =
                EscalateRequestRegisterGraphicsKernelPipelineStateDepthCompareOp::LessOrEqual;
            req.pipeline_state.depth_write = true;
            req.pipeline_state.color_blend_enabled = true;
            req.pipeline_state.color_write_mask = 0b0101; // R | B only
            req.pipeline_state.color_blend_src_color_factor =
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor::SrcAlpha;
            req.pipeline_state.color_blend_dst_color_factor =
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor::OneMinusSrcAlpha;
            req.pipeline_state.color_blend_color_op =
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp::Subtract;
            req.pipeline_state.color_blend_src_alpha_factor =
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor::ConstantAlpha;
            req.pipeline_state.color_blend_dst_alpha_factor =
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor::OneMinusConstantAlpha;
            req.pipeline_state.color_blend_alpha_op =
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp::Max;
            req.pipeline_state.attachment_color_formats =
                vec!["bgra8_unorm_srgb".to_string()];
            req.pipeline_state.attachment_depth_format = Some(
                EscalateRequestRegisterGraphicsKernelPipelineStateAttachmentDepthFormat::D32Sfloat,
            );
            req.pipeline_state.dynamic_state =
                EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState::None;
            req.push_constant_size = 16;
            req.push_constant_stages = 3;
            req.descriptor_sets_in_flight = 4;

            let response = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::RegisterGraphicsKernel(req),
            )
            .expect("must produce a response");
            match response {
                EscalateResponse::Ok(ok) => assert_eq!(ok.request_id, "req-translate"),
                other => panic!("expected Ok, got {other:?}"),
            }

            let registered = bridge
                .last_registered()
                .expect("bridge should have stored the descriptor");

            // Top-level fields.
            assert_eq!(registered.label, "test-graphics");
            assert_eq!(registered.vertex_spv, vec![0xde, 0xad, 0xbe, 0xef]);
            assert_eq!(registered.fragment_spv, vec![0xca, 0xfe, 0xba, 0xbe]);
            assert_eq!(registered.push_constant_size, 16);
            assert_eq!(registered.push_constant_stages, 3);
            assert_eq!(registered.descriptor_sets_in_flight, 4);

            // Bindings translation.
            assert_eq!(registered.bindings.len(), 1);
            assert_eq!(registered.bindings[0].binding, 7);
            assert_eq!(
                registered.bindings[0].kind,
                GraphicsBindingKindWire::UniformBuffer
            );
            assert_eq!(registered.bindings[0].stages, 3);

            let p = &registered.pipeline_state;
            assert_eq!(p.topology, PrimitiveTopologyWire::TriangleStrip);
            assert_eq!(p.vertex_input_bindings.len(), 1);
            assert_eq!(p.vertex_input_bindings[0].binding, 2);
            assert_eq!(p.vertex_input_bindings[0].stride, 28);
            assert_eq!(
                p.vertex_input_bindings[0].input_rate,
                VertexInputRateWire::Instance
            );
            assert_eq!(p.vertex_input_attributes.len(), 1);
            assert_eq!(p.vertex_input_attributes[0].location, 5);
            assert_eq!(p.vertex_input_attributes[0].binding, 2);
            assert_eq!(
                p.vertex_input_attributes[0].format,
                VertexAttributeFormatWire::Rgb32Float
            );
            assert_eq!(p.vertex_input_attributes[0].offset, 12);
            assert_eq!(p.rasterization_polygon_mode, PolygonModeWire::Line);
            assert_eq!(p.rasterization_cull_mode, CullModeWire::Back);
            assert_eq!(p.rasterization_front_face, FrontFaceWire::Clockwise);
            assert_eq!(p.rasterization_line_width, 2.5);
            assert_eq!(p.multisample_samples, 1);
            assert!(p.depth_stencil_enabled);
            assert_eq!(p.depth_compare_op, DepthCompareOpWire::LessOrEqual);
            assert!(p.depth_write);
            assert!(p.color_blend_enabled);
            assert_eq!(p.color_write_mask, 0b0101);
            assert_eq!(p.color_blend_src_color_factor, BlendFactorWire::SrcAlpha);
            assert_eq!(
                p.color_blend_dst_color_factor,
                BlendFactorWire::OneMinusSrcAlpha
            );
            assert_eq!(p.color_blend_color_op, BlendOpWire::Subtract);
            assert_eq!(
                p.color_blend_src_alpha_factor,
                BlendFactorWire::ConstantAlpha
            );
            assert_eq!(
                p.color_blend_dst_alpha_factor,
                BlendFactorWire::OneMinusConstantAlpha
            );
            assert_eq!(p.color_blend_alpha_op, BlendOpWire::Max);
            assert_eq!(p.attachment_color_formats, vec!["bgra8_unorm_srgb"]);
            assert_eq!(p.attachment_depth_format, Some(DepthFormatWire::D32Sfloat));
            assert_eq!(p.dynamic_state, DynamicStateWire::None);
        }

        #[test]
        fn run_with_unregistered_kernel_id_returns_err() {
            let bridge = RecordingGraphicsBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "run_with_unregistered_kernel_id_returns_err: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RunGraphicsDraw(make_run_req(
                "req-bad-id",
                "never-registered",
                "surface-y",
            ));
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-bad-id");
                    assert!(
                        err.message.contains("not registered")
                            || err.message.contains("never-registered"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err for unregistered kernel_id, got {other:?}"),
            }
        }

        #[test]
        fn run_forwards_payload_to_bridge_and_echoes_kernel_id() {
            let bridge = RecordingGraphicsBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "run_forwards_payload_to_bridge_and_echoes_kernel_id: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();

            // Register first so the bridge has the kernel_id cached.
            let reg = EscalateRequest::RegisterGraphicsKernel(make_register_req(
                "reg",
                "abcdef0123456789",
                "fedcba9876543210",
            ));
            let kernel_id = match handle_escalate_op(&sandbox, &registry, reg).unwrap() {
                EscalateResponse::Ok(ok) => ok.handle_id,
                other => panic!("register expected Ok, got {other:?}"),
            };

            // Indexed draw with a vertex buffer + push constants — exercises
            // every translation arm in the wire→domain mapper.
            let mut run = make_run_req("run", &kernel_id, "color-target-uuid");
            run.frame_index = 1;
            run.bindings = vec![EscalateRequestRunGraphicsDrawBinding {
                binding: 0,
                kind: EscalateRequestRunGraphicsDrawBindingKind::SampledTexture,
                surface_uuid: "tex-uuid".to_string(),
            }];
            run.vertex_buffers = vec![EscalateRequestRunGraphicsDrawVertexBuffer {
                binding: 0,
                surface_uuid: "vb-uuid".to_string(),
                offset: "128".to_string(),
            }];
            run.index_buffer = Some(EscalateRequestRunGraphicsDrawIndexBuffer {
                surface_uuid: "ib-uuid".to_string(),
                offset: "64".to_string(),
                index_type: EscalateRequestRunGraphicsDrawIndexBufferIndexType::Uint32,
            });
            run.push_constants_hex = "00112233aabbccdd".to_string();
            run.draw = EscalateRequestRunGraphicsDrawDraw {
                kind: EscalateRequestRunGraphicsDrawDrawKind::DrawIndexed,
                vertex_count: 0,
                index_count: 6,
                instance_count: 2,
                first_vertex: 0,
                first_instance: 1,
                first_index: 3,
                vertex_offset: -4,
            };
            run.viewport = Some(EscalateRequestRunGraphicsDrawViewport {
                x: 0.0,
                y: 0.0,
                width: 320.0,
                height: 240.0,
                min_depth: 0.0,
                max_depth: 1.0,
            });
            run.scissor = Some(EscalateRequestRunGraphicsDrawScissor {
                x: 0,
                y: 0,
                width: 320,
                height: 240,
            });

            let response = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::RunGraphicsDraw(run),
            )
            .unwrap();
            match response {
                EscalateResponse::Ok(ok) => {
                    assert_eq!(ok.request_id, "run");
                    assert_eq!(
                        ok.handle_id, kernel_id,
                        "run response handle_id must echo the kernel_id"
                    );
                    assert!(
                        ok.timeline_value.is_none(),
                        "run_graphics_draw responses carry no timeline"
                    );
                }
                other => panic!("run expected Ok, got {other:?}"),
            }
            let runs = bridge.runs();
            assert_eq!(runs.len(), 1, "bridge.run_draw must have been called once");
            let r = &runs[0];
            assert_eq!(r.kernel_id, kernel_id);
            assert_eq!(r.frame_index, 1);
            assert_eq!(r.color_target_uuids, vec!["color-target-uuid".to_string()]);
            assert_eq!(r.extent, (320, 240));
            assert_eq!(r.bindings.len(), 1);
            assert_eq!(r.bindings[0].surface_uuid, "tex-uuid");
            assert_eq!(r.vertex_buffers.len(), 1);
            assert_eq!(r.vertex_buffers[0].surface_uuid, "vb-uuid");
            assert_eq!(r.vertex_buffers[0].offset, 128);
            let ib = r.index_buffer.as_ref().expect("index_buffer present");
            assert_eq!(ib.surface_uuid, "ib-uuid");
            assert_eq!(ib.offset, 64);
            assert_eq!(ib.index_type, IndexTypeWire::Uint32);
            assert_eq!(r.push_constants.len(), 8);
            assert!(r.viewport.is_some());
            assert!(r.scissor.is_some());
            match r.draw {
                GraphicsDrawSpec::DrawIndexed {
                    index_count,
                    instance_count,
                    first_index,
                    vertex_offset,
                    first_instance,
                } => {
                    assert_eq!(index_count, 6);
                    assert_eq!(instance_count, 2);
                    assert_eq!(first_index, 3);
                    assert_eq!(vertex_offset, -4);
                    assert_eq!(first_instance, 1);
                }
                other => panic!("expected DrawIndexed, got {other:?}"),
            }
        }
    }

    /// Tests for the ray-tracing-kernel + acceleration-structure
    /// escalate ops (issue #667).
    ///
    /// Mirrors the `graphics_kernel_dispatch` mod above: a synthetic
    /// `RecordingRayTracingBridge` keeps the tests independent of a
    /// working `VkDevice` (and an RT-capable GPU), so handler-shape
    /// regressions surface even on machines without a GPU.
    #[cfg(target_os = "linux")]
    mod ray_tracing_kernel_dispatch {
        use super::super::*;
        use super::EscalateHandleRegistry;
        use std::sync::{Arc, Mutex};

        use crate::_generated_::com_streamlib_escalate_request::{
            EscalateRequestRegisterAccelerationStructureTlasInstance,
            EscalateRequestRegisterRayTracingKernelBinding,
            EscalateRequestRegisterRayTracingKernelGroup,
            EscalateRequestRegisterRayTracingKernelStage,
            EscalateRequestRunRayTracingKernelBinding,
        };
        use crate::core::context::{
            BlasRegisterDecl, GpuContext, GpuContextLimitedAccess, RayTracingKernelBridge,
            RayTracingKernelRegisterDecl, RayTracingKernelRunDispatch, TlasRegisterDecl,
            RAY_TRACING_STAGE_INDEX_NONE,
        };

        /// Synthetic bridge — accepts any caller-provided BLAS/TLAS/kernel
        /// (no SPIR-V reflection or AS build), keys handles by SHA-256
        /// over the canonicalized inputs so identical descriptors hit
        /// the cache, and records every `run_kernel` for later assertion.
        struct RecordingRayTracingBridge {
            blases: Mutex<std::collections::HashMap<String, BlasRegisterDecl>>,
            tlases: Mutex<std::collections::HashMap<String, TlasRegisterDecl>>,
            kernels: Mutex<std::collections::HashMap<String, RayTracingKernelRegisterDecl>>,
            runs: Mutex<Vec<RayTracingKernelRunDispatch>>,
        }

        impl RecordingRayTracingBridge {
            fn new() -> Arc<Self> {
                Arc::new(Self {
                    blases: Mutex::new(std::collections::HashMap::new()),
                    tlases: Mutex::new(std::collections::HashMap::new()),
                    kernels: Mutex::new(std::collections::HashMap::new()),
                    runs: Mutex::new(Vec::new()),
                })
            }

            fn blas_count(&self) -> usize {
                self.blases.lock().unwrap().len()
            }

            fn tlas_count(&self) -> usize {
                self.tlases.lock().unwrap().len()
            }

            fn kernel_count(&self) -> usize {
                self.kernels.lock().unwrap().len()
            }

            fn last_kernel(&self) -> Option<RayTracingKernelRegisterDecl> {
                self.kernels.lock().unwrap().values().next().cloned()
            }

            fn last_tlas(&self) -> Option<TlasRegisterDecl> {
                self.tlases.lock().unwrap().values().next().cloned()
            }

            fn runs(&self) -> Vec<RayTracingKernelRunDispatch> {
                self.runs.lock().unwrap().clone()
            }

            fn blas_key(decl: &BlasRegisterDecl) -> String {
                use sha2::{Digest, Sha256};
                let mut h = Sha256::new();
                h.update(b"blas|v=");
                for f in &decl.vertices {
                    h.update(&f.to_le_bytes());
                }
                h.update(b"|i=");
                for i in &decl.indices {
                    h.update(&i.to_le_bytes());
                }
                format!("{:x}", h.finalize())
            }

            fn tlas_key(decl: &TlasRegisterDecl) -> String {
                use sha2::{Digest, Sha256};
                let mut h = Sha256::new();
                h.update(b"tlas|n=");
                h.update(&(decl.instances.len() as u32).to_le_bytes());
                for inst in &decl.instances {
                    h.update(b"|b=");
                    h.update(inst.blas_id.as_bytes());
                    h.update(b"|c=");
                    h.update(&inst.custom_index.to_le_bytes());
                    h.update(b"|m=");
                    h.update(&[inst.mask]);
                }
                format!("{:x}", h.finalize())
            }

            fn kernel_key(decl: &RayTracingKernelRegisterDecl) -> String {
                use sha2::{Digest, Sha256};
                let mut h = Sha256::new();
                h.update(b"k|s=");
                h.update(&(decl.stages.len() as u32).to_le_bytes());
                for s in &decl.stages {
                    h.update(&s.spv);
                    h.update(b"|");
                }
                h.update(b"|g=");
                h.update(&(decl.groups.len() as u32).to_le_bytes());
                h.update(b"|nb=");
                h.update(&(decl.bindings.len() as u32).to_le_bytes());
                h.update(b"|pcs=");
                h.update(&decl.push_constant_size.to_le_bytes());
                h.update(b"|mrd=");
                h.update(&decl.max_recursion_depth.to_le_bytes());
                format!("{:x}", h.finalize())
            }
        }

        impl RayTracingKernelBridge for RecordingRayTracingBridge {
            fn register_blas(
                &self,
                decl: &BlasRegisterDecl,
            ) -> std::result::Result<String, String> {
                if decl.vertices.is_empty() || decl.indices.is_empty() {
                    return Err("BLAS requires non-empty vertices + indices".into());
                }
                let id = Self::blas_key(decl);
                self.blases
                    .lock()
                    .unwrap()
                    .entry(id.clone())
                    .or_insert_with(|| decl.clone());
                Ok(id)
            }

            fn register_tlas(
                &self,
                decl: &TlasRegisterDecl,
            ) -> std::result::Result<String, String> {
                if decl.instances.is_empty() {
                    return Err("TLAS must have at least one instance".into());
                }
                let blases = self.blases.lock().unwrap();
                for (i, inst) in decl.instances.iter().enumerate() {
                    if !blases.contains_key(&inst.blas_id) {
                        return Err(format!(
                            "TLAS instance {i} references unknown blas_id '{}'",
                            inst.blas_id
                        ));
                    }
                }
                drop(blases);
                let id = Self::tlas_key(decl);
                self.tlases
                    .lock()
                    .unwrap()
                    .entry(id.clone())
                    .or_insert_with(|| decl.clone());
                Ok(id)
            }

            fn register_kernel(
                &self,
                decl: &RayTracingKernelRegisterDecl,
            ) -> std::result::Result<String, String> {
                if decl.stages.is_empty() {
                    return Err("kernel requires at least one shader stage".into());
                }
                if decl.groups.is_empty() {
                    return Err("kernel requires at least one shader group".into());
                }
                let id = Self::kernel_key(decl);
                self.kernels
                    .lock()
                    .unwrap()
                    .entry(id.clone())
                    .or_insert_with(|| decl.clone());
                Ok(id)
            }

            fn run_kernel(
                &self,
                dispatch: &RayTracingKernelRunDispatch,
            ) -> std::result::Result<(), String> {
                if !self
                    .kernels
                    .lock()
                    .unwrap()
                    .contains_key(&dispatch.kernel_id)
                {
                    return Err(format!(
                        "kernel_id '{}' not registered with this bridge",
                        dispatch.kernel_id
                    ));
                }
                self.runs.lock().unwrap().push(dispatch.clone());
                Ok(())
            }
        }

        fn make_sandbox_with_bridge(
            bridge: Option<Arc<dyn RayTracingKernelBridge>>,
        ) -> Option<GpuContextLimitedAccess> {
            let gpu = match GpuContext::init_for_platform_sync() {
                Ok(g) => g,
                Err(_) => return None,
            };
            if let Some(b) = bridge {
                gpu.set_ray_tracing_kernel_bridge(b);
            }
            Some(GpuContextLimitedAccess::new(gpu))
        }

        // ----- BLAS register tests --------------------------------------

        fn make_blas_req(
            request_id: &str,
            vertices_hex: &str,
            indices_hex: &str,
        ) -> EscalateRequestRegisterAccelerationStructureBlas {
            EscalateRequestRegisterAccelerationStructureBlas {
                request_id: request_id.to_string(),
                label: "test-blas".to_string(),
                vertices_hex: vertices_hex.to_string(),
                indices_hex: indices_hex.to_string(),
            }
        }

        /// Encode `[f32]` as the lowercase hex blob the wire expects.
        fn vertex_hex(vs: &[f32]) -> String {
            let mut bytes = Vec::with_capacity(vs.len() * 4);
            for v in vs {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            bytes_to_hex(&bytes)
        }

        /// Encode `[u32]` as the lowercase hex blob the wire expects.
        fn index_hex(is: &[u32]) -> String {
            let mut bytes = Vec::with_capacity(is.len() * 4);
            for i in is {
                bytes.extend_from_slice(&i.to_le_bytes());
            }
            bytes_to_hex(&bytes)
        }

        fn bytes_to_hex(b: &[u8]) -> String {
            let mut s = String::with_capacity(b.len() * 2);
            for &x in b {
                s.push_str(&format!("{:02x}", x));
            }
            s
        }

        const TRIANGLE_VERTS: &[f32] = &[
            0.0, 0.5, 0.0, // top
            -0.5, -0.5, 0.0, // bottom-left
            0.5, -0.5, 0.0, // bottom-right
        ];
        const TRIANGLE_INDICES: &[u32] = &[0, 1, 2];

        #[test]
        fn register_blas_without_bridge_returns_err() {
            let sandbox = match make_sandbox_with_bridge(None) {
                Some(s) => s,
                None => {
                    println!("register_blas_without_bridge_returns_err: no GPU — skipping");
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                "req-blas-1",
                &vertex_hex(TRIANGLE_VERTS),
                &index_hex(TRIANGLE_INDICES),
            ));
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-blas-1");
                    assert!(
                        err.message.contains("RayTracingKernelBridge"),
                        "expected bridge-not-registered error, got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err when no bridge registered, got {other:?}"),
            }
        }

        #[test]
        fn register_blas_with_invalid_vertex_hex_returns_err() {
            let bridge = RecordingRayTracingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "register_blas_with_invalid_vertex_hex_returns_err: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                "req-bad-v",
                "xyz123",
                &index_hex(TRIANGLE_INDICES),
            ));
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-bad-v");
                    assert!(err.message.contains("vertices_hex"), "got: {}", err.message);
                }
                other => panic!("expected Err for bad vertices_hex, got {other:?}"),
            }
            assert_eq!(bridge.blas_count(), 0);
        }

        #[test]
        fn register_blas_with_misaligned_vertex_blob_returns_err() {
            let bridge = RecordingRayTracingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "register_blas_with_misaligned_vertex_blob_returns_err: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            // 11 bytes (not a multiple of 12 — should be rejected before the
            // bridge is even called).
            let req = EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                "req-misaligned-v",
                &"00".repeat(11),
                &index_hex(TRIANGLE_INDICES),
            ));
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-misaligned-v");
                    assert!(
                        err.message.contains("multiple of 12"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!(
                    "expected Err for misaligned vertex blob, got {other:?}"
                ),
            }
            assert_eq!(bridge.blas_count(), 0);
        }

        #[test]
        fn register_blas_with_misaligned_index_blob_returns_err() {
            let bridge = RecordingRayTracingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "register_blas_with_misaligned_index_blob_returns_err: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            // 8 bytes (not a multiple of 12 — should be rejected).
            let req = EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                "req-misaligned-i",
                &vertex_hex(TRIANGLE_VERTS),
                &"00".repeat(8),
            ));
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-misaligned-i");
                    assert!(
                        err.message.contains("multiple of 12"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!(
                    "expected Err for misaligned index blob, got {other:?}"
                ),
            }
            assert_eq!(bridge.blas_count(), 0);
        }

        #[test]
        fn register_blas_succeeds_and_caches() {
            let bridge = RecordingRayTracingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!("register_blas_succeeds_and_caches: no GPU — skipping");
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req1 = EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                "req-blas-a",
                &vertex_hex(TRIANGLE_VERTS),
                &index_hex(TRIANGLE_INDICES),
            ));
            let resp1 = handle_escalate_op(&sandbox, &registry, req1)
                .expect("must produce a response");
            let id1 = match resp1 {
                EscalateResponse::Ok(ok) => {
                    assert_eq!(ok.request_id, "req-blas-a");
                    ok.handle_id
                }
                other => panic!("expected Ok, got {other:?}"),
            };
            // Re-register identical descriptor — bridge cache hit, same id.
            let req2 = EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                "req-blas-b",
                &vertex_hex(TRIANGLE_VERTS),
                &index_hex(TRIANGLE_INDICES),
            ));
            let resp2 = handle_escalate_op(&sandbox, &registry, req2)
                .expect("must produce a response");
            let id2 = match resp2 {
                EscalateResponse::Ok(ok) => ok.handle_id,
                other => panic!("expected Ok on re-register, got {other:?}"),
            };
            assert_eq!(id1, id2, "identical BLAS descriptors must collide on as_id");
            assert_eq!(bridge.blas_count(), 1, "cache must coalesce identical BLAS");
        }

        // ----- TLAS register tests --------------------------------------

        fn make_tlas_req(
            request_id: &str,
            blas_id: &str,
        ) -> EscalateRequestRegisterAccelerationStructureTlas {
            EscalateRequestRegisterAccelerationStructureTlas {
                request_id: request_id.to_string(),
                label: "test-tlas".to_string(),
                instances: vec![
                    EscalateRequestRegisterAccelerationStructureTlasInstance {
                        blas_id: blas_id.to_string(),
                        transform: vec![
                            1.0, 0.0, 0.0, 0.0, // row 0
                            0.0, 1.0, 0.0, 0.0, // row 1
                            0.0, 0.0, 1.0, 0.0, // row 2
                        ],
                        custom_index: 7,
                        mask: 0xff,
                        sbt_record_offset: 0,
                        flags: 0,
                    },
                ],
            }
        }

        #[test]
        fn register_tlas_with_wrong_transform_length_returns_err() {
            let bridge = RecordingRayTracingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "register_tlas_with_wrong_transform_length_returns_err: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let mut req = make_tlas_req("req-bad-tx", "blas-x");
            req.instances[0].transform = vec![1.0; 11]; // wrong length
            let response = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::RegisterAccelerationStructureTlas(req),
            )
            .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-bad-tx");
                    assert!(err.message.contains("transform"), "got: {}", err.message);
                }
                other => panic!(
                    "expected Err for wrong-length transform, got {other:?}"
                ),
            }
            assert_eq!(bridge.tlas_count(), 0);
        }

        #[test]
        fn register_tlas_with_oversized_mask_returns_err() {
            let bridge = RecordingRayTracingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "register_tlas_with_oversized_mask_returns_err: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let mut req = make_tlas_req("req-bad-mask", "blas-x");
            req.instances[0].mask = 0xfff; // > 0xff, should be rejected
            let response = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::RegisterAccelerationStructureTlas(req),
            )
            .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-bad-mask");
                    assert!(err.message.contains("mask"), "got: {}", err.message);
                }
                other => panic!("expected Err for oversized mask, got {other:?}"),
            }
            assert_eq!(bridge.tlas_count(), 0);
        }

        #[test]
        fn register_tlas_succeeds_after_blas() {
            let bridge = RecordingRayTracingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "register_tlas_succeeds_after_blas: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            // 1. Register a BLAS first to obtain a real as_id.
            let blas_req = EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                "req-blas",
                &vertex_hex(TRIANGLE_VERTS),
                &index_hex(TRIANGLE_INDICES),
            ));
            let blas_resp = handle_escalate_op(&sandbox, &registry, blas_req)
                .expect("must produce a response");
            let blas_id = match blas_resp {
                EscalateResponse::Ok(ok) => ok.handle_id,
                other => panic!("expected Ok for BLAS register, got {other:?}"),
            };
            // 2. Now register a TLAS pointing at it.
            let tlas_req = EscalateRequest::RegisterAccelerationStructureTlas(
                make_tlas_req("req-tlas", &blas_id),
            );
            let tlas_resp = handle_escalate_op(&sandbox, &registry, tlas_req)
                .expect("must produce a response");
            let tlas_id = match tlas_resp {
                EscalateResponse::Ok(ok) => {
                    assert_eq!(ok.request_id, "req-tlas");
                    ok.handle_id
                }
                other => panic!("expected Ok for TLAS register, got {other:?}"),
            };
            assert!(!tlas_id.is_empty(), "TLAS id must be non-empty");
            // Verify the bridge actually saw the right shape.
            let tlas_decl = bridge
                .last_tlas()
                .expect("bridge must have stored the TLAS decl");
            assert_eq!(tlas_decl.instances.len(), 1);
            assert_eq!(tlas_decl.instances[0].blas_id, blas_id);
            assert_eq!(tlas_decl.instances[0].custom_index, 7);
            assert_eq!(tlas_decl.instances[0].mask, 0xff);
            assert_eq!(
                tlas_decl.instances[0].transform,
                [
                    [1.0, 0.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0, 0.0],
                ]
            );
        }

        #[test]
        fn register_tlas_with_unknown_blas_id_returns_err() {
            let bridge = RecordingRayTracingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "register_tlas_with_unknown_blas_id_returns_err: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RegisterAccelerationStructureTlas(make_tlas_req(
                "req-tlas-bad",
                "definitely-not-a-real-blas-id",
            ));
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-tlas-bad");
                    assert!(
                        err.message.contains("unknown blas_id"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!(
                    "expected Err for unknown blas_id, got {other:?}"
                ),
            }
            assert_eq!(bridge.tlas_count(), 0);
        }

        // ----- Kernel register + run tests ------------------------------

        fn make_kernel_req(
            request_id: &str,
        ) -> EscalateRequestRegisterRayTracingKernel {
            EscalateRequestRegisterRayTracingKernel {
                request_id: request_id.to_string(),
                label: "test-rt-kernel".to_string(),
                stages: vec![
                    EscalateRequestRegisterRayTracingKernelStage {
                        stage: EscalateRequestRegisterRayTracingKernelStageStage::RayGen,
                        spv_hex: "deadbeef".to_string(),
                        entry_point: "main".to_string(),
                    },
                    EscalateRequestRegisterRayTracingKernelStage {
                        stage: EscalateRequestRegisterRayTracingKernelStageStage::Miss,
                        spv_hex: "cafebabe".to_string(),
                        entry_point: "main".to_string(),
                    },
                    EscalateRequestRegisterRayTracingKernelStage {
                        stage: EscalateRequestRegisterRayTracingKernelStageStage::ClosestHit,
                        spv_hex: "facefeed".to_string(),
                        entry_point: "main".to_string(),
                    },
                ],
                groups: vec![
                    EscalateRequestRegisterRayTracingKernelGroup {
                        kind: EscalateRequestRegisterRayTracingKernelGroupKind::General,
                        general_stage: 0,
                        closest_hit_stage: RAY_TRACING_STAGE_INDEX_NONE,
                        any_hit_stage: RAY_TRACING_STAGE_INDEX_NONE,
                        intersection_stage: RAY_TRACING_STAGE_INDEX_NONE,
                    },
                    EscalateRequestRegisterRayTracingKernelGroup {
                        kind: EscalateRequestRegisterRayTracingKernelGroupKind::General,
                        general_stage: 1,
                        closest_hit_stage: RAY_TRACING_STAGE_INDEX_NONE,
                        any_hit_stage: RAY_TRACING_STAGE_INDEX_NONE,
                        intersection_stage: RAY_TRACING_STAGE_INDEX_NONE,
                    },
                    EscalateRequestRegisterRayTracingKernelGroup {
                        kind: EscalateRequestRegisterRayTracingKernelGroupKind::TrianglesHit,
                        general_stage: RAY_TRACING_STAGE_INDEX_NONE,
                        closest_hit_stage: 2,
                        any_hit_stage: RAY_TRACING_STAGE_INDEX_NONE,
                        intersection_stage: RAY_TRACING_STAGE_INDEX_NONE,
                    },
                ],
                bindings: vec![
                    EscalateRequestRegisterRayTracingKernelBinding {
                        binding: 0,
                        kind: EscalateRequestRegisterRayTracingKernelBindingKind::AccelerationStructure,
                        stages: 1, // RAYGEN
                    },
                    EscalateRequestRegisterRayTracingKernelBinding {
                        binding: 1,
                        kind: EscalateRequestRegisterRayTracingKernelBindingKind::StorageImage,
                        stages: 1, // RAYGEN
                    },
                ],
                push_constant_size: 16,
                push_constant_stages: 1, // RAYGEN
                max_recursion_depth: 1,
            }
        }

        fn make_run_req(
            request_id: &str,
            kernel_id: &str,
        ) -> EscalateRequestRunRayTracingKernel {
            EscalateRequestRunRayTracingKernel {
                request_id: request_id.to_string(),
                kernel_id: kernel_id.to_string(),
                bindings: vec![
                    EscalateRequestRunRayTracingKernelBinding {
                        binding: 0,
                        kind: EscalateRequestRunRayTracingKernelBindingKind::AccelerationStructure,
                        target_id: "test-tlas-uuid".to_string(),
                    },
                    EscalateRequestRunRayTracingKernelBinding {
                        binding: 1,
                        kind: EscalateRequestRunRayTracingKernelBindingKind::StorageImage,
                        target_id: "test-storage-uuid".to_string(),
                    },
                ],
                push_constants_hex: "00".repeat(16),
                width: 1280,
                height: 720,
                depth: 1,
            }
        }

        #[test]
        fn register_kernel_without_bridge_returns_err() {
            let sandbox = match make_sandbox_with_bridge(None) {
                Some(s) => s,
                None => {
                    println!(
                        "register_kernel_without_bridge_returns_err: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RegisterRayTracingKernel(make_kernel_req("req-k-1"));
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-k-1");
                    assert!(
                        err.message.contains("RayTracingKernelBridge"),
                        "expected bridge-not-registered error, got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err when no bridge registered, got {other:?}"),
            }
        }

        #[test]
        fn register_kernel_with_invalid_stage_hex_returns_err() {
            let bridge = RecordingRayTracingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "register_kernel_with_invalid_stage_hex_returns_err: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let mut req = make_kernel_req("req-bad-stage");
            req.stages[1].spv_hex = "qq".to_string();
            let response = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::RegisterRayTracingKernel(req),
            )
            .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-bad-stage");
                    assert!(
                        err.message.contains("stages[1].spv_hex"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!(
                    "expected Err for bad stage SPIR-V hex, got {other:?}"
                ),
            }
            assert_eq!(bridge.kernel_count(), 0);
        }

        #[test]
        fn register_kernel_with_procedural_missing_intersection_returns_err() {
            let bridge = RecordingRayTracingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "register_kernel_with_procedural_missing_intersection_returns_err: \
                         no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let mut req = make_kernel_req("req-bad-proc");
            // Replace the third group with a procedural_hit that lacks
            // an intersection stage (sentinel-encoded "absent").
            req.groups[2] = EscalateRequestRegisterRayTracingKernelGroup {
                kind: EscalateRequestRegisterRayTracingKernelGroupKind::ProceduralHit,
                general_stage: RAY_TRACING_STAGE_INDEX_NONE,
                closest_hit_stage: 2,
                any_hit_stage: RAY_TRACING_STAGE_INDEX_NONE,
                intersection_stage: RAY_TRACING_STAGE_INDEX_NONE,
            };
            let response = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::RegisterRayTracingKernel(req),
            )
            .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-bad-proc");
                    assert!(
                        err.message.contains("procedural_hit"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!(
                    "expected Err for procedural_hit missing intersection_stage, got {other:?}"
                ),
            }
            assert_eq!(bridge.kernel_count(), 0);
        }

        #[test]
        fn register_kernel_succeeds_and_caches() {
            let bridge = RecordingRayTracingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!("register_kernel_succeeds_and_caches: no GPU — skipping");
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req1 =
                EscalateRequest::RegisterRayTracingKernel(make_kernel_req("req-k-a"));
            let resp1 = handle_escalate_op(&sandbox, &registry, req1)
                .expect("must produce a response");
            let id1 = match resp1 {
                EscalateResponse::Ok(ok) => ok.handle_id,
                other => panic!("expected Ok, got {other:?}"),
            };
            let req2 =
                EscalateRequest::RegisterRayTracingKernel(make_kernel_req("req-k-b"));
            let resp2 = handle_escalate_op(&sandbox, &registry, req2)
                .expect("must produce a response");
            let id2 = match resp2 {
                EscalateResponse::Ok(ok) => ok.handle_id,
                other => panic!("expected Ok, got {other:?}"),
            };
            assert_eq!(id1, id2, "identical kernel descriptors must collide on id");
            assert_eq!(bridge.kernel_count(), 1);

            // Verify the bridge stored what we sent — sanity check on the
            // wire→domain conversion.
            let stored = bridge.last_kernel().expect("must have a stored decl");
            assert_eq!(stored.stages.len(), 3);
            assert_eq!(stored.groups.len(), 3);
            assert_eq!(stored.bindings.len(), 2);
            assert_eq!(stored.push_constant_size, 16);
            assert_eq!(stored.max_recursion_depth, 1);
        }

        #[test]
        fn run_kernel_without_bridge_returns_err() {
            let sandbox = match make_sandbox_with_bridge(None) {
                Some(s) => s,
                None => {
                    println!("run_kernel_without_bridge_returns_err: no GPU — skipping");
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RunRayTracingKernel(make_run_req(
                "req-run-1",
                "kernel-x",
            ));
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-run-1");
                    assert!(
                        err.message.contains("RayTracingKernelBridge"),
                        "expected bridge-not-registered error, got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err when no bridge registered, got {other:?}"),
            }
        }

        #[test]
        fn run_kernel_with_invalid_push_constants_hex_returns_err() {
            let bridge = RecordingRayTracingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "run_kernel_with_invalid_push_constants_hex_returns_err: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let mut req = make_run_req("req-bad-push", "kernel-x");
            req.push_constants_hex = "qq".to_string();
            let response = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::RunRayTracingKernel(req),
            )
            .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-bad-push");
                    assert!(
                        err.message.contains("push_constants_hex"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!(
                    "expected Err for malformed push hex, got {other:?}"
                ),
            }
            assert!(bridge.runs().is_empty());
        }

        #[test]
        fn run_kernel_with_unknown_kernel_id_returns_err() {
            let bridge = RecordingRayTracingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!(
                        "run_kernel_with_unknown_kernel_id_returns_err: no GPU — skipping"
                    );
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RunRayTracingKernel(make_run_req(
                "req-run-x",
                "definitely-not-a-real-kernel-id",
            ));
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-run-x");
                    assert!(
                        err.message.contains("not registered"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!(
                    "expected Err for unknown kernel_id, got {other:?}"
                ),
            }
            assert!(bridge.runs().is_empty());
        }

        #[test]
        fn run_kernel_succeeds_after_register() {
            let bridge = RecordingRayTracingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!("run_kernel_succeeds_after_register: no GPU — skipping");
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            // 1. Register the kernel.
            let kernel_req =
                EscalateRequest::RegisterRayTracingKernel(make_kernel_req("req-k"));
            let kernel_resp = handle_escalate_op(&sandbox, &registry, kernel_req)
                .expect("must produce a response");
            let kernel_id = match kernel_resp {
                EscalateResponse::Ok(ok) => ok.handle_id,
                other => panic!("expected Ok for kernel register, got {other:?}"),
            };
            // 2. Now dispatch it.
            let run_req = EscalateRequest::RunRayTracingKernel(make_run_req(
                "req-run-k",
                &kernel_id,
            ));
            let run_resp = handle_escalate_op(&sandbox, &registry, run_req)
                .expect("must produce a response");
            match run_resp {
                EscalateResponse::Ok(ok) => {
                    assert_eq!(ok.request_id, "req-run-k");
                    assert_eq!(
                        ok.handle_id, kernel_id,
                        "Ok response must echo kernel_id back"
                    );
                }
                other => panic!("expected Ok for run, got {other:?}"),
            }
            // Verify the bridge actually saw the dispatch with the right
            // shape.
            let runs = bridge.runs();
            assert_eq!(runs.len(), 1);
            assert_eq!(runs[0].kernel_id, kernel_id);
            assert_eq!(runs[0].width, 1280);
            assert_eq!(runs[0].height, 720);
            assert_eq!(runs[0].depth, 1);
            assert_eq!(runs[0].bindings.len(), 2);
            assert_eq!(runs[0].push_constants.len(), 16);
        }
    }

    /// Blocking `RunCpuReadbackCopy` with malformed `surface_id` must
    /// report a parse error.
    #[cfg(target_os = "linux")]
    #[test]
    fn run_cpu_readback_copy_malformed_surface_id_returns_err() {
        use crate::core::context::{GpuContext, GpuContextLimitedAccess};

        let gpu = match GpuContext::init_for_platform_sync() {
            Ok(g) => g,
            Err(_) => {
                println!(
                    "run_cpu_readback_copy_malformed_surface_id_returns_err: no GPU — skipping"
                );
                return;
            }
        };
        let sandbox = GpuContextLimitedAccess::new(gpu);
        let registry = EscalateHandleRegistry::new();

        let req = EscalateRequest::RunCpuReadbackCopy(EscalateRequestRunCpuReadbackCopy {
            request_id: "req-cpu-bad".to_string(),
            surface_id: "not-a-u64".to_string(),
            direction: EscalateRequestRunCpuReadbackCopyDirection::BufferToImage,
        });
        let response = handle_escalate_op(&sandbox, &registry, req)
            .expect("run_cpu_readback_copy must produce a response");
        match response {
            EscalateResponse::Err(err) => {
                assert_eq!(err.request_id, "req-cpu-bad");
                assert!(
                    err.message.contains("not a u64") || err.message.contains("invalid"),
                    "got: {}",
                    err.message
                );
            }
            EscalateResponse::Ok(_) => panic!("malformed surface_id must not succeed"),
            EscalateResponse::Contended(_) => {
                panic!("malformed surface_id must surface as Err, not Contended")
            }
        }
    }

    #[test]
    fn handle_escalate_op_end_to_end() {
        use crate::core::context::GpuContext;
        use crate::core::context::GpuContextLimitedAccess;

        let gpu = match GpuContext::init_for_platform_sync() {
            Ok(g) => g,
            Err(_) => {
                println!("handle_escalate_op_end_to_end: no GPU device — skipping");
                return;
            }
        };
        let sandbox = GpuContextLimitedAccess::new(gpu);
        let registry = EscalateHandleRegistry::new();

        let acquire =
            EscalateRequest::AcquirePixelBuffer(EscalateRequestAcquirePixelBuffer {
                request_id: "req-1".to_string(),
                width: 320,
                height: 240,
                format: "bgra".to_string(),
            });
        let response = handle_escalate_op(&sandbox, &registry, acquire)
            .expect("acquire_pixel_buffer must produce a response");
        let buffer_handle_id = match response {
            EscalateResponse::Ok(ref ok) => {
                assert_eq!(ok.request_id, "req-1");
                assert_eq!(ok.width, Some(320));
                assert_eq!(ok.height, Some(240));
                assert_eq!(ok.format.as_deref(), Some("bgra32"));
                assert!(ok.usage.is_none(), "pixel buffers have no usage field");
                assert!(!ok.handle_id.is_empty(), "handle id should not be empty");
                ok.handle_id.clone()
            }
            EscalateResponse::Err(err) => {
                panic!("acquire_pixel_buffer escalate failed: {}", err.message);
            }
            EscalateResponse::Contended(_) => {
                panic!("acquire_pixel_buffer must never return Contended")
            }
        };
        assert_eq!(registry.handle_count(), 1);

        let acquire_tex =
            EscalateRequest::AcquireTexture(EscalateRequestAcquireTexture {
                request_id: "req-tex".to_string(),
                width: 256,
                height: 128,
                format: "rgba8_unorm".to_string(),
                usage: vec![
                    "texture_binding".to_string(),
                    "copy_src".to_string(),
                ],
            });
        let response = handle_escalate_op(&sandbox, &registry, acquire_tex)
            .expect("acquire_texture must produce a response");
        let texture_handle_id = match response {
            EscalateResponse::Ok(ref ok) => {
                assert_eq!(ok.request_id, "req-tex");
                assert_eq!(ok.width, Some(256));
                assert_eq!(ok.height, Some(128));
                assert_eq!(ok.format.as_deref(), Some("rgba8_unorm"));
                let usage = ok.usage.as_deref().expect("acquire_texture sets usage");
                assert!(usage.iter().any(|u| u == "texture_binding"));
                assert!(usage.iter().any(|u| u == "copy_src"));
                assert!(!ok.handle_id.is_empty(), "texture handle id should not be empty");
                assert_ne!(
                    ok.handle_id, buffer_handle_id,
                    "texture and buffer should get distinct handle ids"
                );
                ok.handle_id.clone()
            }
            EscalateResponse::Err(err) => {
                panic!("acquire_texture escalate failed: {}", err.message);
            }
            EscalateResponse::Contended(_) => {
                panic!("acquire_texture must never return Contended")
            }
        };
        assert_eq!(registry.handle_count(), 2);

        let release_tex = EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
            request_id: "req-tex-rel".to_string(),
            handle_id: texture_handle_id.clone(),
        });
        match handle_escalate_op(&sandbox, &registry, release_tex)
            .expect("release_handle must produce a response")
        {
            EscalateResponse::Ok(ok) => {
                assert_eq!(ok.request_id, "req-tex-rel");
                assert_eq!(ok.handle_id, texture_handle_id);
            }
            EscalateResponse::Err(err) => panic!("release_handle (texture) failed: {}", err.message),
            EscalateResponse::Contended(_) => panic!("release_handle must never return Contended"),
        }
        assert_eq!(registry.handle_count(), 1);

        let release = EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
            request_id: "req-2".to_string(),
            handle_id: buffer_handle_id.clone(),
        });
        let response = handle_escalate_op(&sandbox, &registry, release)
            .expect("release_handle must produce a response");
        match response {
            EscalateResponse::Ok(ok) => {
                assert_eq!(ok.request_id, "req-2");
                assert_eq!(ok.handle_id, buffer_handle_id);
            }
            EscalateResponse::Err(err) => panic!("release_handle failed: {}", err.message),
            EscalateResponse::Contended(_) => panic!("release_handle must never return Contended"),
        }
        assert_eq!(registry.handle_count(), 0);

        let release_unknown =
            EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
                request_id: "req-3".to_string(),
                handle_id: "never-existed".to_string(),
            });
        match handle_escalate_op(&sandbox, &registry, release_unknown)
            .expect("release_handle must produce a response")
        {
            EscalateResponse::Err(err) => {
                assert_eq!(err.request_id, "req-3");
                assert!(err.message.contains("not found"));
            }
            EscalateResponse::Ok(_) => panic!("unknown handle should not succeed"),
            EscalateResponse::Contended(_) => {
                panic!("release_handle on unknown id must surface Err, not Contended")
            }
        }
    }

    /// Tests for the escalate-IPC `{op:"log"}` variant (issue #442).
    ///
    /// These tests assert the full pipeline: wire parse → host dispatch →
    /// polyglot sink → drain worker → JSONL file. Each test runs with
    /// `#[serial]` and its own `TempDir`-scoped `XDG_STATE_HOME` so the
    /// JSONL writer writes to a path we can read back.
    mod log_op {
        use super::*;
        use std::sync::Arc;
        use std::time::Duration;

        use serial_test::serial;
        use tempfile::TempDir;

        use crate::core::logging::{
            init_for_tests, LogLevel, RuntimeLogEvent, Source,
            StreamlibLoggingConfig, StreamlibLoggingGuard,
        };
        use crate::core::runtime::RuntimeUniqueId;

        fn install_logging(runtime_tag: &str) -> (TempDir, StreamlibLoggingGuard) {
            let tmp = TempDir::new().unwrap();
            unsafe {
                std::env::set_var("XDG_STATE_HOME", tmp.path());
                // Capture debug+ so all the test levels surface.
                std::env::set_var("RUST_LOG", "debug");
                std::env::remove_var("STREAMLIB_QUIET");
            }
            let runtime_id = Arc::new(RuntimeUniqueId::from(runtime_tag));
            let config = StreamlibLoggingConfig::for_runtime("test", runtime_id);
            let guard = init_for_tests(config).unwrap();
            (tmp, guard)
        }

        fn read_jsonl(path: &std::path::Path) -> Vec<RuntimeLogEvent> {
            let contents = std::fs::read_to_string(path).unwrap_or_default();
            contents
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| serde_json::from_str::<RuntimeLogEvent>(l).expect("valid JSONL"))
                .collect()
        }

        fn dispatch_log(log: EscalateRequestLog) {
            push_polyglot_record(log_record_from_wire(log));
        }

        fn sample_log(seq: &str, ts: &str, level: EscalateRequestLogLevel) -> EscalateRequestLog {
            EscalateRequestLog {
                source: EscalateRequestLogSource::Python,
                source_seq: seq.to_string(),
                source_ts: ts.to_string(),
                level,
                message: format!("record {seq}"),
                intercepted: false,
                channel: None,
                pipeline_id: Some("pl-1".into()),
                processor_id: Some("pr-1".into()),
                attrs: HashMap::new(),
            }
        }

        /// Every optional and required field on the wire round-trips
        /// byte-for-byte through serde; the discriminator dispatches to
        /// [`EscalateRequest::Log`] on decode.
        #[test]
        fn schema_round_trip() {
            let mut attrs = HashMap::new();
            attrs.insert("device".to_string(), Some(serde_json::json!("/dev/video0")));
            attrs.insert("count".to_string(), Some(serde_json::json!(3)));
            let original = EscalateRequestLog {
                source: EscalateRequestLogSource::Python,
                source_seq: "9001".into(),
                source_ts: "2026-04-23T14:00:00Z".into(),
                level: EscalateRequestLogLevel::Warn,
                message: "hello".into(),
                intercepted: true,
                channel: Some("fd1".into()),
                pipeline_id: Some("pl-42".into()),
                processor_id: Some("camera-1".into()),
                attrs: attrs.clone(),
            };
            let wrapped = EscalateRequest::Log(original.clone());
            let json = serde_json::to_value(&wrapped).expect("serializes");
            assert_eq!(json.get("op").and_then(|v| v.as_str()), Some("log"));

            let decoded: EscalateRequest = serde_json::from_value(json).expect("decodes");
            match decoded {
                EscalateRequest::Log(back) => {
                    assert_eq!(back.source, original.source);
                    assert_eq!(back.source_seq, original.source_seq);
                    assert_eq!(back.source_ts, original.source_ts);
                    assert_eq!(back.level, original.level);
                    assert_eq!(back.message, original.message);
                    assert_eq!(back.intercepted, original.intercepted);
                    assert_eq!(back.channel, original.channel);
                    assert_eq!(back.pipeline_id, original.pipeline_id);
                    assert_eq!(back.processor_id, original.processor_id);
                    assert_eq!(back.attrs, original.attrs);
                }
                other => panic!("expected Log variant, got {other:?}"),
            }
        }

        /// `level: "warn"` on the wire produces a JSONL record with
        /// `level: "warn"`; required structured fields land in their
        /// dedicated columns (not `attrs`) and `host_ts` is stamped
        /// non-zero by the host.
        #[test]
        #[serial]
        fn host_emits_jsonl_record_at_correct_level() {
            let (_tmp, guard) = install_logging("RlogOpLv");
            let path = guard.jsonl_path().unwrap().to_path_buf();

            dispatch_log(sample_log("42", "2026-04-23T14:00:00Z", EscalateRequestLogLevel::Warn));

            drop(guard);

            let events = read_jsonl(&path);
            let record = events
                .iter()
                .find(|e| e.source == Source::Python && e.message == "record 42")
                .unwrap_or_else(|| panic!("no polyglot record; got {events:#?}"));
            assert_eq!(record.level, LogLevel::Warn);
            assert_eq!(record.source_seq, Some(42));
            assert_eq!(record.source_ts.as_deref(), Some("2026-04-23T14:00:00Z"));
            assert_eq!(record.pipeline_id.as_deref(), Some("pl-1"));
            assert_eq!(record.processor_id.as_deref(), Some("pr-1"));
            assert!(record.host_ts > 0, "host stamp must be non-zero");
        }

        /// Two records with identical `source_ts` receive distinct
        /// monotonically-increasing `host_ts` — subprocesses with broken
        /// clocks can't collapse ordering by accident.
        #[test]
        #[serial]
        fn host_stamps_host_ts() {
            let (_tmp, guard) = install_logging("RlogOpTs");
            let path = guard.jsonl_path().unwrap().to_path_buf();

            let ts = "2026-04-23T14:00:00Z";
            dispatch_log(sample_log("1", ts, EscalateRequestLogLevel::Info));
            std::thread::sleep(Duration::from_millis(2));
            dispatch_log(sample_log("2", ts, EscalateRequestLogLevel::Info));

            drop(guard);

            let events = read_jsonl(&path);
            let polyglot: Vec<_> = events
                .iter()
                .filter(|e| e.source == Source::Python)
                .collect();
            assert_eq!(polyglot.len(), 2, "expected exactly 2 polyglot records");
            assert_eq!(polyglot[0].source_ts, polyglot[1].source_ts);
            assert!(
                polyglot[1].host_ts > polyglot[0].host_ts,
                "host_ts must be monotonic: {} vs {}",
                polyglot[0].host_ts,
                polyglot[1].host_ts,
            );
        }

        /// `intercepted: true` + `channel: "fd1"` survive the wire → host
        /// → JSONL hop untouched, landing in their dedicated columns.
        #[test]
        #[serial]
        fn intercepted_flag_round_trip() {
            let (_tmp, guard) = install_logging("RlogOpInt");
            let path = guard.jsonl_path().unwrap().to_path_buf();

            let mut log = sample_log("7", "2026-04-23T14:00:00Z", EscalateRequestLogLevel::Error);
            log.intercepted = true;
            log.channel = Some("fd1".into());
            log.message = "fd1 capture".into();
            dispatch_log(log);

            drop(guard);

            let events = read_jsonl(&path);
            let record = events
                .iter()
                .find(|e| e.source == Source::Python && e.message == "fd1 capture")
                .unwrap_or_else(|| panic!("no polyglot record; got {events:#?}"));
            assert!(record.intercepted);
            assert_eq!(record.channel.as_deref(), Some("fd1"));
            assert_eq!(record.level, LogLevel::Error);
        }

        /// 1000 records with strictly increasing `source_seq` arrive at
        /// the JSONL file in the same order. Proves the single-producer
        /// path preserves FIFO without extra sequencing logic.
        #[test]
        #[serial]
        fn within_source_fifo_preserved() {
            let (_tmp, guard) = install_logging("RlogOpFif");
            let path = guard.jsonl_path().unwrap().to_path_buf();

            for i in 0..1000 {
                dispatch_log(sample_log(
                    &i.to_string(),
                    "2026-04-23T14:00:00Z",
                    EscalateRequestLogLevel::Debug,
                ));
            }

            drop(guard);

            let events = read_jsonl(&path);
            let seqs: Vec<u64> = events
                .iter()
                .filter(|e| e.source == Source::Python)
                .filter_map(|e| e.source_seq)
                .collect();
            assert_eq!(seqs.len(), 1000, "all records must land in JSONL");
            for (expected, got) in seqs.iter().enumerate() {
                assert_eq!(
                    *got, expected as u64,
                    "records out of order at index {expected}",
                );
            }
        }

        /// Rust + Python + Deno emit interleaved records into the unified
        /// JSONL pathway. Verifies the architectural contract from #430:
        /// `host_ts` is the authoritative sort key across the merged
        /// stream (monotonically non-decreasing) and `source_seq` is
        /// preserved within each subprocess source (monotonically
        /// increasing). Rust records carry no `source_seq` because the
        /// host-local tracing layer has no need for one — host receipt
        /// IS the local order.
        #[test]
        #[serial]
        fn cross_language_source_seq_monotonic_within_source() {
            let (_tmp, guard) = install_logging("RxLang");
            let path = guard.jsonl_path().unwrap().to_path_buf();

            // Round-robin emit Rust / Python / Deno. Each subprocess
            // source carries a monotonic `source_seq`; Rust records do
            // not. A 50µs nap between emissions guarantees `host_ts`
            // strictly increases, which is the stronger property — the
            // contract only requires non-decreasing.
            const ROUNDS: u64 = 16;
            let mut py_seq = 0u64;
            let mut deno_seq = 0u64;
            for _ in 0..ROUNDS {
                tracing::info!(round = py_seq, "rust-merged");
                std::thread::sleep(Duration::from_micros(50));

                let py_log = EscalateRequestLog {
                    source: EscalateRequestLogSource::Python,
                    source_seq: py_seq.to_string(),
                    source_ts: "2026-04-25T12:00:00Z".into(),
                    level: EscalateRequestLogLevel::Info,
                    message: format!("py-merged-{py_seq}"),
                    intercepted: false,
                    channel: None,
                    pipeline_id: Some("pl-merge".into()),
                    processor_id: Some("pr-merge".into()),
                    attrs: HashMap::new(),
                };
                dispatch_log(py_log);
                py_seq += 1;
                std::thread::sleep(Duration::from_micros(50));

                let deno_log = EscalateRequestLog {
                    source: EscalateRequestLogSource::Deno,
                    source_seq: deno_seq.to_string(),
                    source_ts: "2026-04-25T12:00:00Z".into(),
                    level: EscalateRequestLogLevel::Info,
                    message: format!("deno-merged-{deno_seq}"),
                    intercepted: false,
                    channel: None,
                    pipeline_id: Some("pl-merge".into()),
                    processor_id: Some("pr-merge".into()),
                    attrs: HashMap::new(),
                };
                dispatch_log(deno_log);
                deno_seq += 1;
                std::thread::sleep(Duration::from_micros(50));
            }

            drop(guard);

            let events = read_jsonl(&path);

            let merged: Vec<&RuntimeLogEvent> = events
                .iter()
                .filter(|e| {
                    e.message.starts_with("rust-merged")
                        || e.message.starts_with("py-merged-")
                        || e.message.starts_with("deno-merged-")
                })
                .collect();
            assert_eq!(
                merged.len(),
                (ROUNDS * 3) as usize,
                "expected {} merged-stream records, got {}: {merged:#?}",
                ROUNDS * 3,
                merged.len()
            );

            // host_ts is the authoritative cross-source order.
            for pair in merged.windows(2) {
                assert!(
                    pair[1].host_ts >= pair[0].host_ts,
                    "host_ts must be monotonic across merged stream: \
                     {} ({:?}) precedes {} ({:?})",
                    pair[0].message,
                    pair[0].host_ts,
                    pair[1].message,
                    pair[1].host_ts,
                );
            }

            // source_seq is monotonic within each subprocess source and
            // covers exactly [0, ROUNDS).
            let py_seqs: Vec<u64> = merged
                .iter()
                .filter(|e| e.source == Source::Python)
                .filter_map(|e| e.source_seq)
                .collect();
            assert_eq!(
                py_seqs,
                (0..ROUNDS).collect::<Vec<u64>>(),
                "python source_seq must be monotonic and contiguous"
            );
            let deno_seqs: Vec<u64> = merged
                .iter()
                .filter(|e| e.source == Source::Deno)
                .filter_map(|e| e.source_seq)
                .collect();
            assert_eq!(
                deno_seqs,
                (0..ROUNDS).collect::<Vec<u64>>(),
                "deno source_seq must be monotonic and contiguous"
            );

            // Rust records carry no source_seq — host-local tracing has
            // no use for one.
            let rust_records: Vec<&RuntimeLogEvent> = merged
                .iter()
                .copied()
                .filter(|e| e.source == Source::Rust)
                .collect();
            assert_eq!(rust_records.len(), ROUNDS as usize);
            for record in &rust_records {
                assert!(
                    record.source_seq.is_none(),
                    "rust records must not carry source_seq; got {:?}",
                    record.source_seq,
                );
                assert_eq!(record.level, LogLevel::Info);
            }
        }
    }

    /// End-to-end tests that spawn a real Python 3 subprocess, have it
    /// call `streamlib.log.*`, read the framed escalate-IPC traffic off
    /// its stdout, dispatch each frame through the host handler, and
    /// assert the records land in the unified JSONL.
    ///
    /// These sit above the wire-format unit tests in `log_op` and the
    /// Python-side pytest suite — together they pin the whole loop from
    /// `streamlib.log.info("msg")` in Python to a JSONL line on disk.
    ///
    /// Skipped when `python3` is not on PATH (minimal sandboxes).
    mod python_subprocess {
        use std::io::{BufReader, Read};
        use std::path::PathBuf;
        use std::process::{Command, Stdio};
        use std::time::Duration;

        use super::*;
        use crate::core::compiler::compiler_ops::subprocess_bridge::{
            spawn_fd_line_reader, EscalateTransport,
        };
        use crate::core::logging::{
            init_for_tests, LogLevel, RuntimeLogEvent, Source,
            StreamlibLoggingConfig, StreamlibLoggingGuard,
        };
        use crate::core::runtime::RuntimeUniqueId;
        use serial_test::serial;
        use std::sync::Arc;
        use tempfile::TempDir;

        fn python3() -> Option<PathBuf> {
            let path_env = std::env::var_os("PATH")?;
            for dir in std::env::split_paths(&path_env) {
                let candidate = dir.join("python3");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
            None
        }

        fn streamlib_python_path() -> PathBuf {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("streamlib-python")
                .join("python")
        }

        fn install_logging(tag: &str) -> (TempDir, StreamlibLoggingGuard) {
            let tmp = TempDir::new().unwrap();
            unsafe {
                std::env::set_var("XDG_STATE_HOME", tmp.path());
                std::env::set_var("RUST_LOG", "debug");
                std::env::remove_var("STREAMLIB_QUIET");
            }
            let runtime_id = Arc::new(RuntimeUniqueId::from(tag));
            let config = StreamlibLoggingConfig::for_runtime("test", runtime_id);
            let guard = init_for_tests(config).unwrap();
            (tmp, guard)
        }

        fn read_jsonl(path: &std::path::Path) -> Vec<RuntimeLogEvent> {
            let contents = std::fs::read_to_string(path).unwrap_or_default();
            contents
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| {
                    serde_json::from_str::<RuntimeLogEvent>(l).expect("valid JSONL")
                })
                .collect()
        }

        /// Run the given Python snippet with streamlib-python on PYTHONPATH.
        /// Returns `None` when `python3` is missing.
        ///
        /// Reads length-prefixed JSON frames from the subprocess stdout
        /// and feeds each through `try_parse_escalate_request` →
        /// `handle_escalate_op`, mirroring what the real bridge reader
        /// does on a live host.
        fn run_and_drain(snippet: &str) -> Option<usize> {
            let py = python3()?;
            let lib = streamlib_python_path();
            if !lib.exists() {
                return None;
            }
            let mut child = Command::new(py)
                .arg("-c")
                .arg(snippet)
                .env("PYTHONPATH", &lib)
                .env_remove("PYTHONHOME")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn python3");

            let stdout = child.stdout.take().expect("child stdout");
            let mut reader = BufReader::new(stdout);
            let mut frame_count = 0usize;

            // The `process_bridge_message` pipeline expects a
            // `GpuContextLimitedAccess` for resource ops; log ops never
            // touch it. We build a parse → dispatch loop that handles
            // `log` directly via `handle_escalate_op` with a sandbox
            // that is never read on the log path. This keeps the test
            // independent of GPU availability.
            loop {
                let mut len_buf = [0u8; 4];
                match reader.read_exact(&mut len_buf) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(e) => panic!("bridge read failed: {e}"),
                }
                let len = u32::from_be_bytes(len_buf) as usize;
                let mut buf = vec![0u8; len];
                reader.read_exact(&mut buf).expect("read frame body");
                let value: serde_json::Value =
                    serde_json::from_slice(&buf).expect("valid JSON frame");
                let parsed = match try_parse_escalate_request(&value) {
                    Some(Ok(op)) => op,
                    Some(Err(e)) => panic!("escalate decode failed: {}", e.message),
                    None => panic!(
                        "python subprocess only sends escalate traffic; got {value}"
                    ),
                };
                // For log ops we only need to drive the wire-decode →
                // sink path. Non-log ops are not expected from the
                // helper snippet.
                if let EscalateRequest::Log(log_op) = parsed {
                    push_polyglot_record(log_record_from_wire(log_op));
                    frame_count += 1;
                } else {
                    panic!("unexpected escalate op from helper snippet");
                }
            }

            // Drain stderr for diagnostics.
            if let Some(mut stderr) = child.stderr.take() {
                let mut s = String::new();
                let _ = stderr.read_to_string(&mut s);
                if !s.is_empty() {
                    eprintln!("python subprocess stderr:\n{s}");
                }
            }

            let _ = child.wait();
            Some(frame_count)
        }

        // Post-#604 the EscalateChannel takes a single writer; the
        // bridge reader thread (started by subprocess_runner.main)
        // owns the read side. These log-only tests don't need a
        // reader thread — they just enqueue records that the writer
        // thread frames onto stdout.
        const HELPER_PREAMBLE: &str = r#"
import sys
from streamlib import log
from streamlib.escalate import EscalateChannel
channel = EscalateChannel(sys.stdout.buffer)
log.set_processor_id("pr-test")
log.set_pipeline_id("pl-test")
log.install(channel, install_interceptors=False)
"#;

        /// `streamlib.log.info("hi", ...)` from Python surfaces in the
        /// host JSONL with `source=python`, correct message, level, and
        /// context fields.
        #[test]
        #[serial]
        fn python_log_surfaces_in_host_jsonl() {
            let (_tmp, guard) = install_logging("PyLogSurf");
            let path = guard.jsonl_path().unwrap().to_path_buf();

            let body = r#"
log.info("hi from python", count=7)
log.shutdown()
"#;
            let snippet = format!("{HELPER_PREAMBLE}{body}");
            let frames = match run_and_drain(&snippet) {
                Some(n) => n,
                None => {
                    println!(
                        "python3 or streamlib-python source missing — skipping"
                    );
                    return;
                }
            };
            assert!(frames >= 1, "expected at least one frame, got {frames}");

            drop(guard);

            let events = read_jsonl(&path);
            let record = events
                .iter()
                .find(|e| {
                    e.source == Source::Python && e.message == "hi from python"
                })
                .unwrap_or_else(|| panic!("no python record; got {events:#?}"));
            assert_eq!(record.level, LogLevel::Info);
            assert_eq!(record.pipeline_id.as_deref(), Some("pl-test"));
            assert_eq!(record.processor_id.as_deref(), Some("pr-test"));
            assert_eq!(
                record.attrs.get("count").and_then(|v| v.as_i64()),
                Some(7)
            );
            assert!(record.host_ts > 0);
        }

        /// A burst of 20 records arrives fully ordered and distinct — FIFO
        /// holds across the real `queue.Queue` + writer-thread →
        /// length-prefixed-frame → wire path.
        #[test]
        #[serial]
        fn python_log_burst_preserves_order() {
            let (_tmp, guard) = install_logging("PyLogBurst");
            let path = guard.jsonl_path().unwrap().to_path_buf();

            let body = r#"
for i in range(20):
    log.info("burst", index=i)
log.shutdown()
"#;
            let snippet = format!("{HELPER_PREAMBLE}{body}");
            let frames = match run_and_drain(&snippet) {
                Some(n) => n,
                None => {
                    println!("python3 missing — skipping");
                    return;
                }
            };
            assert_eq!(frames, 20, "subprocess should emit all 20 frames");

            drop(guard);

            let events = read_jsonl(&path);
            let indices: Vec<i64> = events
                .iter()
                .filter(|e| e.source == Source::Python && e.message == "burst")
                .filter_map(|e| e.attrs.get("index").and_then(|v| v.as_i64()))
                .collect();
            assert_eq!(indices.len(), 20, "all 20 records should land");
            assert_eq!(
                indices,
                (0..20).collect::<Vec<i64>>(),
                "order must match emission order"
            );
        }

        /// Spawn `python3` with the host's escalate-transport + fd1/fd2
        /// line readers installed exactly like the real spawn path does,
        /// then run a caller-supplied snippet. Parent closes its end of
        /// the escalate socketpair immediately so the child is free to
        /// exit once the snippet finishes. Returns the child handle +
        /// the kept-alive parent-side socket half (dropped by the
        /// caller after it's done with the run). `None` when `python3`
        /// isn't available.
        fn spawn_python_with_host_fd_readers(
            snippet: &str,
            processor_id: &str,
        ) -> Option<(std::process::Child, std::os::unix::net::UnixStream)> {
            let py = python3()?;
            let lib = streamlib_python_path();
            if !lib.exists() {
                return None;
            }
            let mut command = Command::new(py);
            command
                .arg("-c")
                .arg(snippet)
                .env("PYTHONPATH", &lib)
                .env_remove("PYTHONHOME")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut transport =
                EscalateTransport::attach(&mut command).expect("attach transport");

            let mut child = command.spawn().expect("spawn python3");
            transport.release_child_end();

            if let Some(stdout) = child.stdout.take() {
                spawn_fd_line_reader(stdout, "py-stdout", "fd1", processor_id);
            }
            if let Some(stderr) = child.stderr.take() {
                spawn_fd_line_reader(stderr, "py-stderr", "fd2", processor_id);
            }

            let parent_socket = transport.into_parent_stream();
            Some((child, parent_socket))
        }

        /// A raw `os.write(1, …)` from a Python subprocess — the
        /// canonical case a C-extension or `printf` from a loaded C
        /// library would hit — must now surface in the host JSONL as
        /// `intercepted=true, channel="fd1", source="python"`. Deferred
        /// from #443; unlocked by moving escalate IPC onto the
        /// dedicated socketpair so fd1 is free to capture raw writes.
        #[cfg(unix)]
        #[test]
        #[serial]
        fn python_os_write_fd1_intercepted() {
            let (_tmp, guard) = install_logging("PyFd1Intercept");
            let path = guard.jsonl_path().unwrap().to_path_buf();

            let snippet = r#"
import os
os.write(1, b"hi from c\n")
"#;
            let (mut child, _sock) =
                match spawn_python_with_host_fd_readers(snippet, "pr-fd1") {
                    Some(v) => v,
                    None => {
                        println!("python3 missing — skipping");
                        return;
                    }
                };

            // Wait for child to exit and for the fd1 reader thread to
            // flush the final line into the JSONL worker queue.
            let _ = child.wait();
            std::thread::sleep(Duration::from_millis(200));

            drop(guard);

            let events = read_jsonl(&path);
            let record = events
                .iter()
                .find(|e| {
                    e.intercepted
                        && e.channel.as_deref() == Some("fd1")
                        && e.source == Source::Python
                        && e.message == "hi from c"
                })
                .unwrap_or_else(|| {
                    panic!(
                        "no fd1-intercepted record for python; got {events:#?}"
                    )
                });
            assert_eq!(record.level, LogLevel::Warn);
            assert_eq!(record.processor_id.as_deref(), Some("pr-fd1"));
        }

        /// Sanity: fd2 capture survives the transport move. Confirms
        /// the existing fd2 path from #443 still works after #451
        /// promoted fd1 to a captured log pipe.
        #[cfg(unix)]
        #[test]
        #[serial]
        fn python_stderr_fd2_intercepted_on_dedicated_fd_transport() {
            let (_tmp, guard) = install_logging("PyFd2Intercept");
            let path = guard.jsonl_path().unwrap().to_path_buf();

            let snippet = r#"
import os
os.write(2, b"stderr after transport move\n")
"#;
            let (mut child, _sock) =
                match spawn_python_with_host_fd_readers(snippet, "pr-fd2") {
                    Some(v) => v,
                    None => {
                        println!("python3 missing — skipping");
                        return;
                    }
                };

            let _ = child.wait();
            std::thread::sleep(Duration::from_millis(200));

            drop(guard);

            let events = read_jsonl(&path);
            let record = events
                .iter()
                .find(|e| {
                    e.intercepted
                        && e.channel.as_deref() == Some("fd2")
                        && e.source == Source::Python
                        && e.message == "stderr after transport move"
                })
                .unwrap_or_else(|| {
                    panic!(
                        "no fd2-intercepted record for python; got {events:#?}"
                    )
                });
            assert_eq!(record.level, LogLevel::Warn);
            assert_eq!(record.processor_id.as_deref(), Some("pr-fd2"));
        }
    }

    /// End-to-end tests that spawn a real Deno subprocess, have it call
    /// `streamlib.log.*`, read framed escalate-IPC traffic off its
    /// stdout, dispatch each frame through the host handler, and assert
    /// the records land in the unified JSONL.
    ///
    /// Mirrors `python_subprocess` above for the Deno runtime.
    ///
    /// Skipped when `deno` is not on PATH or when the streamlib-deno
    /// source tree is not present.
    mod deno_subprocess {
        use std::io::{BufReader, Read, Write};
        use std::path::PathBuf;
        use std::process::{Command, Stdio};
        use std::time::Duration;

        use super::*;
        use crate::core::compiler::compiler_ops::subprocess_bridge::{
            spawn_fd_line_reader, EscalateTransport,
        };
        use crate::core::logging::{
            init_for_tests, LogLevel, RuntimeLogEvent, Source,
            StreamlibLoggingConfig, StreamlibLoggingGuard,
        };
        use crate::core::runtime::RuntimeUniqueId;
        use serial_test::serial;
        use std::sync::Arc;
        use tempfile::TempDir;

        fn deno_binary() -> Option<PathBuf> {
            let path_env = std::env::var_os("PATH")?;
            for dir in std::env::split_paths(&path_env) {
                let candidate = dir.join("deno");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
            None
        }

        fn streamlib_deno_path() -> PathBuf {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("streamlib-deno")
        }

        fn install_logging(tag: &str) -> (TempDir, StreamlibLoggingGuard) {
            let tmp = TempDir::new().unwrap();
            unsafe {
                std::env::set_var("XDG_STATE_HOME", tmp.path());
                std::env::set_var("RUST_LOG", "debug");
                std::env::remove_var("STREAMLIB_QUIET");
            }
            let runtime_id = Arc::new(RuntimeUniqueId::from(tag));
            let config = StreamlibLoggingConfig::for_runtime("test", runtime_id);
            let guard = init_for_tests(config).unwrap();
            (tmp, guard)
        }

        fn read_jsonl(path: &std::path::Path) -> Vec<RuntimeLogEvent> {
            let contents = std::fs::read_to_string(path).unwrap_or_default();
            contents
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| {
                    serde_json::from_str::<RuntimeLogEvent>(l).expect("valid JSONL")
                })
                .collect()
        }

        /// Build a Deno helper script (TypeScript) that imports `log` +
        /// `EscalateChannel` from the streamlib-deno SDK at `sdk_path`,
        /// sets the processor context, installs the writer, and runs the
        /// caller-supplied `body` inside a top-level async IIFE. Writes
        /// the script to a temp file inside `tmp` and returns its path.
        fn write_helper_script(
            tmp: &TempDir,
            sdk_path: &std::path::Path,
            body: &str,
        ) -> PathBuf {
            let log_url = format!("file://{}/log.ts", sdk_path.display());
            let escalate_url =
                format!("file://{}/escalate.ts", sdk_path.display());
            let script = format!(
                r#"// auto-generated test helper
import * as log from "{log_url}";
import {{ EscalateChannel }} from "{escalate_url}";

async function bridgeWrite(msg: Record<string, unknown>): Promise<void> {{
  const text = JSON.stringify(msg);
  const encoded = new TextEncoder().encode(text);
  const lenBuf = new Uint8Array(4);
  new DataView(lenBuf.buffer).setUint32(0, encoded.length, false);
  await Deno.stdout.write(lenBuf);
  await Deno.stdout.write(encoded);
}}

const channel = new EscalateChannel(bridgeWrite);
log.setProcessorContext({{ processorId: "pr-test", pipelineId: "pl-test" }});
await log.install(channel, {{ installInterceptors: false }});

(async () => {{
{body}
await log.shutdown();
}})();
"#,
                log_url = log_url,
                escalate_url = escalate_url,
                body = body,
            );
            let script_path = tmp.path().join("deno_log_helper.ts");
            std::fs::write(&script_path, script).expect("write helper script");
            script_path
        }

        /// Run the helper, drain framed escalate frames from the
        /// subprocess's stdout, dispatch each `log` op into the host
        /// JSONL pipeline. Returns frame count, or `None` when `deno`
        /// or the SDK source isn't available.
        fn run_and_drain(body: &str) -> Option<usize> {
            let deno = deno_binary()?;
            let sdk = streamlib_deno_path();
            if !sdk.exists() {
                return None;
            }
            let tmp = TempDir::new().unwrap();
            let script = write_helper_script(&tmp, &sdk, body);

            let mut child = Command::new(deno)
                .arg("run")
                .arg("--quiet")
                .arg("--allow-read")
                .arg("--allow-env")
                .arg("--no-prompt")
                .arg(&script)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn deno");

            // Subprocess never reads from stdin; close it so the writer
            // task can drain and shutdown returns cleanly.
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(&[]);
            }

            let stdout = child.stdout.take().expect("child stdout");
            let mut reader = BufReader::new(stdout);
            let mut frame_count = 0usize;

            loop {
                let mut len_buf = [0u8; 4];
                match reader.read_exact(&mut len_buf) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(e) => panic!("bridge read failed: {e}"),
                }
                let len = u32::from_be_bytes(len_buf) as usize;
                let mut buf = vec![0u8; len];
                reader.read_exact(&mut buf).expect("read frame body");
                let value: serde_json::Value =
                    serde_json::from_slice(&buf).expect("valid JSON frame");
                let parsed = match try_parse_escalate_request(&value) {
                    Some(Ok(op)) => op,
                    Some(Err(e)) => panic!("escalate decode failed: {}", e.message),
                    None => panic!(
                        "deno subprocess only sends escalate traffic; got {value}"
                    ),
                };
                if let EscalateRequest::Log(log_op) = parsed {
                    push_polyglot_record(log_record_from_wire(log_op));
                    frame_count += 1;
                } else {
                    panic!("unexpected escalate op from helper snippet");
                }
            }

            // Drain stderr for diagnostics.
            if let Some(mut stderr) = child.stderr.take() {
                let mut s = String::new();
                let _ = stderr.read_to_string(&mut s);
                if !s.is_empty() {
                    eprintln!("deno subprocess stderr:\n{s}");
                }
            }

            let _ = child.wait();
            Some(frame_count)
        }

        /// `streamlib.log.info("hi", ...)` from Deno surfaces in the
        /// host JSONL with `source=deno`, correct message, level, and
        /// context fields.
        #[test]
        #[serial]
        fn deno_log_surfaces_in_host_jsonl() {
            let (_tmp, guard) = install_logging("DenoLogSurf");
            let path = guard.jsonl_path().unwrap().to_path_buf();

            let body = r#"log.info("hi from deno", { count: 7 });"#;
            let frames = match run_and_drain(body) {
                Some(n) => n,
                None => {
                    println!(
                        "deno or streamlib-deno source missing — skipping"
                    );
                    return;
                }
            };
            assert!(frames >= 1, "expected at least one frame, got {frames}");

            drop(guard);

            let events = read_jsonl(&path);
            let record = events
                .iter()
                .find(|e| {
                    e.source == Source::Deno && e.message == "hi from deno"
                })
                .unwrap_or_else(|| panic!("no deno record; got {events:#?}"));
            assert_eq!(record.level, LogLevel::Info);
            assert_eq!(record.pipeline_id.as_deref(), Some("pl-test"));
            assert_eq!(record.processor_id.as_deref(), Some("pr-test"));
            assert_eq!(
                record.attrs.get("count").and_then(|v| v.as_i64()),
                Some(7)
            );
            assert!(record.host_ts > 0);
        }

        /// A burst of 20 records arrives fully ordered and distinct —
        /// FIFO holds across the queue → writer-task → length-prefixed-
        /// frame → wire path.
        #[test]
        #[serial]
        fn deno_log_burst_preserves_order() {
            let (_tmp, guard) = install_logging("DenoLogBurst");
            let path = guard.jsonl_path().unwrap().to_path_buf();

            let body = r#"for (let i = 0; i < 20; i++) log.info("burst", { index: i });"#;
            let frames = match run_and_drain(body) {
                Some(n) => n,
                None => {
                    println!("deno missing — skipping");
                    return;
                }
            };
            assert_eq!(frames, 20, "subprocess should emit all 20 frames");

            drop(guard);

            let events = read_jsonl(&path);
            let indices: Vec<i64> = events
                .iter()
                .filter(|e| e.source == Source::Deno && e.message == "burst")
                .filter_map(|e| e.attrs.get("index").and_then(|v| v.as_i64()))
                .collect();
            assert_eq!(indices.len(), 20, "all 20 records should land");
            assert_eq!(
                indices,
                (0..20).collect::<Vec<i64>>(),
                "order must match emission order"
            );
        }

        /// Spawn `deno eval` with the host's escalate-transport +
        /// fd1/fd2 line readers installed. Mirrors the Python helper.
        /// Returns `(child, parent_socket)` or `None` when Deno is
        /// missing.
        fn spawn_deno_with_host_fd_readers(
            snippet: &str,
            processor_id: &str,
        ) -> Option<(std::process::Child, std::os::unix::net::UnixStream)> {
            let deno = deno_binary()?;
            let mut command = Command::new(deno);
            command
                .arg("eval")
                .arg(snippet)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut transport =
                EscalateTransport::attach(&mut command).expect("attach transport");

            let mut child = command.spawn().expect("spawn deno");
            transport.release_child_end();

            if let Some(stdout) = child.stdout.take() {
                spawn_fd_line_reader(stdout, "dn-stdout", "fd1", processor_id);
            }
            if let Some(stderr) = child.stderr.take() {
                spawn_fd_line_reader(stderr, "dn-stderr", "fd2", processor_id);
            }

            let parent_socket = transport.into_parent_stream();
            Some((child, parent_socket))
        }

        /// Raw `Deno.stdout.writeSync(…)` from a Deno subprocess lands
        /// in the host JSONL as
        /// `intercepted=true, channel="fd1", source="deno"`. Equivalent
        /// to `python_os_write_fd1_intercepted`; unlocked by #451
        /// moving escalate IPC onto the dedicated socketpair.
        #[cfg(unix)]
        #[test]
        #[serial]
        fn deno_stdout_fd1_intercepted() {
            let (_tmp, guard) = install_logging("DenoFd1Intercept");
            let path = guard.jsonl_path().unwrap().to_path_buf();

            let snippet = r#"Deno.stdout.writeSync(new TextEncoder().encode("hi from deno\n"));"#;
            let (mut child, _sock) =
                match spawn_deno_with_host_fd_readers(snippet, "pr-fd1-deno") {
                    Some(v) => v,
                    None => {
                        println!("deno missing — skipping");
                        return;
                    }
                };

            let _ = child.wait();
            std::thread::sleep(Duration::from_millis(200));

            drop(guard);

            let events = read_jsonl(&path);
            let record = events
                .iter()
                .find(|e| {
                    e.intercepted
                        && e.channel.as_deref() == Some("fd1")
                        && e.source == Source::Deno
                        && e.message == "hi from deno"
                })
                .unwrap_or_else(|| {
                    panic!(
                        "no fd1-intercepted record for deno; got {events:#?}"
                    )
                });
            assert_eq!(record.level, LogLevel::Warn);
            assert_eq!(record.processor_id.as_deref(), Some("pr-fd1-deno"));
        }

        /// Sanity: fd2 capture survives the Deno transport move.
        /// Mirrors `python_stderr_fd2_intercepted_on_dedicated_fd_transport`.
        #[cfg(unix)]
        #[test]
        #[serial]
        fn deno_stderr_fd2_intercepted_on_dedicated_fd_transport() {
            let (_tmp, guard) = install_logging("DenoFd2Intercept");
            let path = guard.jsonl_path().unwrap().to_path_buf();

            let snippet = r#"Deno.stderr.writeSync(new TextEncoder().encode("stderr after transport move\n"));"#;
            let (mut child, _sock) =
                match spawn_deno_with_host_fd_readers(snippet, "pr-fd2-deno") {
                    Some(v) => v,
                    None => {
                        println!("deno missing — skipping");
                        return;
                    }
                };

            let _ = child.wait();
            std::thread::sleep(Duration::from_millis(200));

            drop(guard);

            let events = read_jsonl(&path);
            let record = events
                .iter()
                .find(|e| {
                    e.intercepted
                        && e.channel.as_deref() == Some("fd2")
                        && e.source == Source::Deno
                        && e.message == "stderr after transport move"
                })
                .unwrap_or_else(|| {
                    panic!(
                        "no fd2-intercepted record for deno; got {events:#?}"
                    )
                });
            assert_eq!(record.level, LogLevel::Warn);
            assert_eq!(record.processor_id.as_deref(), Some("pr-fd2-deno"));
        }
    }
}
