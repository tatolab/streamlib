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

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use uuid::Uuid;

#[cfg(target_os = "linux")]
use crate::core::rhi::GlslCompilationTargetStage;
#[cfg(target_os = "linux")]
use crate::host_rhi::HostSurfaceStoreExt;

use super::subprocess_escalate_wire_types::escalate_request::{
    EscalateComputeBindingKind, EscalateRequestAcquireImage, EscalateRequestAcquirePixelBuffer,
    EscalateRequestAcquireTexture, EscalateRequestCopyDeviceExportStagingBackToSurface,
    EscalateRequestLog, EscalateRequestLogLevel, EscalateRequestLogSource,
    EscalateRequestOpenCpuReadbackStaging, EscalateRequestOpenDeviceExportStaging,
    EscalateRequestRefillDeviceExportStaging, EscalateRequestRegisterAccelerationStructureBlas,
    EscalateRequestRegisterAccelerationStructureTlas, EscalateRequestRegisterComputeKernel,
    EscalateRequestRegisterGraphicsKernel, EscalateRequestRegisterGraphicsKernelBindingKind,
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
    EscalateRequestRunComputeKernel, EscalateRequestRunComputeKernelBatch,
    EscalateRequestRunComputeKernelBinding, EscalateRequestRunCpuReadbackCopy,
    EscalateRequestRunCpuReadbackCopyDirection, EscalateRequestRunGraphicsDraw,
    EscalateRequestRunGraphicsDrawBindingKind, EscalateRequestRunGraphicsDrawDrawKind,
    EscalateRequestRunGraphicsDrawIndexBufferIndexType, EscalateRequestRunRayTracingKernel,
    EscalateRequestRunRayTracingKernelBindingKind, EscalateRequestTryRunCpuReadbackCopy,
    EscalateRequestTryRunCpuReadbackCopyDirection, EscalateRequestWaitDeviceIdle,
};
#[cfg(target_os = "linux")]
use super::subprocess_escalate_wire_types::escalate_response::EscalateResponseComputeBinding;
use super::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseContended, EscalateResponseErr, EscalateResponseOk,
};
use super::subprocess_escalate_wire_types::{EscalateRequest, EscalateResponse};
use crate::core::context::GpuContextLimitedAccess;
#[cfg(target_os = "linux")]
use crate::core::context::{
    BlasRegisterDecl, BlendFactorWire, BlendOpWire, CullModeWire, DepthCompareOpWire,
    DepthFormatWire, DynamicStateWire, FrontFaceWire, GraphicsBindingDecl, GraphicsBindingKindWire,
    GraphicsBindingValue, GraphicsDrawSpec, GraphicsIndexBufferBinding, GraphicsKernelBridge,
    GraphicsKernelRegisterDecl, GraphicsKernelRunDraw, GraphicsPipelineStateWire,
    GraphicsVertexBufferBinding, IndexTypeWire, PolygonModeWire, PrimitiveTopologyWire,
    RAY_TRACING_STAGE_INDEX_NONE, RayTracingBindingDecl, RayTracingBindingKindWire,
    RayTracingBindingValue, RayTracingKernelBridge, RayTracingKernelRegisterDecl,
    RayTracingKernelRunDispatch, RayTracingShaderGroupWire, RayTracingShaderStageWire,
    RayTracingStageDecl, ScissorRectWire, SurfaceExportStagingResidency, TlasInstanceDeclWire,
    TlasRegisterDecl, VertexAttributeFormatWire, VertexInputAttributeDecl, VertexInputBindingDecl,
    VertexInputRateWire, ViewportWire,
};
use crate::core::context::{PooledTextureHandle, TexturePoolDescriptor};
use crate::core::logging::{LogLevel, LogRecord, Source, push_polyglot_record};
use crate::core::rhi::{PixelBuffer, PixelFormat, TextureFormat, TextureUsages};

#[cfg(test)]
use crate::core::error::{Error, Result};

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
        EscalateRequest::WaitDeviceIdle(p) => Some(&p.request_id),
        EscalateRequest::OpenCpuReadbackStaging(p) => Some(&p.request_id),
        EscalateRequest::OpenDeviceExportStaging(p) => Some(&p.request_id),
        EscalateRequest::RefillDeviceExportStaging(p) => Some(&p.request_id),
        EscalateRequest::CopyDeviceExportStagingBackToSurface(p) => Some(&p.request_id),
        EscalateRequest::TryRunCpuReadbackCopy(p) => Some(&p.request_id),
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
///
/// Timeline Arcs (when present) keep the per-edge single-writer
/// timelines alive for the registration's lifetime — surface-share
/// duplicates the FDs at register-time via SCM_RIGHTS, but the
/// host-side `Arc<HostVulkanTimelineSemaphore>` must outlive the
/// registration so the kernel objects backing the FDs aren't
/// destroyed. See `docs/architecture/adapter-timeline-single-writer.md`.
pub(crate) enum RegisteredHandle {
    #[allow(dead_code)]
    PixelBuffer(PixelBuffer),
    #[allow(dead_code)]
    Texture {
        texture: PooledTextureHandle,
        #[cfg(target_os = "linux")]
        produce_done: Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>>,
        #[cfg(target_os = "linux")]
        consume_done: Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>>,
    },
    /// Render-target image handed out via `AcquireImage`. The texture
    /// itself returns to its pool when the variant drops; the
    /// timelines keep their FDs alive for surface-share consumers.
    #[cfg(target_os = "linux")]
    #[allow(dead_code)]
    Image {
        texture: crate::core::rhi::Texture,
        produce_done: Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
        consume_done: Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
    },
}

/// Tracks resources acquired on behalf of a subprocess so `release_handle` —
/// or subprocess death — can drop the host's strong reference. Resources stay
/// alive for the duration of the host pool; this map simply prevents the
/// resource from being immediately recycled while the subprocess still
/// references it by ID. Dropping a [`PooledTextureHandle`] releases the pool
/// slot; dropping an [`PixelBuffer`] releases its refcount.
#[derive(Default)]
pub(crate) struct EscalateHandleRegistry {
    handles: Mutex<HashMap<String, RegisteredHandle>>,
}

impl EscalateHandleRegistry {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn insert_buffer(&self, handle_id: String, buffer: PixelBuffer) {
        let mut map = self.handles.lock().expect("poisoned");
        map.insert(handle_id, RegisteredHandle::PixelBuffer(buffer));
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn insert_texture(
        &self,
        handle_id: String,
        texture: PooledTextureHandle,
        produce_done: Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>>,
        consume_done: Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>>,
    ) {
        let mut map = self.handles.lock().expect("poisoned");
        map.insert(
            handle_id,
            RegisteredHandle::Texture {
                texture,
                produce_done,
                consume_done,
            },
        );
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn insert_texture(&self, handle_id: String, texture: PooledTextureHandle) {
        let mut map = self.handles.lock().expect("poisoned");
        map.insert(handle_id, RegisteredHandle::Texture { texture });
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn insert_image(
        &self,
        handle_id: String,
        texture: crate::core::rhi::Texture,
        produce_done: Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
        consume_done: Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
    ) {
        let mut map = self.handles.lock().expect("poisoned");
        map.insert(
            handle_id,
            RegisteredHandle::Image {
                texture,
                produce_done,
                consume_done,
            },
        );
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
        }) => Some(match PixelFormat::parse_wire_name(&format) {
            Ok(parsed) => {
                let acquired = sandbox.escalate(|full| {
                    let (published_frame_id, buffer) =
                        full.acquire_pixel_buffer(width, height, parsed)?;
                    let handle_id = assign_buffer_handle_id(full, &published_frame_id, &buffer)?;
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
                            format: Some(parsed.wire_name().to_string()),
                            ..Default::default()
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
            let desc =
                TexturePoolDescriptor::new(width, height, parsed_format).with_usage(parsed_usage);
            #[cfg(target_os = "linux")]
            let acquired = sandbox.escalate(|full| {
                let texture = full.acquire_texture(&desc)?;
                let (handle_id, produce_done, consume_done) =
                    assign_texture_handle_id(full, &texture)?;
                Ok((handle_id, texture, produce_done, consume_done))
            });
            #[cfg(not(target_os = "linux"))]
            let acquired = sandbox.escalate(|full| {
                let texture = full.acquire_texture(&desc)?;
                let (handle_id,) = assign_texture_handle_id(full, &texture)?;
                Ok((handle_id, texture))
            });
            Some(match acquired {
                #[cfg(target_os = "linux")]
                Ok((handle_id, texture, produce_done, consume_done)) => {
                    registry.insert_texture(handle_id.clone(), texture, produce_done, consume_done);
                    EscalateResponse::Ok(EscalateResponseOk {
                        request_id: rid,
                        handle_id,
                        width: Some(width),
                        height: Some(height),
                        format: Some(texture_format_to_wire(parsed_format).to_string()),
                        usage: Some(texture_usages_to_wire(parsed_usage)),
                        ..Default::default()
                    })
                }
                #[cfg(not(target_os = "linux"))]
                Ok((handle_id, texture)) => {
                    registry.insert_texture(handle_id.clone(), texture);
                    EscalateResponse::Ok(EscalateResponseOk {
                        request_id: rid,
                        handle_id,
                        width: Some(width),
                        height: Some(height),
                        format: Some(texture_format_to_wire(parsed_format).to_string()),
                        usage: Some(texture_usages_to_wire(parsed_usage)),
                        ..Default::default()
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
                    let texture =
                        full.acquire_render_target_dma_buf_image(width, height, parsed_format)?;
                    let (handle_id, produce_done, consume_done) =
                        assign_image_handle_id(full, &texture)?;
                    Ok((handle_id, texture, produce_done, consume_done))
                });
                Some(match acquired {
                    Ok((handle_id, texture, produce_done, consume_done)) => {
                        // Stash the texture + timeline pair in the
                        // registry so the FDs handed to surface-share
                        // stay valid for the registration's lifetime.
                        registry.insert_image(
                            handle_id.clone(),
                            texture,
                            produce_done,
                            consume_done,
                        );
                        EscalateResponse::Ok(EscalateResponseOk {
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
                            ..Default::default()
                        })
                    }
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
                    message:
                        "acquire_image is only available on Linux (DMA-BUF render-target path)"
                            .to_string(),
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
                Some(handle_surface_export_staging_copy(
                    sandbox,
                    rid,
                    &surface_id,
                    SurfaceExportStagingCopyOp::RunCpuReadbackCopy(match direction {
                        EscalateRequestRunCpuReadbackCopyDirection::ImageToBuffer => {
                            SurfaceExportStagingCopyDirection::SurfaceIntoStaging
                        }
                        EscalateRequestRunCpuReadbackCopyDirection::BufferToImage => {
                            SurfaceExportStagingCopyDirection::StagingBackIntoSurface
                        }
                    }),
                ))
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
                Some(handle_surface_export_staging_copy(
                    sandbox,
                    rid,
                    &surface_id,
                    SurfaceExportStagingCopyOp::TryRunCpuReadbackCopy(match direction {
                        EscalateRequestTryRunCpuReadbackCopyDirection::ImageToBuffer => {
                            SurfaceExportStagingCopyDirection::SurfaceIntoStaging
                        }
                        EscalateRequestTryRunCpuReadbackCopyDirection::BufferToImage => {
                            SurfaceExportStagingCopyDirection::StagingBackIntoSurface
                        }
                    }),
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
        }) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_open_cpu_readback_staging(sandbox, rid, &surface_id))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = surface_id;
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "open_cpu_readback_staging is only available on Linux".to_string(),
                }))
            }
        }
        EscalateRequest::OpenDeviceExportStaging(EscalateRequestOpenDeviceExportStaging {
            request_id: _,
            surface_id,
        }) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_open_device_export_staging(sandbox, rid, &surface_id))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = surface_id;
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "open_device_export_staging is only available on Linux".to_string(),
                }))
            }
        }
        EscalateRequest::RefillDeviceExportStaging(EscalateRequestRefillDeviceExportStaging {
            request_id: _,
            surface_id,
        }) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_surface_export_staging_copy(
                    sandbox,
                    rid,
                    &surface_id,
                    SurfaceExportStagingCopyOp::RefillDeviceExportStaging,
                ))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = surface_id;
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "refill_device_export_staging is only available on Linux".to_string(),
                }))
            }
        }
        EscalateRequest::CopyDeviceExportStagingBackToSurface(
            EscalateRequestCopyDeviceExportStagingBackToSurface {
                request_id: _,
                surface_id,
            },
        ) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_surface_export_staging_copy(
                    sandbox,
                    rid,
                    &surface_id,
                    SurfaceExportStagingCopyOp::CopyDeviceExportStagingBackToSurface,
                ))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = surface_id;
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "copy_device_export_staging_back_to_surface is only available on \
                              Linux"
                        .to_string(),
                }))
            }
        }
        EscalateRequest::RegisterComputeKernel(req) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_register_compute_kernel(sandbox, rid, req))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = req;
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "register_compute_kernel is only available on Linux".to_string(),
                }))
            }
        }
        EscalateRequest::RunComputeKernel(req) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_run_compute_kernel(sandbox, rid, req))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = req;
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "run_compute_kernel is only available on Linux".to_string(),
                }))
            }
        }
        EscalateRequest::RunComputeKernelBatch(req) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_run_compute_kernel_batch(sandbox, rid, req))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = req;
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "run_compute_kernel_batch is only available on Linux".to_string(),
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
                Some(handle_register_acceleration_structure_blas(
                    sandbox, rid, req,
                ))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = req;
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "register_acceleration_structure_blas is only available on Linux"
                        .to_string(),
                }))
            }
        }
        EscalateRequest::RegisterAccelerationStructureTlas(req) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_register_acceleration_structure_tlas(
                    sandbox, rid, req,
                ))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = req;
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "register_acceleration_structure_tlas is only available on Linux"
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
                    message: "register_ray_tracing_kernel is only available on Linux".to_string(),
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
                    ..Default::default()
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
/// ordering. Parses `source_seq` from its string wire encoding (JSON has
/// no 64-bit integer); silently drops the value on parse failure so a
/// malformed subprocess can't block log delivery.
fn log_record_from_wire(log: EscalateRequestLog) -> LogRecord {
    let source = match log.source {
        EscalateRequestLogSource::Python => Source::Python,
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
/// `surface_id` becomes the handle_id. On other platforms the published frame
/// id stays as-is (macOS uses its own XPC `check_in_surface` path via the
/// native lib directly).
#[allow(unused_variables)]
fn assign_buffer_handle_id(
    full: &crate::core::context::GpuContextFullAccess,
    published_frame_id: &crate::core::rhi::PublishedPixelBufferFrameId,
    buffer: &PixelBuffer,
) -> crate::core::error::Result<String> {
    #[cfg(target_os = "linux")]
    {
        if let Some(store) = full.surface_store() {
            return store.check_in(buffer);
        }
    }
    Ok(published_frame_id.to_string())
}

/// Resolve the `handle_id` returned to the subprocess for a pooled texture.
///
/// On Linux, register the texture's DMA-BUF with the surface-share service under a fresh UUID
/// so the subprocess can `check_out` it; on other platforms just mint a UUID.
///
/// On Linux, also allocates and registers a single-writer-per-edge
/// timeline pair (`produce_done` + `consume_done` per
/// `docs/architecture/adapter-timeline-single-writer.md`). The
/// timelines are returned so the caller can stash them in the
/// [`EscalateHandleRegistry`]; the surface-share registration
/// duplicates the FDs via SCM_RIGHTS but the host-side Arcs must
/// outlive the registration.
#[cfg(target_os = "linux")]
fn assign_texture_handle_id(
    full: &crate::core::context::GpuContextFullAccess,
    texture: &PooledTextureHandle,
) -> crate::core::error::Result<(
    String,
    Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>>,
    Option<Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>>,
)> {
    let handle_id = Uuid::new_v4().to_string();
    if let Some(store) = full.surface_store() {
        let host_device = full.host_vulkan_device_arc()?;
        // Single-writer-per-edge timelines. Escalate-IPC consumers
        // (CPU-readback bridge today) handle sync via the
        // per-acquire response and don't drive these timelines,
        // but the surface-share IPC delivers both FDs to the
        // cdylib so future consumers riding the dual-timeline
        // contract see them.
        let produce_done = Arc::new(
            crate::vulkan::rhi::HostVulkanTimelineSemaphore::new_exportable(
                host_device.device(),
                0,
            )
            .map_err(|e| {
                crate::core::error::Error::GpuError(format!(
                    "assign_texture_handle_id: new_exportable (produce_done): {e}"
                ))
            })?,
        );
        let consume_done = Arc::new(
            crate::vulkan::rhi::HostVulkanTimelineSemaphore::new_exportable(
                host_device.device(),
                0,
            )
            .map_err(|e| {
                crate::core::error::Error::GpuError(format!(
                    "assign_texture_handle_id: new_exportable (consume_done): {e}"
                ))
            })?,
        );
        // UNDEFINED at registration: pooled textures sit in the
        // texture pool unowned until the first acquire. The host
        // adapter or escalate-IPC bridge transitions to its
        // workload-specific layout on first use; subsequent
        // releases publish the post-release layout via
        // `update_image_layout`.
        store.register_texture(
            &handle_id,
            texture.texture(),
            Some(produce_done.as_ref()),
            Some(consume_done.as_ref()),
            streamlib_consumer_rhi::VulkanLayout::UNDEFINED,
        )?;
        Ok((handle_id, Some(produce_done), Some(consume_done)))
    } else {
        Ok((handle_id, None, None))
    }
}

#[cfg(not(target_os = "linux"))]
fn assign_texture_handle_id(
    _full: &crate::core::context::GpuContextFullAccess,
    _texture: &PooledTextureHandle,
) -> crate::core::error::Result<(String,)> {
    Ok((Uuid::new_v4().to_string(),))
}

/// Resolve the `handle_id` for a render-target DMA-BUF image.
///
/// On Linux, register the image's DMA-BUF (with the chosen DRM modifier and
/// per-plane row pitches) with the surface-share service under a fresh UUID
/// so the subprocess can `check_out` it; the surface-share registration
/// carries the modifier and strides the consumer-side EGL import requires.
///
/// Also allocates a single-writer-per-edge timeline pair and registers
/// it with surface-share. Returns the timelines so the caller can
/// stash them in the [`EscalateHandleRegistry`].
#[cfg(target_os = "linux")]
fn assign_image_handle_id(
    full: &crate::core::context::GpuContextFullAccess,
    texture: &crate::core::rhi::Texture,
) -> crate::core::error::Result<(
    String,
    Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
    Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>,
)> {
    let handle_id = Uuid::new_v4().to_string();
    let host_device = full.host_vulkan_device_arc()?;
    let produce_done = Arc::new(
        crate::vulkan::rhi::HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0)
            .map_err(|e| {
                crate::core::error::Error::GpuError(format!(
                    "assign_image_handle_id: new_exportable (produce_done): {e}"
                ))
            })?,
    );
    let consume_done = Arc::new(
        crate::vulkan::rhi::HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0)
            .map_err(|e| {
                crate::core::error::Error::GpuError(format!(
                    "assign_image_handle_id: new_exportable (consume_done): {e}"
                ))
            })?,
    );
    if let Some(store) = full.surface_store() {
        // Render-target images are freshly allocated and unwritten at
        // registration time — declare UNDEFINED and let the first
        // producer publish their post-release layout via
        // `update_image_layout` once they've issued their QFOT
        // release barrier (#633).
        store.register_texture(
            &handle_id,
            texture,
            Some(produce_done.as_ref()),
            Some(consume_done.as_ref()),
            streamlib_consumer_rhi::VulkanLayout::UNDEFINED,
        )?;
    }
    Ok((handle_id, produce_done, consume_done))
}

/// The four wire ops that run one surface-export staging copy.
///
/// Named as ops rather than as a (residency x direction x may-wait)
/// product because that product has a cell the wire does not: there is no
/// non-blocking device-export copy. Enumerating the ops keeps every state
/// reachable and gives each one its name for free.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
enum SurfaceExportStagingCopyOp {
    RefillDeviceExportStaging,
    CopyDeviceExportStagingBackToSurface,
    RunCpuReadbackCopy(SurfaceExportStagingCopyDirection),
    TryRunCpuReadbackCopy(SurfaceExportStagingCopyDirection),
}

/// Which way one surface-export staging copy runs.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
enum SurfaceExportStagingCopyDirection {
    /// A refill: the surface's current pixels into the staging, so the
    /// consumer's next read sees this frame.
    SurfaceIntoStaging,
    /// A publish: the consumer's edit back into the surface's own
    /// allocation, so every other holder observes it.
    StagingBackIntoSurface,
}

/// Whether a copy may wait for the staging's recorder.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
enum SurfaceExportStagingCopyContention {
    WaitForTheRecorder,
    ReportContended,
}

#[cfg(target_os = "linux")]
impl SurfaceExportStagingCopyOp {
    /// The wire op name, for error messages that have to say which
    /// request failed.
    fn escalate_op_name(self) -> &'static str {
        match self {
            Self::RefillDeviceExportStaging => "refill_device_export_staging",
            Self::CopyDeviceExportStagingBackToSurface => {
                "copy_device_export_staging_back_to_surface"
            }
            Self::RunCpuReadbackCopy(_) => "run_cpu_readback_copy",
            Self::TryRunCpuReadbackCopy(_) => "try_run_cpu_readback_copy",
        }
    }

    fn residency(self) -> SurfaceExportStagingResidency {
        match self {
            Self::RefillDeviceExportStaging | Self::CopyDeviceExportStagingBackToSurface => {
                SurfaceExportStagingResidency::DeviceLocal
            }
            Self::RunCpuReadbackCopy(_) | Self::TryRunCpuReadbackCopy(_) => {
                SurfaceExportStagingResidency::HostVisible
            }
        }
    }

    fn direction(self) -> SurfaceExportStagingCopyDirection {
        match self {
            Self::RefillDeviceExportStaging => {
                SurfaceExportStagingCopyDirection::SurfaceIntoStaging
            }
            Self::CopyDeviceExportStagingBackToSurface => {
                SurfaceExportStagingCopyDirection::StagingBackIntoSurface
            }
            Self::RunCpuReadbackCopy(direction) | Self::TryRunCpuReadbackCopy(direction) => {
                direction
            }
        }
    }

    fn contention(self) -> SurfaceExportStagingCopyContention {
        match self {
            Self::TryRunCpuReadbackCopy(_) => SurfaceExportStagingCopyContention::ReportContended,
            // Spelled out rather than caught: a wire op that gained the
            // wrong contention here would answer `contended` to a child
            // with no arm for it, and a catch-all would carry that
            // silently past every test.
            Self::RefillDeviceExportStaging
            | Self::CopyDeviceExportStagingBackToSurface
            | Self::RunCpuReadbackCopy(_) => SurfaceExportStagingCopyContention::WaitForTheRecorder,
        }
    }
}

/// The wire op that opens a staging at `residency`.
#[cfg(target_os = "linux")]
fn escalate_open_op_name(residency: SurfaceExportStagingResidency) -> &'static str {
    match residency {
        SurfaceExportStagingResidency::DeviceLocal => "open_device_export_staging",
        SurfaceExportStagingResidency::HostVisible => "open_cpu_readback_staging",
    }
}

/// Open the device-local export staging for `surface_id` on behalf of a
/// helper process — the residency an external device API (CUDA) imports.
#[cfg(target_os = "linux")]
fn handle_open_device_export_staging(
    sandbox: &GpuContextLimitedAccess,
    request_id: String,
    surface_id: &str,
) -> EscalateResponse {
    handle_open_surface_export_staging(
        sandbox,
        request_id,
        surface_id,
        SurfaceExportStagingResidency::DeviceLocal,
    )
}

/// Open one residency's export staging for `surface_id` on behalf of a
/// helper process: allocate it if the surface has none at that
/// residency, publish it and its refill timeline to the surface-share
/// service, and answer with everything the child needs to reach the
/// memory — the id to check out, the geometry the staging was sized for,
/// whether a write-back is possible, and the UUID of the GPU that owns
/// it.
///
/// No fd travels on this socket. The staging's OPAQUE_FD and the
/// timeline's fd reach the child through the surface-share check-out it
/// makes with the returned id.
#[cfg(target_os = "linux")]
fn handle_open_surface_export_staging(
    sandbox: &GpuContextLimitedAccess,
    request_id: String,
    surface_id: &str,
    residency: SurfaceExportStagingResidency,
) -> EscalateResponse {
    let opened = (|| -> crate::core::error::Result<EscalateResponseOk> {
        let staging = sandbox.surface_export_staging(surface_id, residency)?;
        let (shared_id, pixel_format) = sandbox.share_surface_export_staging(&staging)?;
        Ok(EscalateResponseOk {
            request_id: request_id.clone(),
            handle_id: shared_id,
            width: Some(staging.surface_width()),
            height: Some(staging.surface_height()),
            format: Some(pixel_format.wire_name().to_string()),
            staging_byte_size: Some(staging.staging_byte_size().to_string()),
            bytes_per_row: Some(staging.bytes_per_row().to_string()),
            writable: Some(staging.writable()),
            exporting_device_uuid: Some(
                staging
                    .exporting_device_uuid()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            ),
            ..Default::default()
        })
    })();
    match opened {
        Ok(response) => EscalateResponse::Ok(response),
        Err(failure) => EscalateResponse::Err(EscalateResponseErr {
            request_id,
            message: format!("{} failed: {failure}", escalate_open_op_name(residency)),
        }),
    }
}

/// Run one surface-export staging copy on behalf of a helper process and
/// answer with the timeline value it signalled.
///
/// The child waits for that value on its imported copy of the staging's
/// `refill_done` timeline before touching the memory — the host's own
/// bounded wait orders the submit for callers in this process, but it
/// says nothing to a consumer one process away.
///
/// Always available at either residency: the staging is a `GpuContext`
/// capability, minted on first ask, with no installation step and
/// nothing supplied by the application.
///
/// Only `try_run_cpu_readback_copy` can answer
/// [`EscalateResponse::Contended`], and only because another copy holds
/// this staging's recorder. Every other refusal — a retired frame id, a
/// read-only export, a geometry change — is an error for every op. The
/// blocking arms map through `Some`, so a child with no `contended` arm
/// can never be handed one.
#[cfg(target_os = "linux")]
fn handle_surface_export_staging_copy(
    sandbox: &GpuContextLimitedAccess,
    request_id: String,
    surface_id: &str,
    op: SurfaceExportStagingCopyOp,
) -> EscalateResponse {
    use SurfaceExportStagingCopyContention as Contention;
    use SurfaceExportStagingCopyDirection as Direction;

    let copied = sandbox
        .surface_export_staging(surface_id, op.residency())
        .and_then(|staging| match (op.direction(), op.contention()) {
            (Direction::SurfaceIntoStaging, Contention::WaitForTheRecorder) => sandbox
                .refill_surface_export_staging(&staging, surface_id)
                .map(Some),
            (Direction::StagingBackIntoSurface, Contention::WaitForTheRecorder) => sandbox
                .copy_surface_export_staging_back_to_surface(&staging, surface_id)
                .map(Some),
            (Direction::SurfaceIntoStaging, Contention::ReportContended) => {
                sandbox.try_refill_surface_export_staging(&staging, surface_id)
            }
            (Direction::StagingBackIntoSurface, Contention::ReportContended) => {
                sandbox.try_copy_surface_export_staging_back_to_surface(&staging, surface_id)
            }
        });
    match copied {
        Ok(Some(signalled)) => EscalateResponse::Ok(EscalateResponseOk {
            request_id,
            handle_id: surface_id.to_string(),
            timeline_value: Some(signalled.to_string()),
            ..Default::default()
        }),
        Ok(None) => EscalateResponse::Contended(EscalateResponseContended { request_id }),
        Err(failure) => EscalateResponse::Err(EscalateResponseErr {
            request_id,
            message: format!("{} failed: {failure}", op.escalate_op_name()),
        }),
    }
}

/// Open the CPU-readable staging for `surface_id` on behalf of a helper
/// process — the readback twin of
/// [`handle_open_device_export_staging`], differing only in residency.
///
/// Without this the copies above would land in a buffer no child can
/// reach: the staging is engine-owned, so nothing else publishes it.
#[cfg(target_os = "linux")]
fn handle_open_cpu_readback_staging(
    sandbox: &GpuContextLimitedAccess,
    request_id: String,
    surface_id: &str,
) -> EscalateResponse {
    handle_open_surface_export_staging(
        sandbox,
        request_id,
        surface_id,
        SurfaceExportStagingResidency::HostVisible,
    )
}

/// The binding kind a wire enum names.
#[cfg(target_os = "linux")]
fn compute_binding_kind_from_wire(
    kind: EscalateComputeBindingKind,
) -> crate::core::rhi::ComputeBindingKind {
    use crate::core::rhi::ComputeBindingKind;
    match kind {
        EscalateComputeBindingKind::SampledImage => ComputeBindingKind::SampledImage,
        EscalateComputeBindingKind::SampledTexture => ComputeBindingKind::SampledTexture,
        EscalateComputeBindingKind::StorageBuffer => ComputeBindingKind::StorageBuffer,
        EscalateComputeBindingKind::StorageImage => ComputeBindingKind::StorageImage,
        EscalateComputeBindingKind::UniformBuffer => ComputeBindingKind::UniformBuffer,
    }
}

/// The wire enum for a binding kind.
#[cfg(target_os = "linux")]
fn compute_binding_kind_to_wire(
    kind: crate::core::rhi::ComputeBindingKind,
) -> EscalateComputeBindingKind {
    use crate::core::rhi::ComputeBindingKind;
    match kind {
        ComputeBindingKind::SampledImage => EscalateComputeBindingKind::SampledImage,
        ComputeBindingKind::SampledTexture => EscalateComputeBindingKind::SampledTexture,
        ComputeBindingKind::StorageBuffer => EscalateComputeBindingKind::StorageBuffer,
        ComputeBindingKind::StorageImage => EscalateComputeBindingKind::StorageImage,
        ComputeBindingKind::UniformBuffer => EscalateComputeBindingKind::UniformBuffer,
    }
}

/// Which of the two shader sources a register op supplied for one stage.
///
/// Resolved before escalating. Supplying neither, both, or undecodable hex is
/// a malformed request, and refusing one must not cost a turn of the device
/// gate — the same reason every other `_hex` field is decoded up here.
#[cfg(target_os = "linux")]
enum RegisteredShaderStageSource {
    /// GLSL text, compiled once the handler holds Full access.
    GlslSource {
        source: String,
        stage: GlslCompilationTargetStage,
        entry_point: String,
        field_name: String,
    },
    /// Bytes the caller compiled elsewhere — the escape hatch.
    PreCompiledSpirv {
        spirv: Arc<[u8]>,
        entry_point: String,
    },
}

/// Read one stage's shader out of a register op, without touching the device.
///
/// GLSL source and pre-compiled SPIR-V are alternatives: both is ambiguous
/// about which the caller meant to run, and neither leaves nothing to build.
/// Neither is guessable, so both are named.
#[cfg(target_os = "linux")]
fn registered_shader_stage_source(
    field_prefix: &str,
    source: &str,
    spv_hex: &str,
    stage: GlslCompilationTargetStage,
    entry_point: &str,
) -> std::result::Result<RegisteredShaderStageSource, String> {
    let source_field = format!("{field_prefix}source");
    let spv_field = format!("{field_prefix}spv_hex");
    match (source.is_empty(), spv_hex.is_empty()) {
        (true, true) => Err(format!(
            "neither {source_field} nor {spv_field} was supplied for the {} stage; a kernel is \
             built from GLSL source or from pre-compiled SPIR-V, and one of the two has to \
             be there",
            stage.wire_name()
        )),
        (false, false) => Err(format!(
            "both {source_field} and {spv_field} were supplied for the {} stage; they are \
             alternatives, and which one the kernel should run is not something to guess at",
            stage.wire_name()
        )),
        (false, true) => Ok(RegisteredShaderStageSource::GlslSource {
            source: source.to_string(),
            stage,
            entry_point: normalized_shader_entry_point(entry_point).to_string(),
            field_name: source_field,
        }),
        (true, false) => decode_hex(spv_hex)
            .map(|spv| RegisteredShaderStageSource::PreCompiledSpirv {
                spirv: spv.into(),
                entry_point: normalized_shader_entry_point(entry_point).to_string(),
            })
            .map_err(|e| format!("{spv_field} decode: {e}")),
    }
}

#[cfg(target_os = "linux")]
impl RegisteredShaderStageSource {
    /// The stage's SPIR-V, compiling the GLSL if that is what was supplied.
    ///
    /// Takes the sandbox rather than a `GpuContextFullAccess`, so it needs no
    /// escalate scope and every handler calls it before opening one:
    /// compilation is CPU work that touches no device, and that gate
    /// serializes every processor's device work. A cold C++ compile is
    /// milliseconds no other processor should ever wait on.
    fn spirv(&self, sandbox: &GpuContextLimitedAccess) -> crate::core::error::Result<Arc<[u8]>> {
        match self {
            Self::GlslSource {
                source,
                stage,
                entry_point,
                field_name,
            } => sandbox.host_inner().compile_glsl_shader_source_to_spirv(
                source,
                *stage,
                entry_point,
                field_name,
            ),
            Self::PreCompiledSpirv { spirv, .. } => Ok(Arc::clone(spirv)),
        }
    }

    /// The entry point the pipeline stage is built against — the same value
    /// the module was compiled against, normalized once at resolution.
    fn entry_point(&self) -> &str {
        match self {
            Self::GlslSource { entry_point, .. } | Self::PreCompiledSpirv { entry_point, .. } => {
                entry_point
            }
        }
    }
}

/// An empty entry point on the wire means `main`, the same normalization the
/// graphics and ray-tracing stage fields have always documented.
#[cfg(target_os = "linux")]
fn normalized_shader_entry_point(entry_point: &str) -> &str {
    if entry_point.is_empty() {
        crate::core::rhi::DEFAULT_SHADER_ENTRY_POINT
    } else {
        entry_point
    }
}

/// Build a compute kernel for a subprocess customer, against `GpuContext`.
///
/// Reflection derives the binding shape and its names; the request's own
/// declaration is checked against it rather than replacing it. Re-registering
/// an identical kernel is a cache hit and answers with the same `kernel_id`.
///
/// The shader arrives as GLSL `source` the engine compiles, or as the
/// pre-compiled `spv_hex` escape hatch.
///
/// Failure modes (each an [`EscalateResponse::Err`] keyed by the request_id):
/// 1. Neither `source` nor `spv_hex` supplied, or both.
/// 2. `stage` names something other than `compute`, or nothing that is a
///    stage at all.
/// 3. `source` does not compile, or declares a non-`main` entry point.
/// 4. `spv_hex` doesn't decode as hex bytes.
/// 5. The blob's `OpName` decorations were stripped — bindings resolve by
///    name, so an unnamed binding cannot be bound at all.
/// 6. The declaration disagrees with reflection on a name or a kind.
/// 7. Push-constant size mismatch, or pipeline build failure.
#[cfg(target_os = "linux")]
fn handle_register_compute_kernel(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRegisterComputeKernel,
) -> EscalateResponse {
    // Parsed before it is judged, so a misspelling and a real-but-wrong stage
    // get different answers: one lists the stages that exist, the other says
    // which one this op means.
    if !req.stage.is_empty() {
        match GlslCompilationTargetStage::from_wire_name(&req.stage) {
            Ok(GlslCompilationTargetStage::Compute) => {}
            Ok(other) => {
                return EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: format!(
                        "register_compute_kernel carries stage `{}`; this op registers a \
                         compute kernel, so the only stage it compiles for is `{}`",
                        other.wire_name(),
                        GlslCompilationTargetStage::Compute.wire_name()
                    ),
                });
            }
            Err(e) => {
                return EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: format!("register_compute_kernel: {e}"),
                });
            }
        }
    }

    let shader_source = match registered_shader_stage_source(
        "",
        &req.source,
        &req.spv_hex,
        GlslCompilationTargetStage::Compute,
        &req.entry_point,
    ) {
        Ok(shader_source) => shader_source,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("register_compute_kernel: {e}"),
            });
        }
    };

    let spv = match shader_source.spirv(sandbox) {
        Ok(spv) => spv,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("register_compute_kernel: {e}"),
            });
        }
    };

    let declared: Vec<crate::core::rhi::ComputeBindingDeclaration> = req
        .bindings
        .iter()
        .map(|wire| crate::core::rhi::ComputeBindingDeclaration {
            name: wire.name.clone(),
            kind: compute_binding_kind_from_wire(wire.kind),
        })
        .collect();

    let registered = sandbox
        .escalate(|full| {
            full.create_or_reuse_compute_kernel(
                &spv,
                req.push_constant_size,
                &declared,
                shader_source.entry_point(),
            )
        })
        .and_then(|(kernel_id, kernel)| {
            // The caller dispatches by name and only the shader knows which
            // kind each name is, so the shape goes back with the id. Every
            // binding this kernel holds came through reflection, which refuses
            // an unnamed one — an absent name here is a broken invariant, not
            // a case to skip over.
            let bindings = kernel
                .bindings()
                .iter()
                .map(|spec| {
                    Ok(EscalateResponseComputeBinding {
                        kind: compute_binding_kind_to_wire(spec.kind),
                        name: spec
                            .name
                            .as_deref()
                            .ok_or_else(|| {
                                crate::core::error::Error::GpuError(format!(
                                    "kernel {kernel_id} holds an unnamed binding at slot {}; \
                                     reflection refuses these, so this kernel did not come \
                                     through registration",
                                    spec.binding
                                ))
                            })?
                            .to_string(),
                    })
                })
                .collect::<crate::core::error::Result<Vec<_>>>()?;
            Ok((kernel_id, bindings))
        });

    match registered {
        Ok((kernel_id, bindings)) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id: kernel_id,
            bindings: Some(bindings),
            ..Default::default()
        }),
        Err(e) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("register_compute_kernel failed: {e}"),
        }),
    }
}

/// Dispatch a registered compute kernel with its bindings resolved by name.
///
/// Compute dispatch on the host is synchronous — `VulkanComputeKernel::dispatch`
/// waits on its own fence — so by the time this emits an `Ok`, the GPU work has
/// retired and the writes are visible to any later submission on the same
/// device. The subprocess can advance its surface-share timeline on receipt.
///
/// Every binding error raises here, before anything is submitted, and names the
/// shader's own bindings so the caller can see what it should have supplied.
#[cfg(target_os = "linux")]
fn handle_run_compute_kernel(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRunComputeKernel,
) -> EscalateResponse {
    let push_constants = match decode_hex(&req.push_constants_hex) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("run_compute_kernel: push_constants_hex decode: {e}"),
            });
        }
    };

    let dispatched = sandbox.escalate(|full| {
        let kernel = full.compute_kernel_by_id(&req.kernel_id).ok_or_else(|| {
            crate::core::error::Error::GpuError(format!(
                "run_compute_kernel: no kernel registered under id {:?}",
                req.kernel_id
            ))
        })?;
        bind_and_dispatch_compute_kernel(full, &kernel, &req, &push_constants)
    });

    match dispatched {
        Ok(()) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            // Echo the kernel_id back — compute is sync host-side, no
            // separate handle is allocated per dispatch.
            handle_id: req.kernel_id,
            ..Default::default()
        }),
        Err(e) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("run_compute_kernel failed: {e}"),
        }),
    }
}

#[cfg(target_os = "linux")]
use crate::core::context::{BatchedComputeKernelDispatch, BatchedComputeKernelDispatchBinding};
#[cfg(target_os = "linux")]
use crate::core::rhi::SurfaceBoundComputeBindingKind;

/// What one validated binding resolved to: the slot to write, the kind to
/// write it as, and the surface to look up.
#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
struct PlannedComputeBinding<'a> {
    binding: u32,
    kind: SurfaceBoundComputeBindingKind,
    name: &'a str,
    target_id: &'a str,
}

/// Match a dispatch's supplied bindings against the kernel's declared ones.
///
/// Every failure here is raised before any resource is bound and long before a
/// submission, and every message names the shader's own bindings. Bindings do
/// not persist on a kernel, so a dispatch supplies all of them or none:
///
/// - **duplicate** — one name supplied twice. Not expressible in a Python
///   mapping, which is why this is checked against the wire array rather than
///   left to the caller's language.
/// - **unknown** — a name the shader does not declare.
/// - **missing** — a declared name the dispatch omitted. There is no implicit
///   default and no carried-over value.
/// - **kind mismatch** — a name supplied as a kind the shader disagrees with.
/// - **unbindable kind** — a declared kind no surface can be named for
///   (buffers, samplerless images). Checked here so the plan is total before
///   any `set_*` call mutates the kernel's staged bindings.
#[cfg(target_os = "linux")]
fn plan_supplied_compute_bindings<'a>(
    supplied: &'a [EscalateRequestRunComputeKernelBinding],
    declared: &'a [crate::core::rhi::ComputeBindingSpec],
) -> crate::core::error::Result<Vec<PlannedComputeBinding<'a>>> {
    use crate::core::error::Error;
    use crate::core::rhi::ComputeBindingKind;

    let declared_names: Vec<&str> = declared.iter().filter_map(|s| s.name.as_deref()).collect();
    // Built only when a refusal fires — this runs per frame, and the happy
    // path should not pay for the error text.
    let shader_declares = || crate::core::rhi::quote_declared_shader_binding_names(&declared_names);

    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for wire in supplied {
        if !seen.insert(wire.name.as_str()) {
            return Err(Error::GpuError(format!(
                "binding `{}` was supplied twice; this shader declares {}, each supplied \
                 exactly once per dispatch",
                wire.name,
                shader_declares()
            )));
        }
    }

    for name in &declared_names {
        if !seen.contains(name) {
            return Err(Error::GpuError(format!(
                "binding `{name}` was not supplied; bindings do not persist between dispatches, \
                 so every dispatch supplies all of {}",
                shader_declares()
            )));
        }
    }

    let mut planned = Vec::with_capacity(supplied.len());
    for wire in supplied {
        let spec = declared
            .iter()
            .find(|s| s.name.as_deref() == Some(wire.name.as_str()))
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "binding `{}` is not one this shader declares; it declares {}",
                    wire.name,
                    shader_declares()
                ))
            })?;
        let supplied_kind = compute_binding_kind_from_wire(wire.kind);
        if spec.kind != supplied_kind {
            return Err(Error::GpuError(format!(
                "binding `{}` was supplied as {:?} but this shader declares it {:?}",
                wire.name, supplied_kind, spec.kind
            )));
        }
        let surface_bound_kind = match spec.kind {
            ComputeBindingKind::StorageImage => SurfaceBoundComputeBindingKind::StorageImage,
            ComputeBindingKind::SampledTexture => SurfaceBoundComputeBindingKind::SampledTexture,
            ComputeBindingKind::SampledImage
            | ComputeBindingKind::StorageBuffer
            | ComputeBindingKind::UniformBuffer => {
                return Err(Error::GpuError(format!(
                    "binding `{}` is {:?}, which a dispatch cannot name a surface for — the \
                     surface-backed kinds are storage_image and sampled_texture",
                    wire.name, spec.kind
                )));
            }
        };
        planned.push(PlannedComputeBinding {
            binding: spec.binding,
            kind: surface_bound_kind,
            name: wire.name.as_str(),
            target_id: wire.target_id.as_str(),
        });
    }
    Ok(planned)
}

/// Plan a dispatch's supplied bindings against the kernel, then resolve each
/// one to the device texture it names.
///
/// Shared by the two dispatch paths — one kernel on its own, and a kernel
/// inside a batch — so the extent convention below and the refusal wording
/// have one home rather than two that can drift.
#[cfg(target_os = "linux")]
fn resolve_supplied_compute_bindings(
    full: &crate::core::context::GpuContextFullAccess,
    supplied: &[EscalateRequestRunComputeKernelBinding],
    kernel: &crate::vulkan::rhi::VulkanComputeKernel,
) -> crate::core::error::Result<Vec<BatchedComputeKernelDispatchBinding>> {
    use crate::core::error::Error;

    // Borrowed, not cloned: this runs per frame, and the specs live on the
    // kernel for its whole life.
    let planned = plan_supplied_compute_bindings(supplied, kernel.host_inner().bindings())?;

    let mut resolved = Vec::with_capacity(planned.len());
    for binding in &planned {
        // Zero extent: a kernel binding names a surface the graph already has
        // as a device texture, which resolves from the same-process cache or
        // the surface-share service. The pixel-buffer fallback is the one
        // path that consults the extent, and it refuses a zero one — a
        // buffer-backed surface is not something a dispatch can bind.
        let registration = full
            .resolve_texture_registration_by_surface_id(binding.target_id, None, 0, 0)
            .map_err(|e| {
                Error::GpuError(format!(
                    "binding `{}` names surface {:?}, which this graph cannot resolve to a \
                     device texture: {e}",
                    binding.name, binding.target_id
                ))
            })?;
        resolved.push(BatchedComputeKernelDispatchBinding {
            binding: binding.binding,
            kind: binding.kind,
            registration,
        });
    }
    Ok(resolved)
}

/// Resolve every named binding onto the kernel's slots, then dispatch.
///
/// The plan is total and every surface is resolved before the first `set_*`
/// call, so a refused dispatch never leaves the kernel holding a mix of this
/// dispatch's bindings and the last one's. The kernel's staged bindings are
/// shared across every caller of the cache; interleaving is prevented by the
/// escalate gate, which serializes the whole surrounding scope runtime-wide.
#[cfg(target_os = "linux")]
fn bind_and_dispatch_compute_kernel(
    full: &crate::core::context::GpuContextFullAccess,
    kernel: &crate::vulkan::rhi::VulkanComputeKernel,
    req: &EscalateRequestRunComputeKernel,
    push_constants: &[u8],
) -> crate::core::error::Result<()> {
    // Held across the dispatch, not consumed by the bind loop: a registration
    // is a refcount on the texture the descriptor set now points at, and
    // dropping the last one before the GPU has run frees the image out from
    // under it.
    let resolved = resolve_supplied_compute_bindings(full, &req.bindings, kernel)?;
    for binding in &resolved {
        binding.write_into_kernel(kernel)?;
    }

    // A kernel that declares push constants must be given them even when the
    // payload is empty, so `set_push_constants` produces the size mismatch
    // rather than the dispatch running against whatever the kernel's staged
    // buffer last held.
    if kernel.push_constant_size() > 0 || !push_constants.is_empty() {
        kernel.set_push_constants(push_constants)?;
    }

    let dispatched = kernel.dispatch(req.group_count_x, req.group_count_y, req.group_count_z);
    drop(resolved);
    dispatched
}

/// Run several dispatches as one recording: one submission, one fence wait.
///
/// The op exists because per-dispatch blocking is what a multi-pass filter
/// would otherwise pay N times over — `run_compute_kernel` submits and waits
/// every time. Here the caller pays once, and still returns with every write
/// visible, so nothing about the synchronous contract changes.
///
/// Every refusal — a decode, an unknown kernel, a binding that does not match
/// the shader, a surface this graph cannot resolve, one kernel named twice —
/// raises before the recording opens or aborts it, so a batch either runs
/// whole or submits nothing.
#[cfg(target_os = "linux")]
fn handle_run_compute_kernel_batch(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRunComputeKernelBatch,
) -> EscalateResponse {
    let mut push_constants_per_dispatch = Vec::with_capacity(req.dispatches.len());
    for (index, dispatch) in req.dispatches.iter().enumerate() {
        match decode_hex(&dispatch.push_constants_hex) {
            Ok(bytes) => push_constants_per_dispatch.push(bytes),
            Err(e) => {
                return EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: format!(
                        "run_compute_kernel_batch: dispatch {index}: push_constants_hex decode: {e}"
                    ),
                });
            }
        }
    }

    let dispatched = sandbox.escalate(move |full| {
        bind_and_dispatch_compute_kernel_batch(full, &req, push_constants_per_dispatch)
    });

    match dispatched {
        Ok(()) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            ..Default::default()
        }),
        Err(e) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("run_compute_kernel_batch failed: {e}"),
        }),
    }
}

/// Resolve every dispatch in a batch, then hand the lot to the recorder.
///
/// Resolution is complete before the first barrier is recorded: the same rule
/// the single-dispatch path follows, for the same reason — a refusal while a
/// command buffer is open costs an abort, and a partially-recorded batch is
/// not something a caller asked for.
#[cfg(target_os = "linux")]
fn bind_and_dispatch_compute_kernel_batch(
    full: &crate::core::context::GpuContextFullAccess,
    req: &EscalateRequestRunComputeKernelBatch,
    push_constants_per_dispatch: Vec<Vec<u8>>,
) -> crate::core::error::Result<()> {
    use crate::core::error::Error;

    let mut batch = Vec::with_capacity(req.dispatches.len());
    for ((index, dispatch), push_constants) in req
        .dispatches
        .iter()
        .enumerate()
        .zip(push_constants_per_dispatch)
    {
        let kernel = full
            .compute_kernel_by_id(&dispatch.kernel_id)
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "dispatch {index} of this batch names no kernel registered under id {:?}",
                    dispatch.kernel_id
                ))
            })?;
        let bindings = resolve_supplied_compute_bindings(full, &dispatch.bindings, &kernel)
            .map_err(|e| Error::GpuError(format!("dispatch {index} of this batch: {e}")))?;
        batch.push(BatchedComputeKernelDispatch {
            kernel,
            bindings,
            push_constants,
            group_count_x: dispatch.group_count_x,
            group_count_y: dispatch.group_count_y,
            group_count_z: dispatch.group_count_z,
        });
    }

    full.dispatch_compute_kernel_batch(&batch)
}

/// Map a wire-format `register_graphics_kernel` request through the
/// registered [`GraphicsKernelBridge`].
///
/// Resolves each stage's shader — GLSL source the engine compiles, or the
/// pre-compiled hex escape hatch — translates the wire-format
/// pipeline-state enums into the bridge's typed [`GraphicsPipelineStateWire`],
/// and asks the bridge to register the kernel. The bridge returns a
/// stable `kernel_id` (recommended: SHA-256 over a canonical
/// representation of all register-time inputs); identical re-registration
/// hits the bridge's cache and returns the same id.
///
/// Failure modes (each surfaced as an [`EscalateResponse::Err`] keyed
/// by the original request_id):
/// 1. A stage supplies neither `*_source` nor `*_spv_hex`, or both; its
///    `*_source` does not compile; or its `*_spv_hex` doesn't decode.
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

    let stage_sources = registered_shader_stage_source(
        "vertex_",
        &req.vertex_source,
        &req.vertex_spv_hex,
        GlslCompilationTargetStage::Vertex,
        &req.vertex_entry_point,
    )
    .and_then(|vertex| {
        registered_shader_stage_source(
            "fragment_",
            &req.fragment_source,
            &req.fragment_spv_hex,
            GlslCompilationTargetStage::Fragment,
            &req.fragment_entry_point,
        )
        .map(|fragment| (vertex, fragment))
    });
    let (vertex_source, fragment_source) = match stage_sources {
        Ok(stage_sources) => stage_sources,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("register_graphics_kernel: {e}"),
            });
        }
    };

    let compiled = vertex_source
        .spirv(sandbox)
        .and_then(|vertex_spv| Ok((vertex_spv, fragment_source.spirv(sandbox)?)));
    let (vertex_spv, fragment_spv) = match compiled {
        Ok(compiled) => compiled,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("register_graphics_kernel: {e}"),
            });
        }
    };

    // Not re-prefixed with the op: this payload already opens with it, and
    // `Error::Configuration` adds its own "Invalid configuration:" on top.
    let bridge: Arc<dyn GraphicsKernelBridge> = match sandbox.escalate(|full| {
        full.graphics_kernel_bridge().ok_or_else(|| {
            crate::core::error::Error::Configuration(
                "register_graphics_kernel: no GraphicsKernelBridge registered on GpuContext"
                    .to_string(),
            )
        })
    }) {
        Ok(bridge) => bridge,
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
        vertex_spv: vertex_spv.to_vec(),
        fragment_spv: fragment_spv.to_vec(),
        vertex_entry_point: vertex_source.entry_point().to_string(),
        fragment_entry_point: fragment_source.entry_point().to_string(),
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
            ..Default::default()
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
            crate::core::error::Error::Configuration(
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
            ..Default::default()
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
                message: format!("register_acceleration_structure_blas: vertices_hex decode: {e}"),
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
                message: format!("register_acceleration_structure_blas: indices_hex decode: {e}"),
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
            crate::core::error::Error::Configuration(
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
            ..Default::default()
        }),
        Err(msg) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("register_acceleration_structure_blas bridge call failed: {msg}"),
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
/// 2. Instance `mask` exceeds 0xff (the wire form is a u32).
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

    if req.instances.is_empty() {
        return EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: "register_acceleration_structure_tlas: instances must not be empty (TLAS \
                 requires at least one instance per Vulkan spec)"
                .to_string(),
        });
    }
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
            crate::core::error::Error::Configuration(
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
            ..Default::default()
        }),
        Err(msg) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("register_acceleration_structure_tlas bridge call failed: {msg}"),
        }),
    }
}

/// The compiler's name for a ray-tracing wire stage.
///
/// Distinct from [`ray_tracing_stage_from_wire`], which maps the same wire
/// value to the bridge's stage vocabulary: one names a pipeline stage to
/// compile for, the other names a stage to build a shader group from.
#[cfg(target_os = "linux")]
fn ray_tracing_pipeline_stage_from_wire(
    stage: EscalateRequestRegisterRayTracingKernelStageStage,
) -> crate::core::rhi::GlslCompilationTargetStage {
    use crate::core::rhi::GlslCompilationTargetStage as Compiled;
    use EscalateRequestRegisterRayTracingKernelStageStage as Wire;
    match stage {
        Wire::AnyHit => Compiled::RayAnyHit,
        Wire::Callable => Compiled::RayCallable,
        Wire::ClosestHit => Compiled::RayClosestHit,
        Wire::Intersection => Compiled::RayIntersection,
        Wire::Miss => Compiled::RayMiss,
        Wire::RayGen => Compiled::RayGeneration,
    }
}

/// Map a wire-format `register_ray_tracing_kernel` request through
/// the registered [`RayTracingKernelBridge`].
///
/// Resolves each stage's shader — GLSL source the engine compiles, or the
/// pre-compiled hex escape hatch — translates the wire-format
/// stage / group / binding kinds into the bridge's typed mirrors, and
/// asks the bridge to register the kernel. The bridge returns a
/// stable `kernel_id` (typically SHA-256 over a canonical
/// representation of all register-time inputs); identical
/// re-registration hits the bridge's cache and returns the same id.
///
/// Failure modes (each surfaced as an [`EscalateResponse::Err`] keyed
/// by the original request_id):
/// 1. Any stage supplies neither `source` nor `spv_hex`, or both; its
///    `source` does not compile; or its `spv_hex` doesn't decode.
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

    // One consuming pass pairs each stage's shader with the bridge stage it
    // fills, so nothing downstream has to keep two vectors index-aligned.
    let mut stage_sources = Vec::with_capacity(req.stages.len());
    for (idx, st) in req.stages.into_iter().enumerate() {
        match registered_shader_stage_source(
            &format!("stages[{idx}]."),
            &st.source,
            &st.spv_hex,
            ray_tracing_pipeline_stage_from_wire(st.stage),
            &st.entry_point,
        ) {
            Ok(stage_source) => {
                stage_sources.push((stage_source, ray_tracing_stage_from_wire(st.stage)));
            }
            Err(e) => {
                return EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: format!("register_ray_tracing_kernel: {e}"),
                });
            }
        }
    }

    let resolved_stages = stage_sources
        .iter()
        .map(|(stage_source, bridge_stage)| {
            Ok(RayTracingStageDecl {
                stage: *bridge_stage,
                spv: stage_source.spirv(sandbox)?.to_vec(),
                entry_point: stage_source.entry_point().to_string(),
            })
        })
        .collect::<crate::core::error::Result<Vec<_>>>();
    let stages = match resolved_stages {
        Ok(stages) => stages,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("register_ray_tracing_kernel: {e}"),
            });
        }
    };

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
            crate::core::error::Error::Configuration(
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
            ..Default::default()
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
            crate::core::error::Error::Configuration(
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
            ..Default::default()
        }),
        Err(msg) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("run_ray_tracing_kernel bridge call failed: {msg}"),
        }),
    }
}

/// Convert a sentinel-encoded wire stage index back into an
/// `Option<u32>`. The wire form uses `0xFFFFFFFF` to mean "absent"
/// because the field is always present on the wire.
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
    let color_blend_src_color_factor =
        blend_factor_from_wire_src_color(p.color_blend_src_color_factor);
    let color_blend_dst_color_factor =
        blend_factor_from_wire_dst_color(p.color_blend_dst_color_factor);
    let color_blend_color_op = blend_op_from_wire_color(p.color_blend_color_op);
    let color_blend_src_alpha_factor =
        blend_factor_from_wire_src_alpha(p.color_blend_src_alpha_factor);
    let color_blend_dst_alpha_factor =
        blend_factor_from_wire_dst_alpha(p.color_blend_dst_alpha_factor);
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
    blend_factor_match!(
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor,
        f
    )
}
#[cfg(target_os = "linux")]
fn blend_factor_from_wire_dst_color(
    f: EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor,
) -> BlendFactorWire {
    blend_factor_match!(
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor,
        f
    )
}
#[cfg(target_os = "linux")]
fn blend_factor_from_wire_src_alpha(
    f: EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor,
) -> BlendFactorWire {
    blend_factor_match!(
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor,
        f
    )
}
#[cfg(target_os = "linux")]
fn blend_factor_from_wire_dst_alpha(
    f: EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor,
) -> BlendFactorWire {
    blend_factor_match!(
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor,
        f
    )
}
#[cfg(target_os = "linux")]
fn blend_op_from_wire_color(
    o: EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp,
) -> BlendOpWire {
    blend_op_match!(
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp,
        o
    )
}
#[cfg(target_os = "linux")]
fn blend_op_from_wire_alpha(
    o: EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp,
) -> BlendOpWire {
    blend_op_match!(
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp,
        o
    )
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

/// Parse a wire-format texture format string into a [`TextureFormat`].
///
/// Lowercase snake-case matches the variant name. A separate vocabulary
/// from [`PixelFormat::parse_wire_name`] — pixel formats include
/// video-specific YUV variants that textures don't expose, and texture
/// formats include float variants that pixel buffers don't.
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
        .ok_or_else(|| Error::Runtime("not an escalate_request".to_string()))?
        .map_err(|e| Error::Runtime(e.message))
}

/// SPIR-V's magic number, little-endian — the cheapest proof that what reached
/// a bridge is a module rather than the source text.
#[cfg(all(test, target_os = "linux"))]
const SPIRV_MAGIC_LE: [u8; 4] = 0x0723_0203u32.to_le_bytes();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pixel_format_accepts_common_aliases() {
        assert_eq!(
            PixelFormat::parse_wire_name("bgra"),
            Ok(PixelFormat::Bgra32)
        );
        assert_eq!(
            PixelFormat::parse_wire_name("BGRA32"),
            Ok(PixelFormat::Bgra32)
        );
        assert_eq!(
            PixelFormat::parse_wire_name("nv12"),
            Ok(PixelFormat::Nv12VideoRange)
        );
        assert_eq!(
            PixelFormat::parse_wire_name("nv12_full_range"),
            Ok(PixelFormat::Nv12FullRange)
        );
        assert_eq!(
            PixelFormat::parse_wire_name("gray8"),
            Ok(PixelFormat::Gray8)
        );
    }

    #[test]
    fn parse_pixel_format_rejects_unknown() {
        assert!(PixelFormat::parse_wire_name("xyz").is_err());
    }

    #[test]
    fn decode_hex_round_trips_lowercase_and_mixed_case() {
        assert_eq!(decode_hex("").unwrap(), Vec::<u8>::new());
        assert_eq!(decode_hex("00").unwrap(), vec![0u8]);
        assert_eq!(decode_hex("ff").unwrap(), vec![0xff]);
        assert_eq!(
            decode_hex("DeAdBeEf").unwrap(),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
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
    fn try_parse_accepts_the_surface_export_staging_ops() {
        for (op_name, expected_variant) in [
            ("open_device_export_staging", "open"),
            ("open_cpu_readback_staging", "open cpu readback"),
            ("refill_device_export_staging", "refill"),
            ("copy_device_export_staging_back_to_surface", "publish"),
        ] {
            let msg = serde_json::json!({
                "rpc": "escalate_request",
                "op": op_name,
                "request_id": "r-device",
                "surface_id": "surface-7",
            });
            let op = parse_op_for_tests(&msg)
                .unwrap_or_else(|failure| panic!("{op_name} decodes: {failure}"));
            let seen = match &op {
                EscalateRequest::OpenDeviceExportStaging(p) => {
                    assert_eq!(p.surface_id, "surface-7");
                    "open"
                }
                EscalateRequest::OpenCpuReadbackStaging(p) => {
                    assert_eq!(p.surface_id, "surface-7");
                    "open cpu readback"
                }
                EscalateRequest::RefillDeviceExportStaging(p) => {
                    assert_eq!(p.surface_id, "surface-7");
                    "refill"
                }
                EscalateRequest::CopyDeviceExportStagingBackToSurface(p) => {
                    assert_eq!(p.surface_id, "surface-7");
                    "publish"
                }
                _ => panic!("{op_name} decoded as the wrong variant"),
            };
            assert_eq!(seen, expected_variant);
            // Every one is request/response: a device export that lost
            // its reply would leave the child waiting on a deadline.
            assert_eq!(request_id(&op), Some("r-device"));
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
        // This locks the `op` discriminator tag — the actual "bridge does not
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
            ..Default::default()
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
        let usage = parse_texture_usages(&["texture_binding".to_string(), "copy_src".to_string()])
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

    /// The readback ops answer from `GpuContext` with nothing installed:
    /// no bridge, no application glue, no runtime-absent case.
    ///
    /// These run without a real surface, so they assert the shape of the
    /// refusal rather than a landed copy — an unresolvable surface is an
    /// error naming the surface, never the missing-bridge refusal the
    /// deleted seam used to answer to every caller. The copy itself is
    /// proven over a real device in
    /// `surface_export_staging`'s own GPU-gated tests.
    #[cfg(target_os = "linux")]
    mod cpu_readback_answers_from_gpu_context {
        use super::*;
        use crate::core::context::{GpuContext, GpuContextLimitedAccess};

        fn sandbox_or_skip(test_name: &str) -> Option<GpuContextLimitedAccess> {
            gpu_or_skip(test_name).map(GpuContextLimitedAccess::new)
        }

        fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
            match GpuContext::init_for_platform_sync() {
                Ok(gpu) => Some(gpu),
                Err(_) => {
                    println!("{test_name}: no GPU device — skipping");
                    None
                }
            }
        }

        /// Every readback op names the surface it could not resolve, and
        /// none of them mentions an installation step.
        #[test]
        fn an_unresolvable_surface_is_refused_by_name_and_never_by_a_missing_bridge() {
            let Some(sandbox) = sandbox_or_skip(
                "an_unresolvable_surface_is_refused_by_name_and_never_by_a_missing_bridge",
            ) else {
                return;
            };
            let registry = EscalateHandleRegistry::new();

            let requests = [
                EscalateRequest::RunCpuReadbackCopy(EscalateRequestRunCpuReadbackCopy {
                    request_id: "req-run".into(),
                    surface_id: "no-such-surface".into(),
                    direction: EscalateRequestRunCpuReadbackCopyDirection::ImageToBuffer,
                }),
                EscalateRequest::TryRunCpuReadbackCopy(EscalateRequestTryRunCpuReadbackCopy {
                    request_id: "req-try".into(),
                    surface_id: "no-such-surface".into(),
                    direction: EscalateRequestTryRunCpuReadbackCopyDirection::BufferToImage,
                }),
                EscalateRequest::OpenCpuReadbackStaging(EscalateRequestOpenCpuReadbackStaging {
                    request_id: "req-open".into(),
                    surface_id: "no-such-surface".into(),
                }),
            ];

            for request in requests {
                let expected_request_id = request_id(&request)
                    .expect("every readback op carries a correlation token")
                    .to_string();
                let response = handle_escalate_op(&sandbox, &registry, request)
                    .expect("every readback op produces a response");
                match response {
                    EscalateResponse::Err(err) => {
                        assert_eq!(err.request_id, expected_request_id);
                        assert!(
                            err.message.contains("no-such-surface"),
                            "{expected_request_id}: the refusal must name the surface, got: {}",
                            err.message
                        );
                        assert!(
                            !err.message.contains("Bridge"),
                            "{expected_request_id}: the capability is always present, so no \
                             refusal may cite a bridge; got: {}",
                            err.message
                        );
                    }
                    other => panic!("{expected_request_id}: expected Err, got {other:?}"),
                }
            }
        }

        /// The seam carries a real frame's pixels into CPU-readable
        /// memory: seed a pool frame, drive `run_cpu_readback_copy`
        /// through `handle_escalate_op`, read the staging's mapping.
        ///
        /// This is what pins the op's mappings. Swap the residency to
        /// `DeviceLocal` and the mapping is null; swap the direction to
        /// `StagingBackIntoSurface` and the staging never receives the
        /// frame. Both fail here.
        /// GPU-gated: skips when no device is present.
        #[test]
        fn the_seam_lands_a_frames_pixels_in_cpu_readable_memory() {
            const SEEDED: u8 = 0x3c;
            let Some(gpu) = gpu_or_skip("the_seam_lands_a_frames_pixels_in_cpu_readable_memory")
            else {
                return;
            };
            let (pool_id, pooled_backing) = gpu
                .acquire_pixel_buffer(64, 64, PixelFormat::Rgba32)
                .expect("acquire a frame to read back");
            let surface_id = pool_id.to_string();
            let plane = pooled_backing.plane_base_address(0);
            assert!(!plane.is_null(), "the pooled backing must be host-mapped");
            unsafe { std::ptr::write_bytes(plane, SEEDED, pooled_backing.plane_size(0) as usize) };

            let sandbox = GpuContextLimitedAccess::new(gpu.clone());
            let registry = EscalateHandleRegistry::new();
            let response = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::RunCpuReadbackCopy(EscalateRequestRunCpuReadbackCopy {
                    request_id: "req-seam-read".into(),
                    surface_id: surface_id.clone(),
                    direction: EscalateRequestRunCpuReadbackCopyDirection::ImageToBuffer,
                }),
            )
            .expect("run_cpu_readback_copy always produces a response");
            let EscalateResponse::Ok(ok) = response else {
                panic!("expected Ok, got {response:?}");
            };
            assert_eq!(ok.request_id, "req-seam-read");
            assert!(
                ok.timeline_value.is_some(),
                "the child waits on the timeline value this op answers with"
            );

            let staging = gpu
                .surface_export_staging(
                    &surface_id,
                    crate::core::context::SurfaceExportStagingResidency::HostVisible,
                )
                .expect("the op minted the staging the child would check out");
            let mapped = staging.staging_buffer().mapped_ptr();
            assert!(
                !mapped.is_null(),
                "the op must stage into memory a CPU consumer can map"
            );
            let staged =
                unsafe { std::slice::from_raw_parts(mapped, staging.staging_byte_size() as usize) };
            assert!(
                staged.iter().all(|byte| *byte == SEEDED),
                "the staging must carry the seeded frame; first mismatch at {:?}",
                staged.iter().position(|byte| *byte != SEEDED)
            );
        }

        /// `try_` answers `contended` through the seam while another copy
        /// holds the staging's recorder — the only thing that response
        /// means now. Pins the contention mapping: were `TryRun` treated
        /// as blocking, this would hang rather than answer.
        /// GPU-gated: skips when no device is present.
        #[test]
        fn the_seam_answers_contended_while_the_recorder_is_held() {
            let Some(gpu) = gpu_or_skip("the_seam_answers_contended_while_the_recorder_is_held")
            else {
                return;
            };
            let (pool_id, _pooled_backing) = gpu
                .acquire_pixel_buffer(64, 64, PixelFormat::Rgba32)
                .expect("acquire a frame to read back");
            let surface_id = pool_id.to_string();
            let staging = gpu
                .surface_export_staging(
                    &surface_id,
                    crate::core::context::SurfaceExportStagingResidency::HostVisible,
                )
                .expect("open the staging so the test can hold its recorder");

            let sandbox = GpuContextLimitedAccess::new(gpu.clone());
            let registry = EscalateHandleRegistry::new();
            // The recorder is held on *another* thread: held on this one, a
            // mapping that wrongly waits would re-lock the same
            // non-reentrant mutex and deadlock, turning the mutation this
            // test exists to catch into an unbounded hang instead of a
            // failure.
            let recorder_is_held = std::sync::Arc::new(std::sync::Barrier::new(2));
            let release_the_recorder = std::sync::Arc::new(std::sync::Barrier::new(2));
            let holder = {
                let staging = std::sync::Arc::clone(&staging);
                let recorder_is_held = std::sync::Arc::clone(&recorder_is_held);
                let release_the_recorder = std::sync::Arc::clone(&release_the_recorder);
                std::thread::spawn(move || {
                    staging.while_holding_the_refill_recorder_for_a_test(|| {
                        recorder_is_held.wait();
                        release_the_recorder.wait();
                    })
                })
            };
            recorder_is_held.wait();
            let response = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::TryRunCpuReadbackCopy(EscalateRequestTryRunCpuReadbackCopy {
                    request_id: "req-seam-try".into(),
                    surface_id: surface_id.clone(),
                    direction: EscalateRequestTryRunCpuReadbackCopyDirection::ImageToBuffer,
                }),
            )
            .expect("try_run_cpu_readback_copy always produces a response");
            release_the_recorder.wait();
            holder.join().expect("the recorder holder thread joins");
            match response {
                EscalateResponse::Contended(contended) => {
                    assert_eq!(contended.request_id, "req-seam-try");
                }
                other => panic!("expected Contended, got {other:?}"),
            }
        }

        /// `buffer_to_image` through the seam refuses to publish a
        /// staging nothing has read a frame into. Pins the direction
        /// mapping: `image_to_buffer` would have succeeded here.
        /// GPU-gated: skips when no device is present.
        #[test]
        fn the_seam_refuses_to_publish_a_staging_no_frame_was_read_into() {
            let Some(gpu) =
                gpu_or_skip("the_seam_refuses_to_publish_a_staging_no_frame_was_read_into")
            else {
                return;
            };
            let (pool_id, _pooled_backing) = gpu
                .acquire_pixel_buffer(64, 64, PixelFormat::Rgba32)
                .expect("acquire a frame");
            let sandbox = GpuContextLimitedAccess::new(gpu.clone());
            let registry = EscalateHandleRegistry::new();

            let response = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::RunCpuReadbackCopy(EscalateRequestRunCpuReadbackCopy {
                    request_id: "req-seam-write".into(),
                    surface_id: pool_id.to_string(),
                    direction: EscalateRequestRunCpuReadbackCopyDirection::BufferToImage,
                }),
            )
            .expect("run_cpu_readback_copy always produces a response");
            match response {
                EscalateResponse::Err(err) => assert!(
                    err.message.contains("never been read into"),
                    "publishing an unread staging must be refused by name, got: {}",
                    err.message
                ),
                other => panic!("expected Err, got {other:?}"),
            }
        }

        /// The `try_` publish direction, driven to success through the
        /// seam: read a frame in, edit the mapping, publish it back.
        ///
        /// The only coverage the try_-publish path has. Swap
        /// `TryRun + BufferToImage` to `SurfaceIntoStaging` and the edit is
        /// silently discarded and overwritten by the frame — which this
        /// catches, and nothing else did.
        /// GPU-gated: skips when no device is present.
        #[test]
        fn the_seam_publishes_a_staged_edit_through_the_try_direction() {
            const EDIT: u8 = 0x6b;
            let Some(gpu) =
                gpu_or_skip("the_seam_publishes_a_staged_edit_through_the_try_direction")
            else {
                return;
            };
            let (pool_id, pooled_backing) = gpu
                .acquire_pixel_buffer(64, 64, PixelFormat::Rgba32)
                .expect("acquire a pool-only frame");
            let surface_id = pool_id.to_string();
            let sandbox = GpuContextLimitedAccess::new(gpu.clone());
            let registry = EscalateHandleRegistry::new();

            // Read the frame in — the write-back's precondition.
            let read = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::TryRunCpuReadbackCopy(EscalateRequestTryRunCpuReadbackCopy {
                    request_id: "req-try-read".into(),
                    surface_id: surface_id.clone(),
                    direction: EscalateRequestTryRunCpuReadbackCopyDirection::ImageToBuffer,
                }),
            )
            .expect("try_run_cpu_readback_copy always produces a response");
            assert!(
                matches!(read, EscalateResponse::Ok(_)),
                "an uncontended read must succeed, got {read:?}"
            );

            let staging = gpu
                .surface_export_staging(
                    &surface_id,
                    crate::core::context::SurfaceExportStagingResidency::HostVisible,
                )
                .expect("the staging the read minted");
            let mapped = staging.staging_buffer().mapped_ptr();
            assert!(!mapped.is_null(), "a host-visible staging must be mapped");
            unsafe { std::ptr::write_bytes(mapped, EDIT, staging.staging_byte_size() as usize) };

            let published = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::TryRunCpuReadbackCopy(EscalateRequestTryRunCpuReadbackCopy {
                    request_id: "req-try-publish".into(),
                    surface_id: surface_id.clone(),
                    direction: EscalateRequestTryRunCpuReadbackCopyDirection::BufferToImage,
                }),
            )
            .expect("try_run_cpu_readback_copy always produces a response");
            let EscalateResponse::Ok(ok) = published else {
                panic!("expected Ok, got {published:?}");
            };
            assert!(
                ok.timeline_value.is_some(),
                "a publish answers with the timeline value the child waits on"
            );

            let plane = pooled_backing.plane_base_address(0);
            let backing =
                unsafe { std::slice::from_raw_parts(plane, pooled_backing.plane_size(0) as usize) };
            assert!(
                backing.iter().all(|byte| *byte == EDIT),
                "the edit must reach the pooled backing; first mismatch at {:?}",
                backing.iter().position(|byte| *byte != EDIT)
            );
        }

        /// The open op names itself, not its device-export twin — the two
        /// share one handler and differ only by residency, so a swapped
        /// mapping would surface here as the wrong op in the message.
        /// GPU-gated: skips when no device is present.
        #[test]
        fn the_open_op_names_itself_and_not_its_device_export_twin() {
            let Some(gpu) = gpu_or_skip("the_open_op_names_itself_and_not_its_device_export_twin")
            else {
                return;
            };
            let (pool_id, _pooled_backing) = gpu
                .acquire_pixel_buffer(64, 64, PixelFormat::Rgba32)
                .expect("acquire a frame");
            let sandbox = GpuContextLimitedAccess::new(gpu.clone());
            let registry = EscalateHandleRegistry::new();

            // No surface-share service in a bare context, so the publish
            // step refuses — which is the step whose op name is under test.
            let response = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::OpenCpuReadbackStaging(EscalateRequestOpenCpuReadbackStaging {
                    request_id: "req-seam-open".into(),
                    surface_id: pool_id.to_string(),
                }),
            )
            .expect("open_cpu_readback_staging always produces a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert!(
                        err.message.starts_with("open_cpu_readback_staging"),
                        "the refusal must name this op, got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err without a surface-share service, got {other:?}"),
            }
        }

        /// A surface id that is not a decimal `u64` is no longer special.
        ///
        /// The deleted bridge keyed its own registry by `u64` and parsed
        /// the id before doing anything else, which would refuse every
        /// modern id: a published frame is `<slot>#<generation>`. The
        /// engine resolves the id it was given.
        #[test]
        fn a_non_numeric_surface_id_is_not_a_parse_error() {
            let Some(sandbox) = sandbox_or_skip("a_non_numeric_surface_id_is_not_a_parse_error")
            else {
                return;
            };
            let registry = EscalateHandleRegistry::new();

            let response = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::RunCpuReadbackCopy(EscalateRequestRunCpuReadbackCopy {
                    request_id: "req-frame-id".into(),
                    surface_id: "pool-slot-7#3".into(),
                    direction: EscalateRequestRunCpuReadbackCopyDirection::ImageToBuffer,
                }),
            )
            .expect("run_cpu_readback_copy always produces a response");
            match response {
                EscalateResponse::Err(err) => assert!(
                    !err.message.contains("u64"),
                    "a per-frame surface id must not be refused as a malformed integer, got: {}",
                    err.message
                ),
                other => panic!("expected Err, got {other:?}"),
            }
        }
    }

    /// Compute-kernel handler tests.
    ///
    /// The named-binding cases are the point: a dispatch supplies every
    /// binding the shader declares, by the shader's own name, exactly once.
    /// They run against the binding planner directly rather than through a
    /// GPU, because that is the layer the rules live at — and because
    /// **duplicate is not expressible in a Python mapping**, so the wire
    /// array is the only place it can be tested at all.
    #[cfg(target_os = "linux")]
    mod compute_kernel_dispatch {
        use super::super::*;
        use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestRegisterComputeKernelBinding;
        use crate::core::context::GpuContext;
        use crate::core::rhi::ComputeBindingSpec;
        use crate::host_rhi::HostTextureExt;

        /// Compute is an always-present capability now, so there is no bridge
        /// to install — only a device to have or not have.
        fn make_gpu_sandbox_if_available() -> Option<GpuContextLimitedAccess> {
            GpuContext::init_for_platform_sync()
                .ok()
                .map(GpuContextLimitedAccess::new)
        }

        /// The GLSL the wire now carries instead of bytes: the same
        /// read-one-write-another pass, as an author would write it.
        const READ_ONE_WRITE_ANOTHER_GLSL: &str = "\
#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0) uniform sampler2D source_image;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D output_image;
void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    imageStore(output_image, at, texelFetch(source_image, at, 0));
}
";

        fn register_from_glsl(source: &str, stage: &str) -> EscalateRequestRegisterComputeKernel {
            EscalateRequestRegisterComputeKernel {
                bindings: Vec::new(),
                push_constant_size: 0,
                request_id: "rid-glsl".to_string(),
                source: source.to_string(),
                stage: stage.to_string(),
                entry_point: String::new(),
                spv_hex: String::new(),
            }
        }

        fn refusal_message(response: EscalateResponse) -> String {
            match response {
                EscalateResponse::Err(err) => err.message,
                other => panic!("expected Err, got {other:?}"),
            }
        }

        /// Neither and both are the two ways to get the alternatives wrong, and
        /// each names the pair rather than picking one. Pure wire validation —
        /// no device, so it runs everywhere CI does.
        #[test]
        fn a_register_op_supplying_neither_source_nor_spirv_is_refused_naming_both() {
            let message =
                registered_shader_stage_source("", "", "", GlslCompilationTargetStage::Compute, "")
                    .err()
                    .expect("a register op with no shader at all must be refused");
            assert!(message.contains("source"), "{message}");
            assert!(message.contains("spv_hex"), "{message}");
        }

        #[test]
        fn a_register_op_supplying_both_source_and_spirv_is_refused_naming_both() {
            let message = registered_shader_stage_source(
                "vertex_",
                READ_ONE_WRITE_ANOTHER_GLSL,
                "0badc0de",
                GlslCompilationTargetStage::Vertex,
                "",
            )
            .err()
            .expect("supplying both alternatives must be refused");
            assert!(message.contains("vertex_source"), "{message}");
            assert!(message.contains("vertex_spv_hex"), "{message}");
        }

        /// A stage that disagrees with the op it arrived on is a caller
        /// mistake, and the refusal has to name the only stage this op means.
        #[test]
        fn a_compute_register_op_carrying_another_stage_is_refused() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("compute register op stage mismatch: no GPU — skipping");
                return;
            };
            let message = refusal_message(handle_register_compute_kernel(
                &sandbox,
                "rid-glsl".to_string(),
                register_from_glsl(READ_ONE_WRITE_ANOTHER_GLSL, "vertex"),
            ));
            assert!(message.contains("vertex"), "{message}");
            assert!(message.contains("compute"), "{message}");
        }

        /// A misspelling and a real-but-wrong stage are different mistakes and
        /// get different answers — the first needs the list of stages that
        /// exist, the second needs to know which one this op means.
        #[test]
        fn a_stage_that_is_not_a_stage_at_all_is_refused_naming_the_ones_that_are() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("compute register op unknown stage: no GPU — skipping");
                return;
            };
            let message = refusal_message(handle_register_compute_kernel(
                &sandbox,
                "rid-glsl".to_string(),
                register_from_glsl(READ_ONE_WRITE_ANOTHER_GLSL, "commpute"),
            ));
            assert!(message.contains("commpute"), "{message}");
            for stage in GlslCompilationTargetStage::ALL {
                assert!(message.contains(stage.wire_name()), "{message}");
            }
        }

        /// The ticket's demo, at the wire: GLSL text where bytes used to go,
        /// with the binding names reflection found handed back.
        #[test]
        fn a_glsl_source_registers_a_kernel_and_reports_its_binding_names() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("register from GLSL: no GPU — skipping");
                return;
            };
            let response = handle_register_compute_kernel(
                &sandbox,
                "rid-glsl".to_string(),
                register_from_glsl(READ_ONE_WRITE_ANOTHER_GLSL, "compute"),
            );
            let EscalateResponse::Ok(ok) = response else {
                panic!("expected Ok, got {response:?}");
            };
            let names: Vec<String> = ok
                .bindings
                .expect("a registered kernel reports its binding shape")
                .into_iter()
                .map(|binding| binding.name)
                .collect();
            assert!(
                names.contains(&"source_image".to_string())
                    && names.contains(&"output_image".to_string()),
                "expected the shader\'s own binding names, got {names:?}"
            );
        }

        /// Re-registering the same source costs no second compilation — the
        /// assertion counts compiler invocations, never elapsed time, because
        /// re-creation is free of compilation while still allocating handles.
        #[test]
        fn registering_the_same_glsl_twice_compiles_it_once() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("GLSL compile cache: no GPU — skipping");
                return;
            };
            let before = sandbox.host_inner().glsl_shader_compiler_invocation_count();
            for _ in 0..2 {
                let response = handle_register_compute_kernel(
                    &sandbox,
                    "rid-glsl".to_string(),
                    register_from_glsl(READ_ONE_WRITE_ANOTHER_GLSL, "compute"),
                );
                assert!(matches!(response, EscalateResponse::Ok(_)), "{response:?}");
            }
            assert_eq!(
                sandbox.host_inner().glsl_shader_compiler_invocation_count() - before,
                1
            );
        }

        /// A two-binding kernel shaped like the read-one-write-another pass
        /// this whole change exists to make possible.
        fn blur_kernel_bindings() -> Vec<ComputeBindingSpec> {
            vec![
                ComputeBindingSpec::sampled_texture(0).with_name("source_image"),
                ComputeBindingSpec::storage_image(1).with_name("output_image"),
            ]
        }

        fn supplied(
            entries: &[(&str, EscalateComputeBindingKind, &str)],
        ) -> Vec<EscalateRequestRunComputeKernelBinding> {
            entries
                .iter()
                .map(
                    |(name, kind, target_id)| EscalateRequestRunComputeKernelBinding {
                        kind: *kind,
                        name: (*name).to_string(),
                        target_id: (*target_id).to_string(),
                    },
                )
                .collect()
        }

        fn plan_error(supplied: &[EscalateRequestRunComputeKernelBinding]) -> String {
            let declared = blur_kernel_bindings();
            let err = plan_supplied_compute_bindings(supplied, &declared)
                .err()
                .expect("expected the plan to be refused");
            format!("{err}")
        }

        #[test]
        fn a_complete_dispatch_resolves_every_name_to_its_slot() {
            let entries = supplied(&[
                (
                    "output_image",
                    EscalateComputeBindingKind::StorageImage,
                    "surface-out",
                ),
                (
                    "source_image",
                    EscalateComputeBindingKind::SampledTexture,
                    "surface-in",
                ),
            ]);
            let declared = blur_kernel_bindings();
            let planned = plan_supplied_compute_bindings(&entries, &declared)
                .expect("a complete, correctly-typed dispatch");

            // Resolution is by name, so the order the caller supplied them in
            // is not the order the shader declared them in — and that is fine.
            assert_eq!(planned.len(), 2);
            assert_eq!(planned[0].name, "output_image");
            assert_eq!(planned[0].binding, 1);
            assert_eq!(
                planned[0].kind,
                SurfaceBoundComputeBindingKind::StorageImage
            );
            assert_eq!(planned[0].target_id, "surface-out");
            assert_eq!(planned[1].name, "source_image");
            assert_eq!(planned[1].binding, 0);
            assert_eq!(planned[1].target_id, "surface-in");
        }

        /// Not expressible in a Python mapping — a dict cannot carry one key
        /// twice — so the wire array is the only layer that can guard it.
        #[test]
        fn a_name_supplied_twice_is_refused() {
            let message = plan_error(&supplied(&[
                (
                    "source_image",
                    EscalateComputeBindingKind::SampledTexture,
                    "surface-in",
                ),
                (
                    "source_image",
                    EscalateComputeBindingKind::SampledTexture,
                    "surface-other",
                ),
                (
                    "output_image",
                    EscalateComputeBindingKind::StorageImage,
                    "surface-out",
                ),
            ]));
            assert!(
                message.contains("`source_image` was supplied twice"),
                "must name the duplicate, got: {message}"
            );
            assert!(
                message.contains("`source_image`, `output_image`"),
                "must name the shader's declared bindings, got: {message}"
            );
        }

        #[test]
        fn a_name_the_shader_does_not_declare_is_refused() {
            let message = plan_error(&supplied(&[
                (
                    "source_image",
                    EscalateComputeBindingKind::SampledTexture,
                    "surface-in",
                ),
                (
                    "output_image",
                    EscalateComputeBindingKind::StorageImage,
                    "surface-out",
                ),
                (
                    "sharpen_amount",
                    EscalateComputeBindingKind::UniformBuffer,
                    "surface-x",
                ),
            ]));
            assert!(
                message.contains("`sharpen_amount` is not one this shader declares"),
                "must name the unknown binding, got: {message}"
            );
            assert!(
                message.contains("`source_image`, `output_image`"),
                "must name the shader's declared bindings, got: {message}"
            );
        }

        /// No implicit default and no carried-over value: the kernel holds no
        /// binding state between dispatches to fall back on.
        #[test]
        fn a_declared_binding_left_out_is_refused() {
            let message = plan_error(&supplied(&[(
                "source_image",
                EscalateComputeBindingKind::SampledTexture,
                "surface-in",
            )]));
            assert!(
                message.contains("`output_image` was not supplied"),
                "must name the missing binding, got: {message}"
            );
            assert!(
                message.contains("do not persist between dispatches"),
                "must say why there is no fallback, got: {message}"
            );
            assert!(
                message.contains("`source_image`, `output_image`"),
                "must name the shader's declared bindings, got: {message}"
            );
        }

        #[test]
        fn a_binding_supplied_as_the_wrong_kind_is_refused() {
            let message = plan_error(&supplied(&[
                (
                    "source_image",
                    EscalateComputeBindingKind::SampledTexture,
                    "surface-in",
                ),
                (
                    "output_image",
                    EscalateComputeBindingKind::StorageBuffer,
                    "surface-out",
                ),
            ]));
            assert!(
                message.contains("`output_image` was supplied as StorageBuffer"),
                "must name the binding and the kind supplied, got: {message}"
            );
            assert!(
                message.contains("declares it StorageImage"),
                "must name the kind the shader declares, got: {message}"
            );
        }

        /// A kernel with no bindings at all dispatches — the empty case is not
        /// an error, and the "missing" rule has nothing to fire on.
        #[test]
        fn a_kernel_declaring_nothing_needs_nothing_supplied() {
            let planned =
                plan_supplied_compute_bindings(&[], &[]).expect("an unbound kernel dispatches");
            assert!(planned.is_empty());
        }

        /// Both hex fields are decoded before the escalate hop, so a malformed
        /// one is refused without touching the GPU at all.
        #[test]
        fn register_with_invalid_spv_hex_is_refused_without_escalating() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("register_with_invalid_spv_hex: no GPU — skipping");
                return;
            };
            let response = handle_register_compute_kernel(
                &sandbox,
                "rid-1".to_string(),
                EscalateRequestRegisterComputeKernel {
                    entry_point: "".to_string(),
                    source: "".to_string(),
                    stage: "".to_string(),
                    bindings: Vec::new(),
                    push_constant_size: 0,
                    request_id: "rid-1".to_string(),
                    spv_hex: "not-hex".to_string(),
                },
            );
            match response {
                EscalateResponse::Err(err) => assert!(
                    err.message.contains("spv_hex decode"),
                    "got: {}",
                    err.message
                ),
                other => panic!("expected Err, got {other:?}"),
            }
        }

        #[test]
        fn run_with_invalid_push_constants_hex_is_refused_without_escalating() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("run_with_invalid_push_constants_hex: no GPU — skipping");
                return;
            };
            let response = handle_run_compute_kernel(
                &sandbox,
                "rid-2".to_string(),
                EscalateRequestRunComputeKernel {
                    bindings: Vec::new(),
                    group_count_x: 1,
                    group_count_y: 1,
                    group_count_z: 1,
                    kernel_id: "whatever".to_string(),
                    push_constants_hex: "zz".to_string(),
                    request_id: "rid-2".to_string(),
                },
            );
            match response {
                EscalateResponse::Err(err) => assert!(
                    err.message.contains("push_constants_hex decode"),
                    "got: {}",
                    err.message
                ),
                other => panic!("expected Err, got {other:?}"),
            }
        }

        /// The SPIR-V for the pass the v1 wire could not express — one
        /// sampled input, one storage output, deliberately different kinds.
        const READ_ONE_WRITE_ANOTHER_SPV: &[u8] =
            include_bytes!(concat!(env!("OUT_DIR"), "/test_read_one_write_another.spv"));

        fn read_one_write_another_spv_hex() -> String {
            READ_ONE_WRITE_ANOTHER_SPV
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        }

        fn register_read_one_write_another(
            sandbox: &GpuContextLimitedAccess,
        ) -> EscalateResponseOk {
            let response = handle_register_compute_kernel(
                sandbox,
                "reg".to_string(),
                EscalateRequestRegisterComputeKernel {
                    entry_point: "".to_string(),
                    source: "".to_string(),
                    stage: "".to_string(),
                    bindings: Vec::new(),
                    push_constant_size: 4,
                    request_id: "reg".to_string(),
                    spv_hex: read_one_write_another_spv_hex(),
                },
            );
            match response {
                EscalateResponse::Ok(ok) => ok,
                other => panic!("registering the conformance kernel failed: {other:?}"),
            }
        }

        /// Registration hands back the shape a dispatch needs: the shader's own
        /// names, each with the kind only the shader knows.
        #[test]
        fn registration_answers_with_the_shaders_binding_names_and_kinds() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("registration_answers_with_the_shaders_bindings: no GPU — skipping");
                return;
            };
            let ok = register_read_one_write_another(&sandbox);
            let bindings = ok.bindings.expect("a register response carries the shape");
            assert_eq!(
                bindings
                    .iter()
                    .map(|b| (b.name.as_str(), b.kind))
                    .collect::<Vec<_>>(),
                vec![
                    ("source_image", EscalateComputeBindingKind::SampledTexture),
                    ("output_image", EscalateComputeBindingKind::StorageImage),
                ],
                "the two bindings differ in name and in kind, so binding by slot order \
                 rather than by name would swap them"
            );
        }

        /// Re-creating an identical kernel is free: same id, and the very same
        /// kernel — counted by identity, never by elapsed time.
        #[test]
        fn re_registering_an_identical_kernel_is_a_cache_hit() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("re_registering_an_identical_kernel: no GPU — skipping");
                return;
            };
            let first = register_read_one_write_another(&sandbox);
            let second = register_read_one_write_another(&sandbox);
            assert_eq!(
                first.handle_id, second.handle_id,
                "an identical kernel keeps its id"
            );

            let held = sandbox
                .escalate(|full| {
                    let a = full.compute_kernel_by_id(&first.handle_id);
                    let b = full.compute_kernel_by_id(&second.handle_id);
                    Ok((a, b))
                })
                .expect("the cache answers inside an escalate scope");
            let (a, b) = (held.0.expect("cached"), held.1.expect("cached"));
            assert!(
                std::sync::Arc::ptr_eq(&a, &b),
                "the second registration must reuse the first kernel, not build another"
            );
        }

        /// Each seed channel inverts exactly in unorm8: out = 255 - in.
        const SEED_RGBA: [u8; 4] = [10, 20, 30, 255];
        const INVERTED_RGBA: [u8; 4] = [245, 235, 225, 255];

        /// The whole point of the change: two surfaces, bound by the shader's
        /// own names, and the output pixels prove the source was read.
        ///
        /// Not just "the dispatch was accepted": the source is seeded with a
        /// known value and the output is read back and compared against the
        /// shader's own arithmetic, so binding the two names backwards — the
        /// exact failure a by-slot resolution would produce, since both
        /// textures share extent, format and usage — fails the assertion
        /// rather than passing silently.
        ///
        /// The textures are registered in-process rather than acquired over
        /// the escalate op, because an escalate-acquired texture is published
        /// to the surface-share service and resolves through it — and this
        /// test has no service. The subject here is the dispatch.
        #[test]
        fn a_dispatch_reads_one_surface_and_writes_another() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("a_dispatch_reads_one_surface_and_writes_another: no GPU — skipping");
                return;
            };
            let kernel_id = register_read_one_write_another(&sandbox).handle_id;

            // Held for the dispatch: dropping a pooled handle hands its slot
            // back, and the registration would then name a recycled texture.
            let held = sandbox
                .escalate(|full| {
                    let desc = TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                        .with_usage(
                            TextureUsages::TEXTURE_BINDING
                                | TextureUsages::STORAGE_BINDING
                                | TextureUsages::COPY_SRC
                                | TextureUsages::COPY_DST,
                        );
                    let source = full.acquire_texture(&desc)?;
                    let output = full.acquire_texture(&desc)?;
                    full.register_texture("conformance-source", source.texture().clone());
                    full.register_texture("conformance-output", output.texture().clone());

                    // Seed the source with a constant the shader's arithmetic
                    // transforms recognizably.
                    let (_pool_id, seed_buffer) =
                        full.acquire_pixel_buffer(64, 64, crate::core::rhi::PixelFormat::Rgba32)?;
                    let plane = seed_buffer.buffer_ref().plane_base_address(0);
                    unsafe {
                        for pixel in 0..(64 * 64) {
                            std::ptr::copy_nonoverlapping(
                                SEED_RGBA.as_ptr(),
                                plane.add(pixel * 4),
                                4,
                            );
                        }
                    }
                    full.copy_pixel_buffer_to_texture(
                        &seed_buffer,
                        source.texture(),
                        "conformance-source",
                        64,
                        64,
                    )?;
                    Ok((source, output))
                })
                .expect("two pooled textures, the source seeded");
            let (source_id, output_id) = ("conformance-source", "conformance-output");

            let response = handle_run_compute_kernel(
                &sandbox,
                "run".to_string(),
                EscalateRequestRunComputeKernel {
                    // Supplied in the reverse of declaration order, so a
                    // resolution that walked slots instead of names would bind
                    // the two backwards and fail the pixel assertion below.
                    bindings: vec![
                        EscalateRequestRunComputeKernelBinding {
                            kind: EscalateComputeBindingKind::StorageImage,
                            name: "output_image".to_string(),
                            target_id: output_id.to_string(),
                        },
                        EscalateRequestRunComputeKernelBinding {
                            kind: EscalateComputeBindingKind::SampledTexture,
                            name: "source_image".to_string(),
                            target_id: source_id.to_string(),
                        },
                    ],
                    group_count_x: 8,
                    group_count_y: 8,
                    group_count_z: 1,
                    kernel_id: kernel_id.clone(),
                    push_constants_hex: "00000000".to_string(),
                    request_id: "run".to_string(),
                },
            );
            match response {
                EscalateResponse::Ok(ok) => assert_eq!(ok.handle_id, kernel_id),
                other => panic!("the read-one-write-another dispatch failed: {other:?}"),
            }

            // The dispatch retired before the response, so the output is
            // readable now — compute leaves a storage image in GENERAL.
            let output_pixels = sandbox
                .escalate(|full| {
                    let readback = full.create_texture_readback(
                        "conformance-readback",
                        64,
                        64,
                        TextureFormat::Rgba8Unorm,
                    )?;
                    let ticket = readback.submit(
                        held.1.texture(),
                        crate::core::rhi::TextureSourceLayout::General,
                    )?;
                    Ok(readback.wait_and_read(ticket, 2_000_000_000)?.to_vec())
                })
                .expect("the output texture reads back");

            for (pixel_index, pixel) in output_pixels.chunks_exact(4).enumerate() {
                assert_eq!(
                    pixel, INVERTED_RGBA,
                    "pixel {pixel_index} must be the inverted seed — the kernel read \
                     `source_image` and wrote `output_image`, by name"
                );
            }
            drop(held);
        }

        /// The cache key covers the blob, not the declaration — so the
        /// declaration is checked on the hit path too, and a wrong assertion
        /// refuses identically whether or not the blob was registered before.
        #[test]
        fn a_wrong_declaration_is_refused_even_when_the_kernel_is_cached() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("a_wrong_declaration_is_refused_when_cached: no GPU — skipping");
                return;
            };
            // First registration warms the cache.
            register_read_one_write_another(&sandbox);

            let response = handle_register_compute_kernel(
                &sandbox,
                "reg-wrong".to_string(),
                EscalateRequestRegisterComputeKernel {
                    entry_point: "".to_string(),
                    source: "".to_string(),
                    stage: "".to_string(),
                    bindings: vec![EscalateRequestRegisterComputeKernelBinding {
                        kind: EscalateComputeBindingKind::StorageBuffer,
                        name: "sharpen_amount".to_string(),
                    }],
                    push_constant_size: 4,
                    request_id: "reg-wrong".to_string(),
                    spv_hex: read_one_write_another_spv_hex(),
                },
            );
            match response {
                EscalateResponse::Err(err) => assert!(
                    err.message.contains("`sharpen_amount`")
                        && err.message.contains("`source_image`"),
                    "the refusal must name the bogus binding and the shader's own: {}",
                    err.message
                ),
                other => panic!("a wrong declaration must refuse on a cache hit, got {other:?}"),
            }
        }

        /// A pixel-buffer surface is a legal id a Python caller can hold, and
        /// binding it must refuse by name — not fall into the buffer→texture
        /// synthesis path with a zero extent.
        ///
        /// The second half is the guard's real substance: a legitimate
        /// resolver's cached canvas for the same slot must survive the refused
        /// dispatch. Without the zero-extent guard the refusal still fires
        /// (the 0×0 create fails), but only after evicting that canvas — so
        /// this test resolves the surface at its real extent before and after,
        /// and asserts the same texture comes back.
        #[test]
        fn binding_a_buffer_backed_surface_is_refused_by_name() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("binding_a_buffer_backed_surface: no GPU — skipping");
                return;
            };
            let kernel_id = register_read_one_write_another(&sandbox).handle_id;

            let (buffer_surface_id, held_buffer) = sandbox
                .escalate(|full| {
                    let desc = TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                        .with_usage(
                            TextureUsages::TEXTURE_BINDING | TextureUsages::STORAGE_BINDING,
                        );
                    let output = full.acquire_texture(&desc)?;
                    full.register_texture("buffer-refusal-output", output.texture().clone());
                    let (pool_id, buffer) =
                        full.acquire_pixel_buffer(64, 64, crate::core::rhi::PixelFormat::Rgba32)?;
                    Ok((pool_id.to_string(), (buffer, output)))
                })
                .expect("a pixel buffer and an output texture");

            // A legitimate resolve at the real extent populates the slot's
            // cached canvas — the thing the refused dispatch must not evict.
            // The registration is held alive across the test: were the canvas
            // evicted and recreated, the driver could hand the replacement a
            // recycled handle value, and comparing dead handles would lie.
            let registration_before = sandbox
                .escalate(|full| {
                    full.resolve_texture_registration_by_surface_id(
                        &buffer_surface_id,
                        None,
                        64,
                        64,
                    )
                })
                .expect("the buffer surface resolves at its real extent");
            let canvas_before = registration_before.texture().vulkan_inner().image();

            let response = handle_run_compute_kernel(
                &sandbox,
                "run-buffer".to_string(),
                EscalateRequestRunComputeKernel {
                    bindings: vec![
                        EscalateRequestRunComputeKernelBinding {
                            kind: EscalateComputeBindingKind::SampledTexture,
                            name: "source_image".to_string(),
                            target_id: buffer_surface_id.clone(),
                        },
                        EscalateRequestRunComputeKernelBinding {
                            kind: EscalateComputeBindingKind::StorageImage,
                            name: "output_image".to_string(),
                            target_id: "buffer-refusal-output".to_string(),
                        },
                    ],
                    group_count_x: 8,
                    group_count_y: 8,
                    group_count_z: 1,
                    kernel_id,
                    push_constants_hex: "00000000".to_string(),
                    request_id: "run-buffer".to_string(),
                },
            );
            match response {
                EscalateResponse::Err(err) => assert!(
                    err.message.contains("`source_image`")
                        && err.message.contains("cannot resolve to a device texture"),
                    "must name the binding and refuse it as a non-texture: {}",
                    err.message
                ),
                other => panic!("a buffer-backed binding must refuse, got {other:?}"),
            }

            let canvas_after = sandbox
                .escalate(|full| {
                    let registration = full.resolve_texture_registration_by_surface_id(
                        &buffer_surface_id,
                        None,
                        64,
                        64,
                    )?;
                    Ok(registration.texture().vulkan_inner().image())
                })
                .expect("the buffer surface still resolves after the refused dispatch");
            assert_eq!(
                canvas_before, canvas_after,
                "the refused dispatch must not evict the slot's cached canvas — a fresh \
                 texture here means the zero-extent guard fired after the eviction, not before"
            );
            drop(registration_before);
            drop(held_buffer);
        }

        #[test]
        fn dispatching_an_unregistered_kernel_id_is_refused() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("dispatching_an_unregistered_kernel_id: no GPU — skipping");
                return;
            };
            let response = handle_run_compute_kernel(
                &sandbox,
                "rid-3".to_string(),
                EscalateRequestRunComputeKernel {
                    bindings: Vec::new(),
                    group_count_x: 1,
                    group_count_y: 1,
                    group_count_z: 1,
                    kernel_id: "never-registered".to_string(),
                    push_constants_hex: String::new(),
                    request_id: "rid-3".to_string(),
                },
            );
            match response {
                EscalateResponse::Err(err) => assert!(
                    err.message.contains("no kernel registered under id"),
                    "got: {}",
                    err.message
                ),
                other => panic!("expected Err, got {other:?}"),
            }
        }

        use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestRunComputeKernelBatchDispatch;

        /// Pass 1 of the chain: every channel gains 40/255.
        const BRIGHTEN_GLSL: &str = "\
#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0) uniform sampler2D unbrightened_image;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D brightened_image;
void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    vec4 source = texelFetch(unbrightened_image, at, 0);
    imageStore(brightened_image, at, vec4(source.rgb + 40.0 / 255.0, source.a));
}
";

        /// Pass 2 of the chain: every channel doubles. Deliberately not
        /// commutative with pass 1, so running them in the wrong order — or
        /// running pass 2 against pass 1's *input* — lands on different pixels.
        const DOUBLE_GLSL: &str = "\
#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0) uniform sampler2D brightened_image;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D doubled_image;
void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    vec4 source = texelFetch(brightened_image, at, 0);
    imageStore(doubled_image, at, vec4(source.rgb * 2.0, source.a));
}
";

        /// 10,20,30 brightened by 40 is 50,60,70; doubled is 100,120,140.
        const CHAIN_SEED_RGBA: [u8; 4] = [10, 20, 30, 255];
        const CHAIN_BRIGHTENED_RGBA: [u8; 4] = [50, 60, 70, 255];
        const CHAIN_DOUBLED_RGBA: [u8; 4] = [100, 120, 140, 255];

        fn register_glsl_kernel(sandbox: &GpuContextLimitedAccess, source: &str) -> String {
            let response = handle_register_compute_kernel(
                sandbox,
                "reg-chain".to_string(),
                register_from_glsl(source, "compute"),
            );
            match response {
                EscalateResponse::Ok(ok) => ok.handle_id,
                other => panic!("registering a chain kernel failed: {other:?}"),
            }
        }

        fn batched_dispatch(
            kernel_id: &str,
            source_binding: (&str, &str),
            output_binding: (&str, &str),
        ) -> EscalateRequestRunComputeKernelBatchDispatch {
            EscalateRequestRunComputeKernelBatchDispatch {
                bindings: vec![
                    EscalateRequestRunComputeKernelBinding {
                        kind: EscalateComputeBindingKind::SampledTexture,
                        name: source_binding.0.to_string(),
                        target_id: source_binding.1.to_string(),
                    },
                    EscalateRequestRunComputeKernelBinding {
                        kind: EscalateComputeBindingKind::StorageImage,
                        name: output_binding.0.to_string(),
                        target_id: output_binding.1.to_string(),
                    },
                ],
                group_count_x: 8,
                group_count_y: 8,
                group_count_z: 1,
                kernel_id: kernel_id.to_string(),
                push_constants_hex: String::new(),
            }
        }

        /// Three 64×64 textures registered under fixed ids, the first seeded
        /// with [`CHAIN_SEED_RGBA`]. Returned held: dropping a pooled handle
        /// hands its slot back, and the registration would then name a
        /// recycled texture.
        fn seeded_chain_textures(
            sandbox: &GpuContextLimitedAccess,
            ids: [&str; 3],
        ) -> Vec<crate::core::context::PooledTextureHandle> {
            sandbox
                .escalate(|full| {
                    let desc = TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                        .with_usage(
                            TextureUsages::TEXTURE_BINDING
                                | TextureUsages::STORAGE_BINDING
                                | TextureUsages::COPY_SRC
                                | TextureUsages::COPY_DST,
                        );
                    let mut held = Vec::with_capacity(ids.len());
                    for id in ids {
                        let texture = full.acquire_texture(&desc)?;
                        full.register_texture(id, texture.texture().clone());
                        held.push(texture);
                    }

                    let (_pool_id, seed_buffer) =
                        full.acquire_pixel_buffer(64, 64, crate::core::rhi::PixelFormat::Rgba32)?;
                    let plane = seed_buffer.buffer_ref().plane_base_address(0);
                    unsafe {
                        for pixel in 0..(64 * 64) {
                            std::ptr::copy_nonoverlapping(
                                CHAIN_SEED_RGBA.as_ptr(),
                                plane.add(pixel * 4),
                                4,
                            );
                        }
                    }
                    full.copy_pixel_buffer_to_texture(
                        &seed_buffer,
                        held[0].texture(),
                        ids[0],
                        64,
                        64,
                    )?;
                    Ok(held)
                })
                .expect("three pooled textures, the first seeded")
        }

        fn read_back_rgba8(
            sandbox: &GpuContextLimitedAccess,
            texture: &crate::core::rhi::Texture,
            label: &str,
        ) -> Vec<u8> {
            sandbox
                .escalate(|full| {
                    let readback =
                        full.create_texture_readback(label, 64, 64, TextureFormat::Rgba8Unorm)?;
                    let ticket =
                        readback.submit(texture, crate::core::rhi::TextureSourceLayout::General)?;
                    Ok(readback.wait_and_read(ticket, 2_000_000_000)?.to_vec())
                })
                .expect("the texture reads back")
        }

        fn assert_every_pixel_is(pixels: &[u8], expected: [u8; 4], what: &str) {
            for (index, pixel) in pixels.chunks_exact(4).enumerate() {
                assert_eq!(pixel, expected, "pixel {index} of {what}");
            }
        }

        /// The claim the whole op rests on: a later pass reads what an earlier
        /// pass wrote, inside one recording.
        ///
        /// The intermediate is written as a storage image and read as a sampled
        /// texture, so the batch owes it both a memory dependency and a layout
        /// transition — and a barrier taken from the texture's *pre-batch*
        /// layout would discard the very writes pass 2 is there for. The two
        /// shaders do not commute, so a swapped order fails on the pixels
        /// rather than passing quietly.
        #[test]
        fn a_later_pass_in_a_batch_reads_what_an_earlier_pass_wrote() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("batched chain: no GPU — skipping");
                return;
            };
            let brighten = register_glsl_kernel(&sandbox, BRIGHTEN_GLSL);
            let double = register_glsl_kernel(&sandbox, DOUBLE_GLSL);
            let held = seeded_chain_textures(
                &sandbox,
                ["chain-seed", "chain-brightened", "chain-doubled"],
            );

            let response = handle_run_compute_kernel_batch(
                &sandbox,
                "chain".to_string(),
                EscalateRequestRunComputeKernelBatch {
                    dispatches: vec![
                        batched_dispatch(
                            &brighten,
                            ("unbrightened_image", "chain-seed"),
                            ("brightened_image", "chain-brightened"),
                        ),
                        batched_dispatch(
                            &double,
                            ("brightened_image", "chain-brightened"),
                            ("doubled_image", "chain-doubled"),
                        ),
                    ],
                    request_id: "chain".to_string(),
                },
            );
            assert!(
                matches!(response, EscalateResponse::Ok(_)),
                "the two-pass chain failed: {response:?}"
            );

            assert_every_pixel_is(
                &read_back_rgba8(&sandbox, held[2].texture(), "chain-readback"),
                CHAIN_DOUBLED_RGBA,
                "the chain's output — pass 2 must have read pass 1's writes, not the \
                 seed and not an undefined intermediate",
            );
            assert_every_pixel_is(
                &read_back_rgba8(&sandbox, held[1].texture(), "chain-intermediate-readback"),
                CHAIN_BRIGHTENED_RGBA,
                "the intermediate — pass 1's own output",
            );

            // Each texture's tracked layout is published as the layout its
            // *last* use in the batch left it in, which is where the next
            // batch's barrier starts from. Asserted because the pixels above
            // cannot check it: a source layout of UNDEFINED licenses the driver
            // to discard contents, and this one declines to, so a batch that
            // barriered every pass from the pre-batch layout would still read
            // back correctly here while being wrong by the spec.
            let tracked = |surface_id: &str| {
                sandbox
                    .escalate(|full| {
                        Ok(full
                            .resolve_texture_registration_by_surface_id(surface_id, None, 64, 64)?
                            .current_layout())
                    })
                    .expect("the chain's textures still resolve")
            };
            assert_eq!(
                tracked("chain-brightened"),
                streamlib_consumer_rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                "the intermediate was written as a storage image and then read as a sampled \
                 texture, so it ends in the layout its last use required"
            );
            assert_eq!(
                tracked("chain-doubled"),
                streamlib_consumer_rhi::VulkanLayout::GENERAL,
                "the final output was only ever written, so it ends in GENERAL"
            );
            drop(held);
        }

        /// The reason the op exists, counted rather than timed: three passes
        /// batched cost one submission and one stall, where three separate
        /// `run_compute_kernel` ops cost three submissions and two stalls each.
        ///
        /// Both arms run the same three dispatches on the same device in the
        /// same test, so the comparison is against the path this op replaces —
        /// not against a remembered number.
        #[test]
        fn a_batch_costs_one_submission_and_one_stall_where_separate_dispatches_cost_n() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("batch submission count: no GPU — skipping");
                return;
            };
            let brighten = register_glsl_kernel(&sandbox, BRIGHTEN_GLSL);
            let double = register_glsl_kernel(&sandbox, DOUBLE_GLSL);
            let held = seeded_chain_textures(
                &sandbox,
                ["counted-seed", "counted-brightened", "counted-doubled"],
            );

            let dispatches = vec![
                batched_dispatch(
                    &brighten,
                    ("unbrightened_image", "counted-seed"),
                    ("brightened_image", "counted-brightened"),
                ),
                batched_dispatch(
                    &double,
                    ("brightened_image", "counted-brightened"),
                    ("doubled_image", "counted-doubled"),
                ),
            ];

            // Measured on the second run, not the first: a per-frame claim is
            // about the steady state, and the opening batch on a fresh context
            // also builds the recorder and finds its fence already signaled.
            let mut batched_submissions = 0;
            let mut batched_stalls = 0;
            for run in 0..2 {
                let submissions_before = sandbox.host_inner().queue_submission_count();
                let stalls_before = sandbox
                    .host_inner()
                    .recorder_and_compute_kernel_fence_wait_count();
                let response = handle_run_compute_kernel_batch(
                    &sandbox,
                    format!("counted-{run}"),
                    EscalateRequestRunComputeKernelBatch {
                        dispatches: dispatches.clone(),
                        request_id: format!("counted-{run}"),
                    },
                );
                assert!(matches!(response, EscalateResponse::Ok(_)), "{response:?}");
                batched_submissions =
                    sandbox.host_inner().queue_submission_count() - submissions_before;
                batched_stalls = sandbox
                    .host_inner()
                    .recorder_and_compute_kernel_fence_wait_count()
                    - stalls_before;
            }

            assert_eq!(
                batched_submissions, 1,
                "two batched dispatches must go out as one command buffer"
            );
            assert_eq!(
                batched_stalls, 1,
                "and cost the caller exactly one fence wait — a second would mean the \
                 recorder waits again at the next begin() on a fence it already drained"
            );

            let submissions_before = sandbox.host_inner().queue_submission_count();
            let stalls_before = sandbox
                .host_inner()
                .recorder_and_compute_kernel_fence_wait_count();
            for dispatch in &dispatches {
                let response = handle_run_compute_kernel(
                    &sandbox,
                    "separate".to_string(),
                    EscalateRequestRunComputeKernel {
                        bindings: dispatch.bindings.clone(),
                        group_count_x: dispatch.group_count_x,
                        group_count_y: dispatch.group_count_y,
                        group_count_z: dispatch.group_count_z,
                        kernel_id: dispatch.kernel_id.clone(),
                        push_constants_hex: dispatch.push_constants_hex.clone(),
                        request_id: "separate".to_string(),
                    },
                );
                assert!(matches!(response, EscalateResponse::Ok(_)), "{response:?}");
            }
            let separate_submissions =
                sandbox.host_inner().queue_submission_count() - submissions_before;
            let separate_stalls = sandbox
                .host_inner()
                .recorder_and_compute_kernel_fence_wait_count()
                - stalls_before;

            assert_eq!(
                separate_submissions,
                dispatches.len(),
                "the path the batch replaces submits once per dispatch — if this is 1 the \
                 counter is not counting and the assertion above proves nothing"
            );
            assert!(
                separate_stalls > batched_stalls,
                "the path the batch replaces must stall more than the batch does: \
                 {separate_stalls} vs {batched_stalls}"
            );
            drop(held);
        }

        /// A kernel owns one descriptor set, so the second bind would hand the
        /// first recorded dispatch this dispatch's bindings — and nothing has
        /// executed yet, so it would do it silently.
        #[test]
        fn a_batch_naming_one_kernel_twice_is_refused_saying_why() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("batch duplicate kernel: no GPU — skipping");
                return;
            };
            let brighten = register_glsl_kernel(&sandbox, BRIGHTEN_GLSL);
            let held = seeded_chain_textures(&sandbox, ["twice-seed", "twice-middle", "twice-out"]);

            let response = handle_run_compute_kernel_batch(
                &sandbox,
                "twice".to_string(),
                EscalateRequestRunComputeKernelBatch {
                    dispatches: vec![
                        batched_dispatch(
                            &brighten,
                            ("unbrightened_image", "twice-seed"),
                            ("brightened_image", "twice-middle"),
                        ),
                        batched_dispatch(
                            &brighten,
                            ("unbrightened_image", "twice-middle"),
                            ("brightened_image", "twice-out"),
                        ),
                    ],
                    request_id: "twice".to_string(),
                },
            );
            match response {
                EscalateResponse::Err(err) => {
                    assert!(
                        err.message.contains("descriptor set"),
                        "the refusal must say why one kernel cannot appear twice: {}",
                        err.message
                    );
                    assert!(
                        err.message.contains("dispatch 1") && err.message.contains("dispatch 0"),
                        "and must name both dispatches: {}",
                        err.message
                    );
                }
                other => panic!("naming one kernel twice must refuse, got {other:?}"),
            }
            drop(held);
        }

        /// A batch runs whole or not at all, and never strands the recorder.
        ///
        /// The refused batch here fails on its *second* dispatch, so the first
        /// one was already planned when the refusal fired. Nothing may reach
        /// the GPU — asserted on the first dispatch's output pixels, which stay
        /// at what they held — and the recorder must still take the next batch,
        /// which is what `begin()` would refuse if the failure had left a
        /// recording open.
        #[test]
        fn a_refused_batch_submits_nothing_and_leaves_the_recorder_usable() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("refused batch: no GPU — skipping");
                return;
            };
            let brighten = register_glsl_kernel(&sandbox, BRIGHTEN_GLSL);
            let double = register_glsl_kernel(&sandbox, DOUBLE_GLSL);
            let held = seeded_chain_textures(
                &sandbox,
                ["refused-seed", "refused-brightened", "refused-doubled"],
            );

            // The intermediate starts undefined; one batch establishes it, so
            // the assertion below is against known content rather than
            // whatever the allocator handed over.
            let established = handle_run_compute_kernel_batch(
                &sandbox,
                "establish".to_string(),
                EscalateRequestRunComputeKernelBatch {
                    dispatches: vec![batched_dispatch(
                        &brighten,
                        ("unbrightened_image", "refused-seed"),
                        ("brightened_image", "refused-brightened"),
                    )],
                    request_id: "establish".to_string(),
                },
            );
            assert!(
                matches!(established, EscalateResponse::Ok(_)),
                "{established:?}"
            );

            let submissions_before = sandbox.host_inner().queue_submission_count();
            let refused = handle_run_compute_kernel_batch(
                &sandbox,
                "refused".to_string(),
                EscalateRequestRunComputeKernelBatch {
                    dispatches: vec![
                        batched_dispatch(
                            &brighten,
                            ("unbrightened_image", "refused-brightened"),
                            ("brightened_image", "refused-brightened"),
                        ),
                        batched_dispatch(
                            &double,
                            ("brightened_image", "no-such-surface"),
                            ("doubled_image", "refused-doubled"),
                        ),
                    ],
                    request_id: "refused".to_string(),
                },
            );
            match refused {
                EscalateResponse::Err(err) => assert!(
                    err.message.contains("dispatch 1") && err.message.contains("no-such-surface"),
                    "the refusal must name the dispatch and the surface: {}",
                    err.message
                ),
                other => panic!("an unresolvable binding must refuse, got {other:?}"),
            }
            assert_eq!(
                sandbox.host_inner().queue_submission_count(),
                submissions_before,
                "a refused batch submits nothing — not even the dispatches ahead of the \
                 one that was refused"
            );
            assert_every_pixel_is(
                &read_back_rgba8(&sandbox, held[1].texture(), "refused-readback"),
                CHAIN_BRIGHTENED_RGBA,
                "the intermediate, untouched by the refused batch",
            );

            // The recorder survived: a fresh batch runs, which begin() would
            // refuse outright if the refusal above had left a recording open.
            let after = handle_run_compute_kernel_batch(
                &sandbox,
                "after".to_string(),
                EscalateRequestRunComputeKernelBatch {
                    dispatches: vec![batched_dispatch(
                        &double,
                        ("brightened_image", "refused-brightened"),
                        ("doubled_image", "refused-doubled"),
                    )],
                    request_id: "after".to_string(),
                },
            );
            assert!(
                matches!(after, EscalateResponse::Ok(_)),
                "the recorder must still be usable after a refused batch: {after:?}"
            );
            assert_every_pixel_is(
                &read_back_rgba8(&sandbox, held[2].texture(), "after-readback"),
                CHAIN_DOUBLED_RGBA,
                "the batch that ran after the refused one",
            );
            drop(held);
        }

        /// The one failure that lands *inside* an open recording, and the only
        /// test that reaches `abort_recording`.
        ///
        /// Every other refusal — an unknown kernel, a binding the shader does
        /// not declare, a surface that will not resolve, one kernel twice —
        /// fires while resolving, before `begin()`. This one cannot: the
        /// push-constant size is the kernel's own business and is only checked
        /// when the payload is staged, which happens after the barriers are
        /// recorded. So the recording is open when it fails, and a batch that
        /// did not abort it would strand the recorder — the next `begin()`
        /// refuses outright while a recording is in progress.
        #[test]
        fn a_batch_failing_inside_the_recording_aborts_it_and_the_recorder_survives() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("mid-recording batch failure: no GPU — skipping");
                return;
            };
            // The conformance kernel declares 4 push-constant bytes; the batch
            // below sends none.
            let kernel_id = register_read_one_write_another(&sandbox).handle_id;
            let brighten = register_glsl_kernel(&sandbox, BRIGHTEN_GLSL);
            let held =
                seeded_chain_textures(&sandbox, ["aborted-seed", "aborted-middle", "aborted-out"]);

            let submissions_before = sandbox.host_inner().queue_submission_count();
            let response = handle_run_compute_kernel_batch(
                &sandbox,
                "aborted".to_string(),
                EscalateRequestRunComputeKernelBatch {
                    dispatches: vec![EscalateRequestRunComputeKernelBatchDispatch {
                        bindings: supplied(&[
                            (
                                "source_image",
                                EscalateComputeBindingKind::SampledTexture,
                                "aborted-seed",
                            ),
                            (
                                "output_image",
                                EscalateComputeBindingKind::StorageImage,
                                "aborted-middle",
                            ),
                        ]),
                        group_count_x: 8,
                        group_count_y: 8,
                        group_count_z: 1,
                        kernel_id,
                        push_constants_hex: String::new(),
                    }],
                    request_id: "aborted".to_string(),
                },
            );
            match response {
                EscalateResponse::Err(err) => assert!(
                    err.message.contains("push-constant size mismatch")
                        && err.message.contains("kernel declares 4"),
                    "the refusal must name the size the kernel wanted: {}",
                    err.message
                ),
                other => panic!("a kernel needing push constants must refuse, got {other:?}"),
            }
            assert_eq!(
                sandbox.host_inner().queue_submission_count(),
                submissions_before,
                "a batch that failed while recording submits nothing"
            );

            // The assertion this test exists for: the recorder took the next
            // batch. Without the abort it is still in `Recording`, and
            // `begin()` refuses a recording already in progress.
            let after = handle_run_compute_kernel_batch(
                &sandbox,
                "after-abort".to_string(),
                EscalateRequestRunComputeKernelBatch {
                    dispatches: vec![batched_dispatch(
                        &brighten,
                        ("unbrightened_image", "aborted-seed"),
                        ("brightened_image", "aborted-middle"),
                    )],
                    request_id: "after-abort".to_string(),
                },
            );
            assert!(
                matches!(after, EscalateResponse::Ok(_)),
                "the recorder must be usable after a batch aborted mid-recording: {after:?}"
            );
            assert_every_pixel_is(
                &read_back_rgba8(&sandbox, held[1].texture(), "after-abort-readback"),
                CHAIN_BRIGHTENED_RGBA,
                "the batch that ran after the aborted one",
            );
            drop(held);
        }

        /// An empty batch is not an error — there is simply nothing to submit,
        /// and a caller who opened a scope and dispatched nothing has not made
        /// a mistake worth raising on.
        #[test]
        fn an_empty_batch_submits_nothing_and_is_not_an_error() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("empty batch: no GPU — skipping");
                return;
            };
            let submissions_before = sandbox.host_inner().queue_submission_count();
            let response = handle_run_compute_kernel_batch(
                &sandbox,
                "empty".to_string(),
                EscalateRequestRunComputeKernelBatch {
                    dispatches: Vec::new(),
                    request_id: "empty".to_string(),
                },
            );
            assert!(matches!(response, EscalateResponse::Ok(_)), "{response:?}");
            assert_eq!(
                sandbox.host_inner().queue_submission_count(),
                submissions_before
            );
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

        use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
            EscalateRequestRegisterGraphicsKernelBinding,
            EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttribute,
            EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBinding,
            EscalateRequestRunGraphicsDrawBinding, EscalateRequestRunGraphicsDrawDraw,
            EscalateRequestRunGraphicsDrawIndexBuffer, EscalateRequestRunGraphicsDrawScissor,
            EscalateRequestRunGraphicsDrawVertexBuffer, EscalateRequestRunGraphicsDrawViewport,
        };
        use crate::core::context::{
            GpuContext, GpuContextLimitedAccess, GraphicsKernelBridge, GraphicsKernelRegisterDecl,
            GraphicsKernelRunDraw,
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

            fn run_draw(&self, draw: &GraphicsKernelRunDraw) -> std::result::Result<(), String> {
                if !self
                    .registered
                    .lock()
                    .unwrap()
                    .contains_key(&draw.kernel_id)
                {
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
                fragment_source: "".to_string(),
                vertex_source: "".to_string(),
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
                "req-reg-1",
                "deadbeef",
                "cafebabe",
            ));
            let response =
                handle_escalate_op(&sandbox, &registry, req).expect("must produce a response");
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
                "req-run-1",
                "kernel-x",
                "surface-y",
            ));
            let response =
                handle_escalate_op(&sandbox, &registry, req).expect("must produce a response");
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
                "req-bad-v",
                "xyz123",
                "cafebabe",
            ));
            let response =
                handle_escalate_op(&sandbox, &registry, req).expect("must produce a response");
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
                "req-bad-f",
                "deadbeef",
                "qq",
            ));
            let response =
                handle_escalate_op(&sandbox, &registry, req).expect("must produce a response");
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
                    println!("run_with_invalid_push_constants_hex_returns_err: no GPU — skipping");
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let mut req = make_run_req("req-bad-push", "kernel-x", "surface-y");
            req.push_constants_hex = "xyz".to_string();
            let response =
                handle_escalate_op(&sandbox, &registry, EscalateRequest::RunGraphicsDraw(req))
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
            let response =
                handle_escalate_op(&sandbox, &registry, EscalateRequest::RunGraphicsDraw(req))
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
            assert_eq!(
                id1, id2,
                "identical descriptor must produce the same kernel_id"
            );
        }

        /// The wire accepts GLSL for graphics too, and this is the only test
        /// that the acceptance is wired to anything: it asserts the bridge was
        /// handed real SPIR-V, by its magic number, rather than the text.
        #[test]
        fn glsl_source_reaches_the_graphics_bridge_as_compiled_spirv() {
            let bridge = RecordingGraphicsBridge::new();
            let Some(sandbox) = make_sandbox_with_bridge(Some(bridge.clone())) else {
                println!("glsl_source_reaches_the_graphics_bridge: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let mut req = make_register_req("glsl", "", "");
            req.vertex_source =
                "#version 450\nvoid main() { gl_Position = vec4(0.0); }\n".to_string();
            req.fragment_source = "#version 450\nlayout(location = 0) out vec4 colour;\n\
                                   void main() { colour = vec4(1.0); }\n"
                .to_string();
            let response = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::RegisterGraphicsKernel(req),
            )
            .expect("must produce a response");
            let kernel_id = match response {
                EscalateResponse::Ok(ok) => ok.handle_id,
                other => panic!("expected Ok, got {other:?}"),
            };
            let registered = bridge.registered.lock().unwrap();
            let decl = registered
                .get(&kernel_id)
                .expect("the bridge saw the kernel");
            for (stage, spv) in [
                ("vertex", &decl.vertex_spv),
                ("fragment", &decl.fragment_spv),
            ] {
                assert_eq!(
                    spv.get(..4),
                    Some(&SPIRV_MAGIC_LE[..]),
                    "the {stage} stage reached the bridge as something other than SPIR-V"
                );
            }
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
            assert_ne!(
                id_a, id_b,
                "different vertex SPIR-V must produce different kernel_ids"
            );
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
                BlendFactorWire, BlendOpWire, CullModeWire, DepthCompareOpWire, DepthFormatWire,
                DynamicStateWire, FrontFaceWire, GraphicsBindingKindWire, PolygonModeWire,
                PrimitiveTopologyWire, VertexAttributeFormatWire, VertexInputRateWire,
            };

            let bridge = RecordingGraphicsBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!("pipeline_state_translates_every_enum_arm: no GPU — skipping");
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();

            // Build a request that uses non-default values for every
            // pipeline-state arm we want to lock down. Each value is
            // chosen to be DIFFERENT from the matching default so a
            // wrong arm in the translation would land in the wrong
            // wire-mirror variant and the assertion would fail.
            let mut req = make_register_req("req-translate", "deadbeef", "cafebabe");
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
            req.pipeline_state.attachment_color_formats = vec!["bgra8_unorm_srgb".to_string()];
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
                    println!("run_with_unregistered_kernel_id_returns_err: no GPU — skipping");
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RunGraphicsDraw(make_run_req(
                "req-bad-id",
                "never-registered",
                "surface-y",
            ));
            let response =
                handle_escalate_op(&sandbox, &registry, req).expect("must produce a response");
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

            let response =
                handle_escalate_op(&sandbox, &registry, EscalateRequest::RunGraphicsDraw(run))
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

        use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
            EscalateRequestRegisterAccelerationStructureTlasInstance,
            EscalateRequestRegisterRayTracingKernelBinding,
            EscalateRequestRegisterRayTracingKernelGroup,
            EscalateRequestRegisterRayTracingKernelStage,
            EscalateRequestRunRayTracingKernelBinding,
        };
        use crate::core::context::{
            BlasRegisterDecl, GpuContext, GpuContextLimitedAccess, RAY_TRACING_STAGE_INDEX_NONE,
            RayTracingKernelBridge, RayTracingKernelRegisterDecl, RayTracingKernelRunDispatch,
            TlasRegisterDecl,
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
            let response =
                handle_escalate_op(&sandbox, &registry, req).expect("must produce a response");
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
            let response =
                handle_escalate_op(&sandbox, &registry, req).expect("must produce a response");
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
            let response =
                handle_escalate_op(&sandbox, &registry, req).expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-misaligned-v");
                    assert!(
                        err.message.contains("multiple of 12"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err for misaligned vertex blob, got {other:?}"),
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
            let response =
                handle_escalate_op(&sandbox, &registry, req).expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-misaligned-i");
                    assert!(
                        err.message.contains("multiple of 12"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err for misaligned index blob, got {other:?}"),
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
            let resp1 =
                handle_escalate_op(&sandbox, &registry, req1).expect("must produce a response");
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
            let resp2 =
                handle_escalate_op(&sandbox, &registry, req2).expect("must produce a response");
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
                instances: vec![EscalateRequestRegisterAccelerationStructureTlasInstance {
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
                }],
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
                other => panic!("expected Err for wrong-length transform, got {other:?}"),
            }
            assert_eq!(bridge.tlas_count(), 0);
        }

        #[test]
        fn register_tlas_with_oversized_mask_returns_err() {
            let bridge = RecordingRayTracingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!("register_tlas_with_oversized_mask_returns_err: no GPU — skipping");
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
                    println!("register_tlas_succeeds_after_blas: no GPU — skipping");
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
            let blas_resp =
                handle_escalate_op(&sandbox, &registry, blas_req).expect("must produce a response");
            let blas_id = match blas_resp {
                EscalateResponse::Ok(ok) => ok.handle_id,
                other => panic!("expected Ok for BLAS register, got {other:?}"),
            };
            // 2. Now register a TLAS pointing at it.
            let tlas_req = EscalateRequest::RegisterAccelerationStructureTlas(make_tlas_req(
                "req-tlas", &blas_id,
            ));
            let tlas_resp =
                handle_escalate_op(&sandbox, &registry, tlas_req).expect("must produce a response");
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
                    println!("register_tlas_with_unknown_blas_id_returns_err: no GPU — skipping");
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RegisterAccelerationStructureTlas(make_tlas_req(
                "req-tlas-bad",
                "definitely-not-a-real-blas-id",
            ));
            let response =
                handle_escalate_op(&sandbox, &registry, req).expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-tlas-bad");
                    assert!(
                        err.message.contains("unknown blas_id"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err for unknown blas_id, got {other:?}"),
            }
            assert_eq!(bridge.tlas_count(), 0);
        }

        // ----- Kernel register + run tests ------------------------------

        fn make_kernel_req(request_id: &str) -> EscalateRequestRegisterRayTracingKernel {
            EscalateRequestRegisterRayTracingKernel {
                request_id: request_id.to_string(),
                label: "test-rt-kernel".to_string(),
                stages: vec![
                    EscalateRequestRegisterRayTracingKernelStage {
                        source: "".to_string(),
                        stage: EscalateRequestRegisterRayTracingKernelStageStage::RayGen,
                        spv_hex: "deadbeef".to_string(),
                        entry_point: "main".to_string(),
                    },
                    EscalateRequestRegisterRayTracingKernelStage {
                        source: "".to_string(),
                        stage: EscalateRequestRegisterRayTracingKernelStageStage::Miss,
                        spv_hex: "cafebabe".to_string(),
                        entry_point: "main".to_string(),
                    },
                    EscalateRequestRegisterRayTracingKernelStage {
                        source: "".to_string(),
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

        fn make_run_req(request_id: &str, kernel_id: &str) -> EscalateRequestRunRayTracingKernel {
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
                    println!("register_kernel_without_bridge_returns_err: no GPU — skipping");
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RegisterRayTracingKernel(make_kernel_req("req-k-1"));
            let response =
                handle_escalate_op(&sandbox, &registry, req).expect("must produce a response");
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
                other => panic!("expected Err for bad stage SPIR-V hex, got {other:?}"),
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

        /// Every ray-tracing stage the wire can name maps to the pipeline stage
        /// the compiler builds for. `ray_tracing_pipeline_stage_from_wire` is a
        /// fresh six-arm mapping, and a swapped pair would compile a miss
        /// shader as a closest-hit without complaint — so each arm is driven
        /// through the handler with source only that stage can compile.
        #[test]
        fn every_ray_tracing_wire_stage_compiles_glsl_for_the_stage_it_names() {
            let bridge = RecordingRayTracingBridge::new();
            let Some(sandbox) = make_sandbox_with_bridge(Some(bridge.clone())) else {
                println!("ray-tracing GLSL stage mapping: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            // Each body is legal only in its own stage: `rayPayloadEXT` is
            // raygen-only, `rayPayloadInEXT` is miss/hit-only, and
            // `reportIntersectionEXT` is intersection-only. A mis-mapped arm
            // fails to compile rather than quietly producing the wrong module.
            let stages = [
                (
                    EscalateRequestRegisterRayTracingKernelStageStage::RayGen,
                    "layout(location = 0) rayPayloadEXT vec3 p;\nvoid main() { p = vec3(1.0); }",
                ),
                (
                    EscalateRequestRegisterRayTracingKernelStageStage::Miss,
                    "layout(location = 0) rayPayloadInEXT vec3 p;\nvoid main() { p = vec3(0.0); }",
                ),
                (
                    EscalateRequestRegisterRayTracingKernelStageStage::ClosestHit,
                    "layout(location = 0) rayPayloadInEXT vec3 p;\nvoid main() { p = vec3(0.5); }",
                ),
                (
                    EscalateRequestRegisterRayTracingKernelStageStage::AnyHit,
                    "layout(location = 0) rayPayloadInEXT vec3 p;\nvoid main() { ignoreIntersectionEXT; }",
                ),
                (
                    EscalateRequestRegisterRayTracingKernelStageStage::Intersection,
                    "hitAttributeEXT vec2 a;\nvoid main() { reportIntersectionEXT(1.0, 0u); }",
                ),
                (
                    EscalateRequestRegisterRayTracingKernelStageStage::Callable,
                    "layout(location = 0) callableDataInEXT vec3 c;\nvoid main() { c = vec3(1.0); }",
                ),
            ];
            for (index, (wire_stage, body)) in stages.into_iter().enumerate() {
                let mut req = make_kernel_req(&format!("rt-glsl-{index}"));
                req.stages.truncate(1);
                req.stages[0].stage = wire_stage;
                req.stages[0].spv_hex = String::new();
                req.stages[0].source =
                    format!("#version 460\n#extension GL_EXT_ray_tracing : require\n{body}\n");
                let response = handle_escalate_op(
                    &sandbox,
                    &registry,
                    EscalateRequest::RegisterRayTracingKernel(req),
                )
                .expect("must produce a response");
                let kernel_id = match response {
                    EscalateResponse::Ok(ok) => ok.handle_id,
                    other => panic!("{wire_stage:?} was refused: {other:?}"),
                };
                let kernels = bridge.kernels.lock().unwrap();
                let decl = kernels.get(&kernel_id).expect("the bridge saw the kernel");
                assert_eq!(
                    decl.stages[0].spv.get(..4),
                    Some(&SPIRV_MAGIC_LE[..]),
                    "{wire_stage:?} reached the bridge as something other than SPIR-V"
                );
                assert_eq!(
                    decl.stages[0].stage,
                    ray_tracing_stage_from_wire(wire_stage)
                );
            }
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
            let req1 = EscalateRequest::RegisterRayTracingKernel(make_kernel_req("req-k-a"));
            let resp1 =
                handle_escalate_op(&sandbox, &registry, req1).expect("must produce a response");
            let id1 = match resp1 {
                EscalateResponse::Ok(ok) => ok.handle_id,
                other => panic!("expected Ok, got {other:?}"),
            };
            let req2 = EscalateRequest::RegisterRayTracingKernel(make_kernel_req("req-k-b"));
            let resp2 =
                handle_escalate_op(&sandbox, &registry, req2).expect("must produce a response");
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
            let req = EscalateRequest::RunRayTracingKernel(make_run_req("req-run-1", "kernel-x"));
            let response =
                handle_escalate_op(&sandbox, &registry, req).expect("must produce a response");
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
                other => panic!("expected Err for malformed push hex, got {other:?}"),
            }
            assert!(bridge.runs().is_empty());
        }

        #[test]
        fn run_kernel_with_unknown_kernel_id_returns_err() {
            let bridge = RecordingRayTracingBridge::new();
            let sandbox = match make_sandbox_with_bridge(Some(bridge.clone())) {
                Some(s) => s,
                None => {
                    println!("run_kernel_with_unknown_kernel_id_returns_err: no GPU — skipping");
                    return;
                }
            };
            let registry = EscalateHandleRegistry::new();
            let req = EscalateRequest::RunRayTracingKernel(make_run_req(
                "req-run-x",
                "definitely-not-a-real-kernel-id",
            ));
            let response =
                handle_escalate_op(&sandbox, &registry, req).expect("must produce a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-run-x");
                    assert!(
                        err.message.contains("not registered"),
                        "got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err for unknown kernel_id, got {other:?}"),
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
            let kernel_req = EscalateRequest::RegisterRayTracingKernel(make_kernel_req("req-k"));
            let kernel_resp = handle_escalate_op(&sandbox, &registry, kernel_req)
                .expect("must produce a response");
            let kernel_id = match kernel_resp {
                EscalateResponse::Ok(ok) => ok.handle_id,
                other => panic!("expected Ok for kernel register, got {other:?}"),
            };
            // 2. Now dispatch it.
            let run_req =
                EscalateRequest::RunRayTracingKernel(make_run_req("req-run-k", &kernel_id));
            let run_resp =
                handle_escalate_op(&sandbox, &registry, run_req).expect("must produce a response");
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
            // Lock the per-binding wire→domain conversion: a handler
            // bug that swapped, dropped, or overwrote `target_id`
            // during conversion would slip past the length check
            // alone. The test request used "test-tlas-uuid" for
            // binding 0 (acceleration_structure) and
            // "test-storage-uuid" for binding 1 (storage_image).
            assert_eq!(runs[0].bindings[0].binding, 0);
            assert_eq!(runs[0].bindings[0].target_id, "test-tlas-uuid");
            assert_eq!(
                runs[0].bindings[0].kind,
                RayTracingBindingKindWire::AccelerationStructure
            );
            assert_eq!(runs[0].bindings[1].binding, 1);
            assert_eq!(runs[0].bindings[1].target_id, "test-storage-uuid");
            assert_eq!(
                runs[0].bindings[1].kind,
                RayTracingBindingKindWire::StorageImage
            );
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

        let acquire = EscalateRequest::AcquirePixelBuffer(EscalateRequestAcquirePixelBuffer {
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

        let acquire_tex = EscalateRequest::AcquireTexture(EscalateRequestAcquireTexture {
            request_id: "req-tex".to_string(),
            width: 256,
            height: 128,
            format: "rgba8_unorm".to_string(),
            usage: vec!["texture_binding".to_string(), "copy_src".to_string()],
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
                assert!(
                    !ok.handle_id.is_empty(),
                    "texture handle id should not be empty"
                );
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
            EscalateResponse::Err(err) => {
                panic!("release_handle (texture) failed: {}", err.message)
            }
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

        let release_unknown = EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
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
            LogLevel, RuntimeLogEvent, Source, StreamlibLoggingConfig, StreamlibLoggingGuard,
            init_for_tests,
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

            dispatch_log(sample_log(
                "42",
                "2026-04-23T14:00:00Z",
                EscalateRequestLogLevel::Warn,
            ));

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

        /// Rust and Python emit interleaved records into the unified
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

            // Round-robin emit Rust / Python. The subprocess
            // source carries a monotonic `source_seq`; Rust records do
            // not. A 50µs nap between emissions guarantees `host_ts`
            // strictly increases, which is the stronger property — the
            // contract only requires non-decreasing.
            const ROUNDS: u64 = 16;
            let mut py_seq = 0u64;
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
            }

            drop(guard);

            let events = read_jsonl(&path);

            let merged: Vec<&RuntimeLogEvent> = events
                .iter()
                .filter(|e| {
                    e.message.starts_with("rust-merged") || e.message.starts_with("py-merged-")
                })
                .collect();
            assert_eq!(
                merged.len(),
                (ROUNDS * 2) as usize,
                "expected {} merged-stream records, got {}: {merged:#?}",
                ROUNDS * 2,
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

            // source_seq is monotonic within the subprocess source and
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
            EscalateTransport, spawn_fd_line_reader,
        };
        use crate::core::logging::{
            LogLevel, RuntimeLogEvent, Source, StreamlibLoggingConfig, StreamlibLoggingGuard,
            init_for_tests,
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
                .map(|l| serde_json::from_str::<RuntimeLogEvent>(l).expect("valid JSONL"))
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
                    None => panic!("python subprocess only sends escalate traffic; got {value}"),
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
                    println!("python3 or streamlib-python source missing — skipping");
                    return;
                }
            };
            assert!(frames >= 1, "expected at least one frame, got {frames}");

            drop(guard);

            let events = read_jsonl(&path);
            let record = events
                .iter()
                .find(|e| e.source == Source::Python && e.message == "hi from python")
                .unwrap_or_else(|| panic!("no python record; got {events:#?}"));
            assert_eq!(record.level, LogLevel::Info);
            assert_eq!(record.pipeline_id.as_deref(), Some("pl-test"));
            assert_eq!(record.processor_id.as_deref(), Some("pr-test"));
            assert_eq!(record.attrs.get("count").and_then(|v| v.as_i64()), Some(7));
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
            let mut transport = EscalateTransport::attach(&mut command).expect("attach transport");

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
            let (mut child, _sock) = match spawn_python_with_host_fd_readers(snippet, "pr-fd1") {
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
                .unwrap_or_else(|| panic!("no fd1-intercepted record for python; got {events:#?}"));
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
            let (mut child, _sock) = match spawn_python_with_host_fd_readers(snippet, "pr-fd2") {
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
                .unwrap_or_else(|| panic!("no fd2-intercepted record for python; got {events:#?}"));
            assert_eq!(record.level, LogLevel::Warn);
            assert_eq!(record.processor_id.as_deref(), Some("pr-fd2"));
        }
    }
}
