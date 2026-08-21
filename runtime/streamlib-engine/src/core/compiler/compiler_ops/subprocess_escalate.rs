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
    EscalateComputeBindingKind, EscalateGraphicsBindingKind, EscalateRayTracingBindingKind,
    EscalateRequestAcquireImage, EscalateRequestAcquirePixelBuffer, EscalateRequestAcquireTexture,
    EscalateRequestCopyDeviceExportStagingBackToSurface, EscalateRequestLog,
    EscalateRequestLogLevel, EscalateRequestLogSource, EscalateRequestOpenCpuReadbackStaging,
    EscalateRequestOpenDeviceExportStaging, EscalateRequestRefillDeviceExportStaging,
    EscalateRequestRegisterAccelerationStructureBlas,
    EscalateRequestRegisterAccelerationStructureTlas, EscalateRequestRegisterComputeKernel,
    EscalateRequestRegisterGraphicsKernel, EscalateRequestRegisterGraphicsKernelPipelineState,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState,
    EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode,
    EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace,
    EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode,
    EscalateRequestRegisterGraphicsKernelPipelineStateTopology,
    EscalateRequestRegisterRayTracingKernel, EscalateRequestRegisterRayTracingKernelGroupKind,
    EscalateRequestRegisterRayTracingKernelStageStage, EscalateRequestReleaseHandle,
    EscalateRequestRunComputeKernel, EscalateRequestRunComputeKernelBatch,
    EscalateRequestRunComputeKernelBinding, EscalateRequestRunCpuReadbackCopy,
    EscalateRequestRunCpuReadbackCopyDirection, EscalateRequestRunGraphicsDraw,
    EscalateRequestRunGraphicsDrawDrawKind, EscalateRequestRunRayTracingKernel,
    EscalateRequestTryRunCpuReadbackCopy, EscalateRequestTryRunCpuReadbackCopyDirection,
    EscalateRequestWaitDeviceIdle, RAY_TRACING_STAGE_INDEX_NONE,
};
// Each names a wire field the handler no longer reads: a depth attachment and
// either half of a vertex input are refused, so only the tests that prove the
// refusals still spell them.
#[cfg(test)]
use super::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestRegisterGraphicsKernelPipelineStateAttachmentDepthFormat,
    EscalateRequestRegisterGraphicsKernelPipelineStateDepthCompareOp,
    EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttributeFormat,
    EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBindingInputRate,
};
#[cfg(target_os = "linux")]
use super::subprocess_escalate_wire_types::escalate_response::EscalateResponseKernelBinding;
use super::subprocess_escalate_wire_types::escalate_response::{
    EscalateResponseContended, EscalateResponseErr, EscalateResponseOk,
};
use super::subprocess_escalate_wire_types::{EscalateRequest, EscalateResponse};
use crate::core::context::GpuContextLimitedAccess;
#[cfg(target_os = "linux")]
use crate::core::context::SurfaceExportStagingResidency;
#[cfg(target_os = "linux")]
use crate::core::context::TextureRegistration;
use crate::core::context::{
    PooledTextureHandle, TextureCrossProcessImportability, TexturePoolDescriptor,
};
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

impl RegisteredHandle {
    /// Whether releasing this handle also owes the parent's texture-cache
    /// entry an eviction — textures and images enter it at acquire, pixel
    /// buffers never do.
    pub(crate) fn is_texture_backed(&self) -> bool {
        match self {
            Self::PixelBuffer(_) => false,
            Self::Texture { .. } => true,
            #[cfg(target_os = "linux")]
            Self::Image { .. } => true,
        }
    }
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

    /// Remove a handle by id, handing back what was held so the caller can
    /// pair its removal with the kind-specific cleanup. `None` when the id
    /// was unknown. Used by the escalate `release_handle` path.
    pub(crate) fn remove_handle(&self, handle_id: &str) -> Option<RegisteredHandle> {
        let mut map = self.handles.lock().expect("poisoned");
        map.remove(handle_id)
    }

    /// Take every held handle, ids included, so a teardown path can run the
    /// same kind-specific cleanup the explicit release path does.
    pub(crate) fn drain_handles(&self) -> Vec<(String, RegisteredHandle)> {
        let mut map = self.handles.lock().expect("poisoned");
        map.drain().collect()
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
            #[cfg(target_os = "linux")]
            let acquired = sandbox.escalate(|full| {
                // The importability flavor is derived engine-side from the
                // request — there is no Python dial for it, and a flavor the
                // request cannot take falls back to NotImportable so a later
                // import refuses by name instead of the acquire failing.
                let desc = TexturePoolDescriptor::new(width, height, parsed_format)
                    .with_usage(parsed_usage)
                    .with_cross_process_importability(match full.host_vulkan_device_arc() {
                        Ok(device) => derive_texture_cross_process_importability(
                            parsed_format,
                            parsed_usage,
                            device.has_render_target_modifier_for_texture_format(parsed_format),
                            device.opaque_fd_image_pool().is_some(),
                        ),
                        Err(_) => TextureCrossProcessImportability::NotImportable,
                    });
                let texture = full.acquire_texture(&desc)?;
                let (handle_id, produce_done, consume_done) =
                    assign_texture_handle_id(full, &texture)?;
                // The parent answers its own binding resolutions from the
                // texture cache — without this entry it would re-import its
                // own allocation through the surface-share socket, a path
                // that cannot rebuild every flavour and re-interprets the
                // ones it can.
                full.register_texture(&handle_id, texture.texture_clone());
                Ok((handle_id, texture, produce_done, consume_done))
            });
            #[cfg(not(target_os = "linux"))]
            let acquired = sandbox.escalate(|full| {
                let desc = TexturePoolDescriptor::new(width, height, parsed_format)
                    .with_usage(parsed_usage);
                let texture = full.acquire_texture(&desc)?;
                let (handle_id,) = assign_texture_handle_id(full, &texture)?;
                full.register_texture(&handle_id, texture.texture_clone());
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
                        format: Some(parsed_format.wire_name().to_string()),
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
                        format: Some(parsed_format.wire_name().to_string()),
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
                            format: Some(parsed_format.wire_name().to_string()),
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
            let removed_handle = registry.remove_handle(&handle_id);
            let removed = removed_handle.is_some();
            if let Some(removed_handle) = removed_handle {
                // Pixel-buffer / texture / image acquires were
                // checked into the surface-share service under the
                // returned handle_id; pair the registry eviction
                // with the matching service release.
                release_surface_share_surface(sandbox, &handle_id);
                // Texture and image acquires also entered the parent's
                // same-process texture cache; `unregister_texture` removes
                // that entry and tears down the surface's export stagings
                // with it. Scoped to texture-backed handles so a buffer
                // release keeps its staging lifetime unchanged.
                if removed_handle.is_texture_backed() {
                    sandbox.unregister_texture(&handle_id);
                }
            }
            // An acceleration structure is registered against `GpuContext`
            // rather than against the per-subprocess handle registry, so its id
            // reaches the same release verb through the device gate.
            let released = removed || release_acceleration_structure(sandbox, &handle_id);
            Some(if released {
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
            // kind each name is, so the shape goes back with the id.
            let bindings = kernel
                .bindings()
                .iter()
                .map(|spec| {
                    reflected_kernel_binding_response(
                        &kernel_id,
                        spec.binding,
                        compute_binding_kind_to_wire(spec.kind).wire_name(),
                        spec.name.as_deref(),
                    )
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
use crate::core::rhi::SurfaceBoundKernelBindingKind;
#[cfg(target_os = "linux")]
use crate::host_rhi::HostTextureExt as _;

/// What one validated binding resolved to: the slot to write, the kind to
/// write it as, and the surface to look up.
#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
struct PlannedComputeBinding<'a> {
    binding: u32,
    kind: SurfaceBoundKernelBindingKind,
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
            ComputeBindingKind::StorageImage => SurfaceBoundKernelBindingKind::StorageImage,
            ComputeBindingKind::SampledTexture => SurfaceBoundKernelBindingKind::SampledTexture,
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

/// Publish each bound surface's post-dispatch layout to the surface-share
/// service, so a cross-process consumer's checkout names the layout the
/// dispatch actually left the image in — the service cell is otherwise
/// frozen at its registration-time UNDEFINED while the in-process
/// registration moves on.
///
/// Best-effort, escalate-path only: an id the service does not hold is an
/// in-process-only surface, not an error, and a publish failure costs the
/// consumer its content-preserving acquire, never the dispatch.
#[cfg(target_os = "linux")]
fn publish_bound_surface_layouts_to_surface_share(
    full: &crate::core::context::GpuContextFullAccess,
    bound_surfaces: &[(String, TextureRegistration)],
) {
    let Some(store) = full.surface_store() else {
        return;
    };
    let mut published_surface_ids: Vec<&str> = Vec::with_capacity(bound_surfaces.len());
    for (surface_id, registration) in bound_surfaces {
        if published_surface_ids.contains(&surface_id.as_str()) {
            continue;
        }
        published_surface_ids.push(surface_id);
        if let Err(publish_failure) =
            store.update_image_layout(surface_id, registration.current_layout())
        {
            tracing::debug!(
                "[escalate] layout publish for '{}' skipped: {}",
                surface_id,
                publish_failure
            );
        }
    }
}

/// One resolved compute binding carried with the surface id it named, so
/// the transition and the layout publish pair by construction rather than
/// by a shared index — the desynchronisation rule
/// [`ResolvedSurfaceBoundKernelBinding`] documents.
#[cfg(target_os = "linux")]
struct ResolvedComputeKernelDispatchBindingWithSurfaceId {
    surface_id: String,
    dispatch_binding: BatchedComputeKernelDispatchBinding,
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
) -> crate::core::error::Result<Vec<ResolvedComputeKernelDispatchBindingWithSurfaceId>> {
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
        resolved.push(ResolvedComputeKernelDispatchBindingWithSurfaceId {
            surface_id: binding.target_id.to_string(),
            dispatch_binding: BatchedComputeKernelDispatchBinding {
                binding: binding.binding,
                kind: binding.kind,
                registration,
            },
        });
    }

    // One image cannot serve two kinds in one dispatch. The descriptor layouts
    // are fixed and disagree — a combined image sampler is written
    // SHADER_READ_ONLY_OPTIMAL and a storage image GENERAL — so whatever layout
    // the texture is put in, one of the two descriptors is wrong.
    //
    // Compared after resolution and on the image, not on the id the caller
    // wrote: a published frame id and its pool slot are two spellings of one
    // texture (`<slot>#<generation>` resolves through the same cache entry as
    // `<slot>`), so a string comparison would let the pair through to exactly
    // the dispatch this refuses.
    for (index, (binding, plan)) in resolved.iter().zip(&planned).enumerate() {
        // A texture carrying no image is its own error, raised where the
        // descriptor would be written. Skipped rather than compared, because
        // two absent images are not one texture and refusing them here would
        // send the caller looking for a duplicate they did not write.
        let Some(image) = binding
            .dispatch_binding
            .registration
            .texture()
            .vulkan_inner()
            .image()
        else {
            continue;
        };
        let clashing = resolved[..index].iter().zip(&planned).find(|(prior, _)| {
            prior.dispatch_binding.kind != binding.dispatch_binding.kind
                && prior
                    .dispatch_binding
                    .registration
                    .texture()
                    .vulkan_inner()
                    .image()
                    == Some(image)
        });
        if let Some((prior, prior_plan)) = clashing {
            // Both ids, as the caller wrote them: a published frame id and its
            // pool slot are different strings for one texture, so naming only
            // one would leave the reader looking for a duplicate that is not
            // there on the page.
            return Err(Error::GpuError(format!(
                "bindings `{}` (surface {:?}) and `{}` (surface {:?}) name one texture but \
                 as {:?} and {:?}; no image layout satisfies both descriptors, so a \
                 dispatch reads and writes different surfaces or binds one of them alone",
                prior_plan.name,
                prior_plan.target_id,
                plan.name,
                plan.target_id,
                prior.dispatch_binding.kind,
                binding.dispatch_binding.kind
            )));
        }
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
    // `VulkanComputeKernel::dispatch` records no image barrier of its own,
    // so without this the bound images run in whatever layout their last
    // producer left — and their registrations, and everything published from
    // them, would keep claiming it.
    let transition_pairs: Vec<(&TextureRegistration, crate::core::rhi::VulkanLayout)> = resolved
        .iter()
        .map(|binding| {
            (
                &binding.dispatch_binding.registration,
                binding.dispatch_binding.kind.required_image_layout(),
            )
        })
        .collect();
    transition_bound_kernel_inputs_into_descriptor_layouts(
        full,
        "escalate_compute_dispatch_input_layouts",
        crate::vulkan::rhi::VulkanStage::COMPUTE_SHADER,
        &transition_pairs,
    )?;
    drop(transition_pairs);
    for binding in &resolved {
        binding.dispatch_binding.write_into_kernel(kernel)?;
    }

    // A kernel that declares push constants must be given them even when the
    // payload is empty, so `set_push_constants` produces the size mismatch
    // rather than the dispatch running against whatever the kernel's staged
    // buffer last held.
    if kernel.push_constant_size() > 0 || !push_constants.is_empty() {
        kernel.set_push_constants(push_constants)?;
    }

    let dispatched = kernel.dispatch(req.group_count_x, req.group_count_y, req.group_count_z);
    if dispatched.is_ok() {
        let bound_surfaces: Vec<(String, TextureRegistration)> = resolved
            .iter()
            .map(|binding| {
                (
                    binding.surface_id.clone(),
                    binding.dispatch_binding.registration.clone(),
                )
            })
            .collect();
        publish_bound_surface_layouts_to_surface_share(full, &bound_surfaces);
    }
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

    let mut bound_surfaces_across_the_batch: Vec<(String, TextureRegistration)> = Vec::new();
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
        let resolved = resolve_supplied_compute_bindings(full, &dispatch.bindings, &kernel)
            .map_err(|e| Error::GpuError(format!("dispatch {index} of this batch: {e}")))?;
        bound_surfaces_across_the_batch.extend(resolved.iter().map(|binding| {
            (
                binding.surface_id.clone(),
                binding.dispatch_binding.registration.clone(),
            )
        }));
        let bindings = resolved
            .into_iter()
            .map(|binding| binding.dispatch_binding)
            .collect();
        batch.push(BatchedComputeKernelDispatch {
            kernel,
            bindings,
            push_constants,
            group_count_x: dispatch.group_count_x,
            group_count_y: dispatch.group_count_y,
            group_count_z: dispatch.group_count_z,
        });
    }

    let dispatched = full.dispatch_compute_kernel_batch(&batch);
    if dispatched.is_ok() {
        publish_bound_surface_layouts_to_surface_share(full, &bound_surfaces_across_the_batch);
    }
    dispatched
}

/// One binding a draw or a trace supplied, as the planner reads it — whichever
/// wire array it arrived in.
#[cfg(target_os = "linux")]
struct SuppliedKernelBindingUnderPlanning<'a> {
    name: &'a str,
    target_id: &'a str,
    kind_wire_name: &'static str,
}

/// One binding a kernel declares, as the planner reads it.
#[cfg(target_os = "linux")]
struct DeclaredKernelBindingUnderPlanning<'a> {
    binding_slot: u32,
    /// `None` on a binding reflection left unnamed, which nothing can resolve
    /// by name.
    name: Option<&'a str>,
    kind_wire_name: &'static str,
    /// `None` for a kind no surface can be named for — buffers, and the
    /// acceleration structure a trace resolves through its own registry.
    surface_bound_kind: Option<SurfaceBoundKernelBindingKind>,
}

/// What one validated binding resolved to: the slot to write, the kind to write
/// it as, and the surface to look up.
#[cfg(target_os = "linux")]
struct PlannedSurfaceBoundKernelBinding<'a> {
    binding_slot: u32,
    kind: SurfaceBoundKernelBindingKind,
    name: &'a str,
    target_id: &'a str,
}

/// One planned binding carried together with the device texture it names.
///
/// The pair travels as one value rather than as two collections read at a
/// shared index: every step after resolution — the kind-clash check, the
/// pre-run barrier, the colour-target check, the `set_*` calls — needs the plan
/// and the registration together, and a shared index is a desynchronisation
/// waiting to be introduced.
#[cfg(target_os = "linux")]
struct ResolvedSurfaceBoundKernelBinding<'a> {
    planned: PlannedSurfaceBoundKernelBinding<'a>,
    registration: TextureRegistration,
}

/// Refuse one binding name supplied twice in a single run's wire array.
///
/// Shared with the trace path, which runs this over the whole array before
/// splitting the acceleration structures out of it — the planner never sees
/// those, and one rule reads as one message wherever it fires. The names it saw
/// come back, so a caller's missing-binding check does not walk the array again.
#[cfg(target_os = "linux")]
fn refuse_a_kernel_binding_name_supplied_twice<'a>(
    invocation_noun: &str,
    supplied_names: impl IntoIterator<Item = &'a str>,
    declared_names: &[&str],
) -> crate::core::error::Result<std::collections::HashSet<&'a str>> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for name in supplied_names {
        if !seen.insert(name) {
            return Err(crate::core::error::Error::GpuError(format!(
                "binding `{name}` was supplied twice; this kernel declares {}, each supplied \
                 exactly once per {invocation_noun}",
                crate::core::rhi::quote_declared_shader_binding_names(declared_names)
            )));
        }
    }
    Ok(seen)
}

/// Match a draw's or a trace's supplied bindings against the kernel's declared
/// ones.
///
/// The graphics and ray-tracing twin of [`plan_supplied_compute_bindings`],
/// with the same rules: every failure raises before any resource is bound and
/// long before a submission, and every message names the kernel's own bindings.
/// Bindings do not persist on a kernel, so one run supplies all of them or
/// none. `invocation_noun` is what one run of this pipeline kind is called, so
/// the refusals read as the caller's op does.
#[cfg(target_os = "linux")]
fn plan_supplied_surface_bound_kernel_bindings<'a>(
    invocation_noun: &str,
    supplied: &[SuppliedKernelBindingUnderPlanning<'a>],
    declared: &[DeclaredKernelBindingUnderPlanning<'a>],
) -> crate::core::error::Result<Vec<PlannedSurfaceBoundKernelBinding<'a>>> {
    use crate::core::error::Error;

    let declared_names: Vec<&str> = declared.iter().filter_map(|d| d.name).collect();
    // Built only when a refusal fires — this runs per frame, and the happy
    // path should not pay for the error text.
    let kernel_declares = || crate::core::rhi::quote_declared_shader_binding_names(&declared_names);

    let seen = refuse_a_kernel_binding_name_supplied_twice(
        invocation_noun,
        supplied.iter().map(|entry| entry.name),
        &declared_names,
    )?;

    for name in &declared_names {
        if !seen.contains(name) {
            return Err(Error::GpuError(format!(
                "binding `{name}` was not supplied; bindings do not persist between \
                 {invocation_noun}s, so every {invocation_noun} supplies all of {}",
                kernel_declares()
            )));
        }
    }

    let mut planned = Vec::with_capacity(supplied.len());
    for entry in supplied {
        let declaration = declared
            .iter()
            .find(|d| d.name == Some(entry.name))
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "binding `{}` is not one this kernel declares; it declares {}",
                    entry.name,
                    kernel_declares()
                ))
            })?;
        if declaration.kind_wire_name != entry.kind_wire_name {
            return Err(Error::GpuError(format!(
                "binding `{}` was supplied as {} but this kernel declares it {}",
                entry.name, entry.kind_wire_name, declaration.kind_wire_name
            )));
        }
        let kind = declaration.surface_bound_kind.ok_or_else(|| {
            Error::GpuError(format!(
                "binding `{}` is {}, which a {invocation_noun} cannot name a surface for — the \
                 surface-backed kinds are storage_image and sampled_texture",
                entry.name, declaration.kind_wire_name
            ))
        })?;
        planned.push(PlannedSurfaceBoundKernelBinding {
            binding_slot: declaration.binding_slot,
            kind,
            name: entry.name,
            target_id: entry.target_id,
        });
    }
    Ok(planned)
}

/// Resolve every planned binding to the device texture it names, keeping the
/// two together.
///
/// Each registration is a refcount on the texture its descriptor will point at
/// — the caller holds them across the submission, because dropping the last one
/// before the GPU has run frees the image out from under it.
#[cfg(target_os = "linux")]
fn resolve_planned_surface_bound_kernel_bindings<'a>(
    full: &crate::core::context::GpuContextFullAccess,
    planned: Vec<PlannedSurfaceBoundKernelBinding<'a>>,
) -> crate::core::error::Result<Vec<ResolvedSurfaceBoundKernelBinding<'a>>> {
    use crate::core::error::Error;

    let mut resolved = Vec::with_capacity(planned.len());
    for binding in planned {
        // Zero extent: a kernel binding names a surface the graph already has
        // as a device texture, which resolves from the same-process cache or
        // the surface-share service. The pixel-buffer fallback is the one path
        // that consults the extent, and it refuses a zero one — a
        // buffer-backed surface is not something a draw can bind.
        let registration = full
            .resolve_texture_registration_by_surface_id(binding.target_id, None, 0, 0)
            .map_err(|e| {
                Error::GpuError(format!(
                    "binding `{}` names surface {:?}, which this graph cannot resolve to a \
                     device texture: {e}",
                    binding.name, binding.target_id
                ))
            })?;
        resolved.push(ResolvedSurfaceBoundKernelBinding {
            planned: binding,
            registration,
        });
    }

    // One image cannot serve two kinds in one run. The descriptor layouts are
    // fixed and disagree — a combined image sampler is written
    // SHADER_READ_ONLY_OPTIMAL and a storage image GENERAL — so whatever layout
    // the texture is put in, one of the two descriptors is wrong.
    //
    // Compared after resolution and on the image, not on the id the caller
    // wrote: a published frame id and its pool slot are two spellings of one
    // texture (`<slot>#<generation>` resolves through the same cache entry as
    // `<slot>`), so a string comparison would let the pair through to exactly
    // the run this refuses.
    for (index, binding) in resolved.iter().enumerate() {
        // A texture carrying no image is its own error, raised where the
        // descriptor would be written. Skipped rather than compared, because
        // two absent images are not one texture and refusing them here would
        // send the caller looking for a duplicate they did not write.
        let Some(image) = binding.registration.texture().vulkan_inner().image() else {
            continue;
        };
        let clashing = resolved[..index].iter().find(|prior| {
            prior.planned.kind != binding.planned.kind
                && prior.registration.texture().vulkan_inner().image() == Some(image)
        });
        if let Some(prior) = clashing {
            // Both ids, as the caller wrote them: a published frame id and its
            // pool slot are different strings for one texture, so naming only
            // one would leave the reader looking for a duplicate that is not
            // there on the page.
            return Err(Error::GpuError(format!(
                "bindings `{}` (surface {:?}) and `{}` (surface {:?}) name one texture but as \
                 {:?} and {:?}; no image layout satisfies both descriptors, so this run reads \
                 and writes different surfaces or binds one of them alone",
                prior.planned.name,
                prior.planned.target_id,
                binding.planned.name,
                binding.planned.target_id,
                prior.planned.kind,
                binding.planned.kind
            )));
        }
    }
    Ok(resolved)
}

/// Barrier every bound input into the layout its descriptor requires, and
/// publish the layout each one landed in.
///
/// Neither `VulkanComputeKernel::dispatch`, `VulkanGraphicsKernel::
/// offscreen_render` nor `VulkanRayTracingKernel::trace_rays` barriers a
/// bound input — the draw path transitions its colour targets and nothing
/// else — so a surface arriving in the wrong layout would be read or written
/// through a descriptor its layout does not satisfy, and its registration
/// would keep claiming a layout the run has left behind. A run whose inputs
/// already sit in the right layout records nothing and mints no command
/// buffer.
#[cfg(target_os = "linux")]
fn transition_bound_kernel_inputs_into_descriptor_layouts(
    full: &crate::core::context::GpuContextFullAccess,
    recorder_label: &str,
    consuming_stage: crate::vulkan::rhi::VulkanStage,
    bound_inputs: &[(&TextureRegistration, crate::core::rhi::VulkanLayout)],
) -> crate::core::error::Result<()> {
    use crate::vulkan::rhi::{VulkanAccess, VulkanStage};

    let mut images_already_barriered = Vec::new();
    let mut bindings_to_barrier = Vec::new();
    for (registration, required_layout) in bound_inputs {
        if registration.current_layout() == *required_layout {
            continue;
        }
        // One texture bound at two slots is one image and one barrier — a
        // second would name an oldLayout the first has already left, and the
        // two slots agree on the layout anyway or the kind clash would have
        // been refused already.
        let image = registration.texture().vulkan_inner().image();
        if images_already_barriered.contains(&image) {
            continue;
        }
        images_already_barriered.push(image);
        bindings_to_barrier.push((registration, *required_layout));
    }
    if bindings_to_barrier.is_empty() {
        return Ok(());
    }

    let mut recorder = full.create_command_recorder(recorder_label)?;
    recorder.begin()?;
    for (registration, required_layout) in &bindings_to_barrier {
        // Whatever wrote this surface before the run is not this run's to know
        // — a transfer upload, a camera, another node — so the source scope is
        // the wide one every other entry-from-an-unknown-producer barrier in
        // the engine uses.
        let recorded = recorder.record_image_barrier(
            registration.texture(),
            registration.current_layout(),
            *required_layout,
            VulkanStage::ALL_COMMANDS,
            consuming_stage,
            VulkanAccess::MEMORY_WRITE,
            VulkanAccess::SHADER_READ | VulkanAccess::SHADER_WRITE,
        );
        if let Err(e) = recorded {
            recorder.abort_recording();
            return Err(e);
        }
    }
    recorder.submit_and_wait()?;
    // Published for every binding, not just the ones that were barriered: a
    // cross-process import synthesizes a fresh registration per resolve, so two
    // slots naming one surface hold two layout cells for the one image.
    for (registration, required_layout) in bound_inputs {
        registration.update_layout(*required_layout);
    }
    Ok(())
}

/// The `(registration, required layout)` pairs the transition fn consumes,
/// derived from planner-resolved bindings.
#[cfg(target_os = "linux")]
fn descriptor_layout_transition_pairs<'a>(
    bound_inputs: &'a [ResolvedSurfaceBoundKernelBinding<'_>],
) -> Vec<(&'a TextureRegistration, crate::core::rhi::VulkanLayout)> {
    bound_inputs
        .iter()
        .map(|binding| {
            (
                &binding.registration,
                binding.planned.kind.required_image_layout(),
            )
        })
        .collect()
}

/// The `(surface id, registration)` pairs the post-dispatch layout publish
/// consumes, from planner-resolved bindings — paired by construction, never
/// by a shared index.
#[cfg(target_os = "linux")]
fn bound_surface_layout_publish_pairs(
    bound_inputs: &[ResolvedSurfaceBoundKernelBinding<'_>],
) -> Vec<(String, TextureRegistration)> {
    bound_inputs
        .iter()
        .map(|binding| {
            (
                binding.planned.target_id.to_string(),
                binding.registration.clone(),
            )
        })
        .collect()
}

/// One reflected binding as a register response spells it.
///
/// Every binding a registered kernel holds came through reflection, which
/// refuses an unnamed one — an absent name here is a broken invariant, not a
/// case to skip over.
#[cfg(target_os = "linux")]
fn reflected_kernel_binding_response(
    kernel_id: &str,
    binding_slot: u32,
    kind_wire_name: &str,
    name: Option<&str>,
) -> crate::core::error::Result<EscalateResponseKernelBinding> {
    Ok(EscalateResponseKernelBinding {
        kind: kind_wire_name.to_string(),
        name: name
            .ok_or_else(|| {
                crate::core::error::Error::GpuError(format!(
                    "kernel {kernel_id} holds an unnamed binding at slot {binding_slot}; \
                     reflection refuses these, so this kernel did not come through registration"
                ))
            })?
            .to_string(),
    })
}

/// The RHI binding kind a graphics wire enum names.
#[cfg(target_os = "linux")]
fn graphics_binding_kind_from_wire(
    kind: EscalateGraphicsBindingKind,
) -> crate::core::rhi::GraphicsBindingKind {
    use crate::core::rhi::GraphicsBindingKind;
    match kind {
        EscalateGraphicsBindingKind::SampledTexture => GraphicsBindingKind::SampledTexture,
        EscalateGraphicsBindingKind::StorageBuffer => GraphicsBindingKind::StorageBuffer,
        EscalateGraphicsBindingKind::StorageImage => GraphicsBindingKind::StorageImage,
        EscalateGraphicsBindingKind::UniformBuffer => GraphicsBindingKind::UniformBuffer,
    }
}

/// The wire enum for a graphics binding kind.
#[cfg(target_os = "linux")]
fn graphics_binding_kind_to_wire(
    kind: crate::core::rhi::GraphicsBindingKind,
) -> EscalateGraphicsBindingKind {
    use crate::core::rhi::GraphicsBindingKind;
    match kind {
        GraphicsBindingKind::SampledTexture => EscalateGraphicsBindingKind::SampledTexture,
        GraphicsBindingKind::StorageBuffer => EscalateGraphicsBindingKind::StorageBuffer,
        GraphicsBindingKind::StorageImage => EscalateGraphicsBindingKind::StorageImage,
        GraphicsBindingKind::UniformBuffer => EscalateGraphicsBindingKind::UniformBuffer,
    }
}

/// Whether a graphics binding kind is one a draw can name a surface for.
#[cfg(target_os = "linux")]
fn surface_bound_graphics_binding_kind(
    kind: crate::core::rhi::GraphicsBindingKind,
) -> Option<SurfaceBoundKernelBindingKind> {
    use crate::core::rhi::GraphicsBindingKind;
    match kind {
        GraphicsBindingKind::SampledTexture => Some(SurfaceBoundKernelBindingKind::SampledTexture),
        GraphicsBindingKind::StorageImage => Some(SurfaceBoundKernelBindingKind::StorageImage),
        GraphicsBindingKind::StorageBuffer | GraphicsBindingKind::UniformBuffer => None,
    }
}

/// The RHI binding kind a ray-tracing wire enum names.
#[cfg(target_os = "linux")]
fn ray_tracing_binding_kind_from_wire(
    kind: EscalateRayTracingBindingKind,
) -> crate::core::rhi::RayTracingBindingKind {
    use crate::core::rhi::RayTracingBindingKind;
    match kind {
        EscalateRayTracingBindingKind::AccelerationStructure => {
            RayTracingBindingKind::AccelerationStructure
        }
        EscalateRayTracingBindingKind::SampledTexture => RayTracingBindingKind::SampledTexture,
        EscalateRayTracingBindingKind::StorageBuffer => RayTracingBindingKind::StorageBuffer,
        EscalateRayTracingBindingKind::StorageImage => RayTracingBindingKind::StorageImage,
        EscalateRayTracingBindingKind::UniformBuffer => RayTracingBindingKind::UniformBuffer,
    }
}

/// The wire enum for a ray-tracing binding kind.
#[cfg(target_os = "linux")]
fn ray_tracing_binding_kind_to_wire(
    kind: crate::core::rhi::RayTracingBindingKind,
) -> EscalateRayTracingBindingKind {
    use crate::core::rhi::RayTracingBindingKind;
    match kind {
        RayTracingBindingKind::AccelerationStructure => {
            EscalateRayTracingBindingKind::AccelerationStructure
        }
        RayTracingBindingKind::SampledTexture => EscalateRayTracingBindingKind::SampledTexture,
        RayTracingBindingKind::StorageBuffer => EscalateRayTracingBindingKind::StorageBuffer,
        RayTracingBindingKind::StorageImage => EscalateRayTracingBindingKind::StorageImage,
        RayTracingBindingKind::UniformBuffer => EscalateRayTracingBindingKind::UniformBuffer,
    }
}

/// Whether a ray-tracing binding kind is one a trace can name a surface for.
///
/// The acceleration structure is excluded because a trace resolves it through
/// the acceleration-structure registry, not through a surface.
#[cfg(target_os = "linux")]
fn surface_bound_ray_tracing_binding_kind(
    kind: crate::core::rhi::RayTracingBindingKind,
) -> Option<SurfaceBoundKernelBindingKind> {
    use crate::core::rhi::RayTracingBindingKind;
    match kind {
        RayTracingBindingKind::SampledTexture => {
            Some(SurfaceBoundKernelBindingKind::SampledTexture)
        }
        RayTracingBindingKind::StorageImage => Some(SurfaceBoundKernelBindingKind::StorageImage),
        RayTracingBindingKind::AccelerationStructure
        | RayTracingBindingKind::StorageBuffer
        | RayTracingBindingKind::UniformBuffer => None,
    }
}

/// Everything a `register_graphics_kernel` settles before it takes the device
/// gate: both stages compiled, the declaration read, the pipeline state
/// flattened.
#[cfg(target_os = "linux")]
struct PreparedGraphicsKernelRegistration {
    label: String,
    vertex_spv: Arc<[u8]>,
    fragment_spv: Arc<[u8]>,
    vertex_entry_point: String,
    fragment_entry_point: String,
    declared_bindings: Vec<crate::core::rhi::GraphicsBindingDeclaration>,
    push_constants: crate::core::rhi::GraphicsPushConstants,
    pipeline_state: crate::core::rhi::GraphicsPipelineState,
    descriptor_sets_in_flight: u32,
}

/// Read a `register_graphics_kernel` request into what `GpuContext` builds a
/// kernel from, without touching the device.
///
/// Compilation is CPU work, and the escalate gate it would otherwise be holding
/// serializes every processor's device work — the same reason
/// [`RegisteredShaderStageSource::spirv`] takes the sandbox rather than a
/// `GpuContextFullAccess`.
#[cfg(target_os = "linux")]
fn prepare_graphics_kernel_registration(
    sandbox: &GpuContextLimitedAccess,
    req: EscalateRequestRegisterGraphicsKernel,
) -> std::result::Result<PreparedGraphicsKernelRegistration, String> {
    use crate::core::rhi::{
        GraphicsBindingDeclaration, GraphicsPushConstants, GraphicsShaderStageFlags,
    };

    let vertex_source = registered_shader_stage_source(
        "vertex_",
        &req.vertex_source,
        &req.vertex_spv_hex,
        GlslCompilationTargetStage::Vertex,
        &req.vertex_entry_point,
    )?;
    let fragment_source = registered_shader_stage_source(
        "fragment_",
        &req.fragment_source,
        &req.fragment_spv_hex,
        GlslCompilationTargetStage::Fragment,
        &req.fragment_entry_point,
    )?;
    let vertex_spv = vertex_source.spirv(sandbox).map_err(|e| e.to_string())?;
    let fragment_spv = fragment_source.spirv(sandbox).map_err(|e| e.to_string())?;

    let mut declared_bindings = Vec::with_capacity(req.bindings.len());
    for wire in &req.bindings {
        declared_bindings.push(GraphicsBindingDeclaration {
            name: wire.name.clone(),
            kind: graphics_binding_kind_from_wire(wire.kind),
            stages: GraphicsShaderStageFlags::from_bits(wire.stages).ok_or_else(|| {
                format!(
                    "binding `{}` names stages {:#b}, which sets a bit no graphics stage owns \
                     (1 = vertex, 2 = fragment)",
                    wire.name, wire.stages
                )
            })?,
        });
    }

    let push_constants = GraphicsPushConstants {
        size: req.push_constant_size,
        stages: GraphicsShaderStageFlags::from_bits(req.push_constant_stages).ok_or_else(|| {
            format!(
                "push_constant_stages {:#b} sets a bit no graphics stage owns (1 = vertex, \
                 2 = fragment)",
                req.push_constant_stages
            )
        })?,
    };

    let pipeline_state = graphics_pipeline_state_from_wire(req.pipeline_state)
        .map_err(|e| format!("pipeline_state: {e}"))?;

    Ok(PreparedGraphicsKernelRegistration {
        label: req.label,
        vertex_spv,
        fragment_spv,
        vertex_entry_point: vertex_source.entry_point().to_string(),
        fragment_entry_point: fragment_source.entry_point().to_string(),
        declared_bindings,
        push_constants,
        pipeline_state,
        descriptor_sets_in_flight: req.descriptor_sets_in_flight,
    })
}

/// Build a graphics kernel for a subprocess customer, against `GpuContext`.
///
/// The graphics twin of [`handle_register_compute_kernel`]: reflection over
/// both stages derives the binding shape and its names, the request's own
/// declaration is checked against it rather than replacing it, and
/// re-registering an identical kernel is a cache hit that answers with the same
/// `kernel_id`.
///
/// Each stage arrives as GLSL `*_source` the engine compiles, or as the
/// pre-compiled `*_spv_hex` escape hatch.
///
/// Failure modes (each an [`EscalateResponse::Err`] keyed by the request_id):
/// 1. A stage supplies neither `*_source` nor `*_spv_hex`, or both; its source
///    does not compile; or its hex doesn't decode.
/// 2. A binding's or the push-constant range's `stages` mask sets a bit no
///    graphics stage owns.
/// 3. `pipeline_state` names a shape a draw cannot run — MSAA, other than
///    exactly one colour attachment, `depth_stencil_enabled` or an
///    `attachment_depth_format` (the offscreen pass a draw runs through
///    attaches colour targets only), a `vertex_input_bindings` or
///    `vertex_input_attributes` entry (no escalate op mints a `VertexBuffer` to
///    fill one), an unknown colour format, or a write mask no channel owns.
/// 4. The blobs' `OpName` decorations were stripped — bindings resolve by name,
///    so an unnamed binding cannot be bound at all.
/// 5. The declaration disagrees with reflection on a name, a kind, or a stage.
/// 6. Push-constant size mismatch, or pipeline build failure.
#[cfg(target_os = "linux")]
fn handle_register_graphics_kernel(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRegisterGraphicsKernel,
) -> EscalateResponse {
    use crate::core::rhi::{GraphicsShaderStage, GraphicsStage};

    let prepared = match prepare_graphics_kernel_registration(sandbox, req) {
        Ok(prepared) => prepared,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("register_graphics_kernel: {e}"),
            });
        }
    };

    let stages = [
        GraphicsStage {
            stage: GraphicsShaderStage::Vertex,
            spv: &prepared.vertex_spv,
            entry_point: &prepared.vertex_entry_point,
        },
        GraphicsStage {
            stage: GraphicsShaderStage::Fragment,
            spv: &prepared.fragment_spv,
            entry_point: &prepared.fragment_entry_point,
        },
    ];

    let registered = sandbox
        .escalate(|full| {
            full.create_or_reuse_graphics_kernel(
                &prepared.label,
                &stages,
                prepared.push_constants,
                &prepared.pipeline_state,
                prepared.descriptor_sets_in_flight,
                &prepared.declared_bindings,
            )
        })
        .and_then(|(kernel_id, kernel)| {
            // The caller draws by name and only the shaders know which kind
            // each name is, so the shape goes back with the id.
            let bindings = kernel
                .bindings()
                .iter()
                .map(|spec| {
                    reflected_kernel_binding_response(
                        &kernel_id,
                        spec.binding,
                        graphics_binding_kind_to_wire(spec.kind).wire_name(),
                        spec.name.as_deref(),
                    )
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
            message: format!("register_graphics_kernel failed: {e}"),
        }),
    }
}

/// Render one offscreen pass with a registered graphics kernel, its bindings
/// resolved by name.
///
/// The draw is synchronous on the host — `offscreen_render` submits and waits
/// on its own fence — so by the time this emits an `Ok`, the GPU work has
/// retired and the writes to the colour targets are visible to any later
/// submission on the same device.
///
/// Three shapes the wire carries have no host path and are refused rather than
/// silently dropped:
/// - `vertex_buffers` / `index_buffer` / an indexed draw. The setters take a
///   [`crate::core::rhi::VertexBuffer`] / [`crate::core::rhi::IndexBuffer`],
///   and no escalate op mints either — a helper can acquire a pixel buffer, a
///   texture or an image, none of which those setters accept.
/// - `depth_target_uuid`. The offscreen pass attaches colour targets only, so a
///   depth attachment would never be tested against.
///
/// Every binding error raises before anything is submitted, and names the
/// kernel's own bindings so the caller can see what it should have supplied.
#[cfg(target_os = "linux")]
fn handle_run_graphics_draw(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRunGraphicsDraw,
) -> EscalateResponse {
    let unsupported = if !req.vertex_buffers.is_empty() {
        Some(format!(
            "vertex_buffers names {} buffer(s), and no escalate op mints a VertexBuffer — a \
             helper can acquire a pixel buffer, a texture or an image, and the vertex-buffer \
             setter takes none of them. Fabricate vertices from gl_VertexIndex instead",
            req.vertex_buffers.len()
        ))
    } else if req.index_buffer.is_some()
        || matches!(
            req.draw.kind,
            EscalateRequestRunGraphicsDrawDrawKind::DrawIndexed
        )
    {
        Some(
            "an indexed draw needs an IndexBuffer, and no escalate op mints one — a helper can \
             acquire a pixel buffer, a texture or an image, and the index-buffer setter takes \
             none of them"
                .to_string(),
        )
    } else if req.depth_target_uuid.is_some() {
        Some(
            "depth_target_uuid is set, and the offscreen pass this op drives attaches colour \
             targets only — the depth attachment would never be tested against"
                .to_string(),
        )
    } else if req.color_target_uuids.len() != 1 {
        Some(format!(
            "color_target_uuids names {} targets; the pipeline is built for exactly one colour \
             attachment",
            req.color_target_uuids.len()
        ))
    } else {
        None
    };
    if let Some(message) = unsupported {
        return EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("run_graphics_draw: {message}"),
        });
    }

    let push_constants = match decode_hex(&req.push_constants_hex) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("run_graphics_draw: push_constants_hex decode: {e}"),
            });
        }
    };

    let drawn = sandbox.escalate(|full| {
        let kernel = full.graphics_kernel_by_id(&req.kernel_id).ok_or_else(|| {
            crate::core::error::Error::GpuError(format!(
                "run_graphics_draw: no kernel registered under id {:?}",
                req.kernel_id
            ))
        })?;
        bind_and_render_graphics_kernel(full, &kernel, &req, &push_constants)
    });

    match drawn {
        Ok(()) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            // Echo the kernel_id back — the draw is sync host-side, so no
            // separate handle is allocated per draw.
            handle_id: req.kernel_id,
            ..Default::default()
        }),
        Err(e) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("run_graphics_draw failed: {e}"),
        }),
    }
}

/// Resolve every named binding onto the kernel's slots, render, and publish the
/// layout each colour target was left in.
///
/// The plan is total and every surface is resolved before the first `set_*`
/// call, so a refused draw never leaves the kernel holding a mix of this draw's
/// bindings and the last one's. The kernel's staged bindings are shared across
/// every caller of the cache; interleaving is prevented by the escalate gate,
/// which serializes the whole surrounding scope runtime-wide.
#[cfg(target_os = "linux")]
fn bind_and_render_graphics_kernel(
    full: &crate::core::context::GpuContextFullAccess,
    kernel: &crate::vulkan::rhi::VulkanGraphicsKernel,
    req: &EscalateRequestRunGraphicsDraw,
    push_constants: &[u8],
) -> crate::core::error::Result<()> {
    use crate::core::error::Error;
    use crate::core::rhi::{DrawCall, ScissorRect, Viewport, VulkanLayout};
    use crate::vulkan::rhi::{OffscreenColorTarget, OffscreenDraw, VulkanStage};

    let declared_specs = kernel.bindings();
    let declared: Vec<DeclaredKernelBindingUnderPlanning<'_>> = declared_specs
        .iter()
        .map(|spec| DeclaredKernelBindingUnderPlanning {
            binding_slot: spec.binding,
            name: spec.name.as_deref(),
            kind_wire_name: graphics_binding_kind_to_wire(spec.kind).wire_name(),
            surface_bound_kind: surface_bound_graphics_binding_kind(spec.kind),
        })
        .collect();
    let supplied: Vec<SuppliedKernelBindingUnderPlanning<'_>> = req
        .bindings
        .iter()
        .map(|wire| SuppliedKernelBindingUnderPlanning {
            name: wire.name.as_str(),
            target_id: wire.surface_uuid.as_str(),
            kind_wire_name: wire.kind.wire_name(),
        })
        .collect();
    let planned = plan_supplied_surface_bound_kernel_bindings("draw", &supplied, &declared)?;

    // Held across the render, not consumed by the bind loop: a registration is
    // a refcount on the texture the descriptor set now points at, and dropping
    // the last one before the GPU has run frees the image out from under it.
    let bound_inputs = resolve_planned_surface_bound_kernel_bindings(full, planned)?;
    transition_bound_kernel_inputs_into_descriptor_layouts(
        full,
        "escalate_graphics_draw_input_layouts",
        VulkanStage::ALL_GRAPHICS,
        &descriptor_layout_transition_pairs(&bound_inputs),
    )?;

    let mut color_targets = Vec::with_capacity(req.color_target_uuids.len());
    for surface_id in &req.color_target_uuids {
        let registration = full
            .resolve_texture_registration_by_surface_id(surface_id, None, 0, 0)
            .map_err(|e| {
                Error::GpuError(format!(
                    "colour target {surface_id:?} is not something this graph can resolve to a \
                     device texture: {e}"
                ))
            })?;
        // A colour target enters the pass from UNDEFINED, which discards what
        // it held — so a binding reading the very image this draw renders into
        // reads discarded pixels. A target carrying no image is its own error,
        // raised where the attachment is built.
        let clashing_binding =
            registration
                .texture()
                .vulkan_inner()
                .image()
                .and_then(|target_image| {
                    bound_inputs.iter().find(|input| {
                        input.registration.texture().vulkan_inner().image() == Some(target_image)
                    })
                });
        if let Some(clashing) = clashing_binding {
            return Err(Error::GpuError(format!(
                "binding `{}` (surface {:?}) and colour target {surface_id:?} name one texture; \
                 the pass discards a colour target's contents on entry, so the binding would \
                 read pixels this draw has already thrown away",
                clashing.planned.name, clashing.planned.target_id
            )));
        }
        color_targets.push(registration);
    }

    for binding in &bound_inputs {
        let texture = binding.registration.texture();
        match binding.planned.kind {
            SurfaceBoundKernelBindingKind::SampledTexture => kernel.set_sampled_texture(
                req.frame_index,
                binding.planned.binding_slot,
                texture,
            )?,
            SurfaceBoundKernelBindingKind::StorageImage => {
                kernel.set_storage_image(req.frame_index, binding.planned.binding_slot, texture)?
            }
        }
    }

    // A kernel that declares push constants must be given them even when the
    // payload is empty, so `set_push_constants` produces the size mismatch
    // rather than the draw running against whatever the kernel's staged buffer
    // last held.
    if kernel.push_constant_size() > 0 || !push_constants.is_empty() {
        kernel.set_push_constants(req.frame_index, push_constants)?;
    }

    let draw = OffscreenDraw::Draw(DrawCall {
        vertex_count: req.draw.vertex_count,
        instance_count: req.draw.instance_count,
        first_vertex: req.draw.first_vertex,
        first_instance: req.draw.first_instance,
        viewport: req.viewport.as_ref().map(|v| Viewport {
            x: v.x,
            y: v.y,
            width: v.width,
            height: v.height,
            min_depth: v.min_depth,
            max_depth: v.max_depth,
        }),
        scissor: req.scissor.as_ref().map(|s| ScissorRect {
            x: s.x,
            y: s.y,
            width: s.width,
            height: s.height,
        }),
    });

    // `clear_color: None` would load an attachment the pass has just
    // transitioned from UNDEFINED, whose contents are undefined by then. The
    // op carries no clear colour of its own, so transparent black is what a
    // discarded target starts from.
    let attachments: Vec<OffscreenColorTarget<'_>> = color_targets
        .iter()
        .map(|registration| OffscreenColorTarget {
            texture: registration.texture(),
            clear_color: Some([0.0, 0.0, 0.0, 0.0]),
        })
        .collect();
    let rendered = kernel.offscreen_render(
        req.frame_index,
        &attachments,
        (req.extent_width, req.extent_height),
        draw,
    );
    drop(attachments);

    // `offscreen_render` transitions each colour target into
    // COLOR_ATTACHMENT_OPTIMAL and never tells its registration, so the record
    // would otherwise keep claiming the pre-draw layout and the next consumer's
    // barrier would name the wrong oldLayout. A refused draw transitioned
    // nothing, so only a rendered one publishes.
    if rendered.is_ok() {
        for registration in &color_targets {
            registration.update_layout(VulkanLayout::COLOR_ATTACHMENT_OPTIMAL);
        }
        let mut bound_surfaces = bound_surface_layout_publish_pairs(&bound_inputs);
        bound_surfaces.extend(
            req.color_target_uuids
                .iter()
                .cloned()
                .zip(color_targets.iter().cloned()),
        );
        publish_bound_surface_layouts_to_surface_share(full, &bound_surfaces);
    }
    drop(color_targets);
    drop(bound_inputs);
    rendered
}

/// Build a triangle-geometry BLAS for a subprocess customer, against
/// `GpuContext`.
///
/// Decodes the hex-encoded vertex (`f32` triples) and index (`u32` triples)
/// blobs, checks triangle-shape consistency, and registers the built structure
/// under a fresh `as_id` a later trace names it by.
///
/// Failure modes (each an [`EscalateResponse::Err`] keyed by the request_id):
/// 1. `vertices_hex` / `indices_hex` doesn't decode as hex bytes.
/// 2. Vertex blob length is not a multiple of 12 (one vertex = 3 × f32).
/// 3. Index blob length is not a multiple of 12 (one triangle = 3 × u32).
/// 4. The device does not expose the `VK_KHR_ray_tracing_pipeline` chain.
/// 5. Empty geometry, or an acceleration-structure build failure.
#[cfg(target_os = "linux")]
fn handle_register_acceleration_structure_blas(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRegisterAccelerationStructureBlas,
) -> EscalateResponse {
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

    let registered = sandbox.escalate(|full| {
        refuse_a_device_without_ray_tracing(full, "register_acceleration_structure_blas")?;
        let blas = full.build_triangles_blas(&req.label, &vertices, &indices)?;
        Ok(full.register_acceleration_structure(blas))
    });

    match registered {
        Ok(acceleration_structure_id) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id: acceleration_structure_id,
            ..Default::default()
        }),
        Err(e) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("register_acceleration_structure_blas failed: {e}"),
        }),
    }
}

/// Build a TLAS over previously-registered BLASes, against `GpuContext`.
///
/// Each instance's transform is exactly 12 floats (row-major 3×4) and its mask
/// is 8-bit; the `blas_id` resolves through the acceleration-structure registry
/// and must name a bottom-level structure. The TLAS keeps every referenced BLAS
/// alive for its own lifetime.
///
/// Failure modes (each an [`EscalateResponse::Err`] keyed by the request_id):
/// 1. Empty instance list — a TLAS needs at least one instance per the spec.
/// 2. An instance transform is not 12 floats, or its mask exceeds 0xff.
/// 3. An instance's `flags` sets a bit no `VkGeometryInstanceFlagsKHR` owns.
/// 4. The device does not expose the `VK_KHR_ray_tracing_pipeline` chain.
/// 5. An unknown `blas_id`, a `blas_id` naming a TLAS, or a build failure.
#[cfg(target_os = "linux")]
fn handle_register_acceleration_structure_tlas(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRegisterAccelerationStructureTlas,
) -> EscalateResponse {
    if req.instances.is_empty() {
        return EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: "register_acceleration_structure_tlas: instances must not be empty (TLAS \
                 requires at least one instance per Vulkan spec)"
                .to_string(),
        });
    }
    for (idx, inst) in req.instances.iter().enumerate() {
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
    }

    let registered = sandbox.escalate(|full| {
        use crate::core::error::Error;
        use crate::vulkan::rhi::{
            AccelerationStructureKind, TlasInstanceDesc, geometry_instance_flags_from_raw_bitmask,
        };

        refuse_a_device_without_ray_tracing(full, "register_acceleration_structure_tlas")?;

        let mut instances = Vec::with_capacity(req.instances.len());
        for (idx, inst) in req.instances.iter().enumerate() {
            let blas = full
                .acceleration_structure_by_id(&inst.blas_id)
                .ok_or_else(|| {
                    Error::GpuError(format!(
                        "instance {idx} names no acceleration structure registered under id {:?}",
                        inst.blas_id
                    ))
                })?;
            if blas.kind() != AccelerationStructureKind::BottomLevel {
                return Err(Error::GpuError(format!(
                    "instance {idx} names {:?}, which is a top-level structure; a TLAS instance \
                     references a bottom-level one",
                    inst.blas_id
                )));
            }
            let t = &inst.transform;
            instances.push(TlasInstanceDesc {
                transform: [
                    [t[0], t[1], t[2], t[3]],
                    [t[4], t[5], t[6], t[7]],
                    [t[8], t[9], t[10], t[11]],
                ],
                custom_index: inst.custom_index,
                mask: inst.mask as u8,
                sbt_record_offset: inst.sbt_record_offset,
                flags: geometry_instance_flags_from_raw_bitmask(inst.flags)
                    .map_err(|e| Error::GpuError(format!("instance {idx}: {e}")))?,
                blas: (*blas).clone(),
            });
        }

        let tlas = full.build_tlas(&req.label, &instances)?;
        Ok(full.register_acceleration_structure(tlas))
    });

    match registered {
        Ok(acceleration_structure_id) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            handle_id: acceleration_structure_id,
            ..Default::default()
        }),
        Err(e) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("register_acceleration_structure_tlas failed: {e}"),
        }),
    }
}

/// Refuse an op that needs the ray-tracing pipeline on a device without it.
///
/// Raised before any build so the caller gets the device's own answer rather
/// than an extension-missing failure from inside a structure build.
#[cfg(target_os = "linux")]
fn refuse_a_device_without_ray_tracing(
    full: &crate::core::context::GpuContextFullAccess,
    op: &str,
) -> crate::core::error::Result<()> {
    if full.supports_ray_tracing_pipeline() {
        return Ok(());
    }
    Err(crate::core::error::Error::GpuError(format!(
        "{op}: this device does not expose the VK_KHR_ray_tracing_pipeline extension chain, so \
         it can build neither acceleration structures nor ray-tracing pipelines"
    )))
}

/// The compiler's name for a ray-tracing wire stage.
///
/// Distinct from [`ray_tracing_stage_from_wire`], which maps the same wire
/// value to the stage a shader group is built from: one names a pipeline stage
/// to compile for, the other names the stage a module fills.
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

/// One compiled ray-tracing stage: which pipeline stage it fills, the SPIR-V
/// that fills it, and the entry point inside that blob.
#[cfg(target_os = "linux")]
struct PreparedRayTracingKernelStage {
    stage: crate::core::rhi::RayTracingShaderStage,
    spirv: Arc<[u8]>,
    entry_point: String,
}

/// Everything a `register_ray_tracing_kernel` settles before it takes the
/// device gate: every stage compiled, the group layout read, the declaration
/// read.
#[cfg(target_os = "linux")]
struct PreparedRayTracingKernelRegistration {
    label: String,
    stages: Vec<PreparedRayTracingKernelStage>,
    groups: Vec<crate::core::rhi::RayTracingShaderGroup>,
    declared_bindings: Vec<crate::core::rhi::RayTracingBindingDeclaration>,
    push_constants: crate::core::rhi::RayTracingPushConstants,
    max_recursion_depth: u32,
}

/// Read a `register_ray_tracing_kernel` request into what `GpuContext` builds a
/// kernel from, without touching the device.
#[cfg(target_os = "linux")]
fn prepare_ray_tracing_kernel_registration(
    sandbox: &GpuContextLimitedAccess,
    req: EscalateRequestRegisterRayTracingKernel,
) -> std::result::Result<PreparedRayTracingKernelRegistration, String> {
    use crate::core::rhi::{
        RayTracingBindingDeclaration, RayTracingPushConstants, RayTracingShaderGroup,
        RayTracingShaderStageFlags,
    };

    let mut stages = Vec::with_capacity(req.stages.len());
    for (idx, st) in req.stages.iter().enumerate() {
        let stage_source = registered_shader_stage_source(
            &format!("stages[{idx}]."),
            &st.source,
            &st.spv_hex,
            ray_tracing_pipeline_stage_from_wire(st.stage),
            &st.entry_point,
        )?;
        stages.push(PreparedRayTracingKernelStage {
            stage: ray_tracing_stage_from_wire(st.stage),
            spirv: stage_source.spirv(sandbox).map_err(|e| e.to_string())?,
            entry_point: stage_source.entry_point().to_string(),
        });
    }

    let mut groups: Vec<RayTracingShaderGroup> = Vec::with_capacity(req.groups.len());
    for (idx, g) in req.groups.iter().enumerate() {
        groups.push(match g.kind {
            EscalateRequestRegisterRayTracingKernelGroupKind::General => {
                RayTracingShaderGroup::General {
                    general: g.general_stage,
                }
            }
            EscalateRequestRegisterRayTracingKernelGroupKind::TrianglesHit => {
                RayTracingShaderGroup::TrianglesHit {
                    closest_hit: optional_stage(g.closest_hit_stage),
                    any_hit: optional_stage(g.any_hit_stage),
                }
            }
            EscalateRequestRegisterRayTracingKernelGroupKind::ProceduralHit => {
                if g.intersection_stage == RAY_TRACING_STAGE_INDEX_NONE {
                    return Err(format!(
                        "groups[{idx}] procedural_hit must set intersection_stage (got \
                         {RAY_TRACING_STAGE_INDEX_NONE} which is the absent-sentinel)"
                    ));
                }
                RayTracingShaderGroup::ProceduralHit {
                    intersection: g.intersection_stage,
                    closest_hit: optional_stage(g.closest_hit_stage),
                    any_hit: optional_stage(g.any_hit_stage),
                }
            }
        });
    }

    let mut declared_bindings = Vec::with_capacity(req.bindings.len());
    for wire in &req.bindings {
        declared_bindings.push(RayTracingBindingDeclaration {
            name: wire.name.clone(),
            kind: ray_tracing_binding_kind_from_wire(wire.kind),
            stages: RayTracingShaderStageFlags::from_bits(wire.stages).ok_or_else(|| {
                format!(
                    "binding `{}` names stages {:#b}, which sets a bit no ray-tracing stage owns \
                     (1 = ray_gen, 2 = miss, 4 = closest_hit, 8 = any_hit, 16 = intersection, \
                     32 = callable)",
                    wire.name, wire.stages
                )
            })?,
        });
    }

    let push_constants = RayTracingPushConstants {
        size: req.push_constant_size,
        stages: RayTracingShaderStageFlags::from_bits(req.push_constant_stages).ok_or_else(
            || {
                format!(
                    "push_constant_stages {:#b} sets a bit no ray-tracing stage owns",
                    req.push_constant_stages
                )
            },
        )?,
    };

    Ok(PreparedRayTracingKernelRegistration {
        label: req.label,
        stages,
        groups,
        declared_bindings,
        push_constants,
        max_recursion_depth: req.max_recursion_depth,
    })
}

/// Build a ray-tracing kernel for a subprocess customer, against `GpuContext`.
///
/// The ray-tracing twin of [`handle_register_compute_kernel`], over N stages
/// rather than one: reflection across every stage derives the binding shape and
/// its names, the request's own declaration is checked against it, and
/// re-registering an identical kernel is a cache hit that answers with the same
/// `kernel_id`.
///
/// Failure modes (each an [`EscalateResponse::Err`] keyed by the request_id):
/// 1. A stage supplies neither `source` nor `spv_hex`, or both; its source does
///    not compile; or its hex doesn't decode.
/// 2. A `procedural_hit` group leaves `intersection_stage` at the sentinel.
/// 3. A binding's or the push-constant range's `stages` mask sets a bit no
///    ray-tracing stage owns.
/// 4. The device does not expose the `VK_KHR_ray_tracing_pipeline` chain.
/// 5. The blobs' `OpName` decorations were stripped, or the declaration
///    disagrees with reflection on a name, a kind, or a stage.
/// 6. Group/stage inconsistency, push-constant size mismatch, or pipeline build
///    failure.
#[cfg(target_os = "linux")]
fn handle_register_ray_tracing_kernel(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRegisterRayTracingKernel,
) -> EscalateResponse {
    use crate::core::rhi::RayTracingStage;

    let prepared = match prepare_ray_tracing_kernel_registration(sandbox, req) {
        Ok(prepared) => prepared,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("register_ray_tracing_kernel: {e}"),
            });
        }
    };

    let stages: Vec<RayTracingStage<'_>> = prepared
        .stages
        .iter()
        .map(|prepared_stage| RayTracingStage {
            stage: prepared_stage.stage,
            spv: &prepared_stage.spirv,
            entry_point: &prepared_stage.entry_point,
        })
        .collect();

    let registered = sandbox
        .escalate(|full| {
            refuse_a_device_without_ray_tracing(full, "register_ray_tracing_kernel")?;
            full.create_or_reuse_ray_tracing_kernel(
                &prepared.label,
                &stages,
                &prepared.groups,
                prepared.push_constants,
                prepared.max_recursion_depth,
                &prepared.declared_bindings,
            )
        })
        .and_then(|(kernel_id, kernel)| {
            let bindings = kernel
                .bindings()
                .iter()
                .map(|spec| {
                    reflected_kernel_binding_response(
                        &kernel_id,
                        spec.binding,
                        ray_tracing_binding_kind_to_wire(spec.kind).wire_name(),
                        spec.name.as_deref(),
                    )
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
            message: format!("register_ray_tracing_kernel failed: {e}"),
        }),
    }
}

/// Trace one grid with a registered ray-tracing kernel, its bindings resolved
/// by name.
///
/// The trace is synchronous on the host — `trace_rays` submits and waits on its
/// own fence — so by the time this emits an `Ok`, the GPU work has retired and
/// the writes to the output storage image are visible to any later submission
/// on the same device.
///
/// An `acceleration_structure` binding names an `as_id` a prior
/// `register_acceleration_structure_tlas` returned; every other kind names a
/// surface. Every binding error raises before anything is submitted, and names
/// the kernel's own bindings.
#[cfg(target_os = "linux")]
fn handle_run_ray_tracing_kernel(
    sandbox: &GpuContextLimitedAccess,
    rid: String,
    req: EscalateRequestRunRayTracingKernel,
) -> EscalateResponse {
    let push_constants = match decode_hex(&req.push_constants_hex) {
        Ok(b) => b,
        Err(e) => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!("run_ray_tracing_kernel: push_constants_hex decode: {e}"),
            });
        }
    };

    let traced = sandbox.escalate(|full| {
        let kernel = full
            .ray_tracing_kernel_by_id(&req.kernel_id)
            .ok_or_else(|| {
                crate::core::error::Error::GpuError(format!(
                    "run_ray_tracing_kernel: no kernel registered under id {:?}",
                    req.kernel_id
                ))
            })?;
        bind_and_trace_ray_tracing_kernel(full, &kernel, &req, &push_constants)
    });

    match traced {
        Ok(()) => EscalateResponse::Ok(EscalateResponseOk {
            request_id: rid,
            // Echo the kernel_id back — the trace is sync host-side, so no
            // separate handle is allocated per trace.
            handle_id: req.kernel_id,
            ..Default::default()
        }),
        Err(e) => EscalateResponse::Err(EscalateResponseErr {
            request_id: rid,
            message: format!("run_ray_tracing_kernel failed: {e}"),
        }),
    }
}

/// Resolve every named binding onto the kernel's slots, then trace.
///
/// The plan is total and every target is resolved before the first `set_*`
/// call, so a refused trace never leaves the kernel holding a mix of this
/// trace's bindings and the last one's.
#[cfg(target_os = "linux")]
fn bind_and_trace_ray_tracing_kernel(
    full: &crate::core::context::GpuContextFullAccess,
    kernel: &crate::vulkan::rhi::VulkanRayTracingKernel,
    req: &EscalateRequestRunRayTracingKernel,
    push_constants: &[u8],
) -> crate::core::error::Result<()> {
    use crate::core::error::Error;
    use crate::core::rhi::RayTracingBindingKind;
    use crate::vulkan::rhi::{AccelerationStructureKind, VulkanStage};

    let declared_specs = kernel.bindings();

    // Checked over the whole array before it is split, so a name supplied twice
    // is refused whichever half each copy would land in.
    let declared_names: Vec<&str> = declared_specs
        .iter()
        .filter_map(|spec| spec.name.as_deref())
        .collect();
    refuse_a_kernel_binding_name_supplied_twice(
        "trace",
        req.bindings.iter().map(|wire| wire.name.as_str()),
        &declared_names,
    )?;

    // The acceleration structures come out first: they resolve through their
    // own registry rather than through a surface, so the surface planner never
    // sees them and the kernel's declaration for them is checked here.
    let mut acceleration_structure_bindings = Vec::new();
    let mut surface_supplied = Vec::with_capacity(req.bindings.len());
    for wire in &req.bindings {
        let declared_as_acceleration_structure = declared_specs
            .iter()
            .find(|spec| spec.name.as_deref() == Some(wire.name.as_str()))
            .filter(|spec| spec.kind == RayTracingBindingKind::AccelerationStructure);
        let Some(declaration) = declared_as_acceleration_structure else {
            surface_supplied.push(SuppliedKernelBindingUnderPlanning {
                name: wire.name.as_str(),
                target_id: wire.target_id.as_str(),
                kind_wire_name: wire.kind.wire_name(),
            });
            continue;
        };
        if ray_tracing_binding_kind_from_wire(wire.kind)
            != RayTracingBindingKind::AccelerationStructure
        {
            return Err(Error::GpuError(format!(
                "binding `{}` was supplied as {} but this kernel declares it \
                 acceleration_structure",
                wire.name,
                wire.kind.wire_name()
            )));
        }
        let slot = declaration.binding;
        let tlas = full
            .acceleration_structure_by_id(&wire.target_id)
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "binding `{}` names no acceleration structure registered under id {:?}",
                    wire.name, wire.target_id
                ))
            })?;
        if tlas.kind() != AccelerationStructureKind::TopLevel {
            return Err(Error::GpuError(format!(
                "binding `{}` names {:?}, which is a bottom-level structure; a trace binds the \
                 top-level one a `register_acceleration_structure_tlas` returned",
                wire.name, wire.target_id
            )));
        }
        acceleration_structure_bindings.push((slot, tlas));
    }

    // Declared acceleration structures are dropped from the surface planner's
    // view of the declaration too, so its missing-binding check counts only
    // what it is responsible for.
    let declared: Vec<DeclaredKernelBindingUnderPlanning<'_>> = declared_specs
        .iter()
        .filter(|spec| spec.kind != RayTracingBindingKind::AccelerationStructure)
        .map(|spec| DeclaredKernelBindingUnderPlanning {
            binding_slot: spec.binding,
            name: spec.name.as_deref(),
            kind_wire_name: ray_tracing_binding_kind_to_wire(spec.kind).wire_name(),
            surface_bound_kind: surface_bound_ray_tracing_binding_kind(spec.kind),
        })
        .collect();
    let planned =
        plan_supplied_surface_bound_kernel_bindings("trace", &surface_supplied, &declared)?;
    for spec in declared_specs
        .iter()
        .filter(|spec| spec.kind == RayTracingBindingKind::AccelerationStructure)
    {
        if acceleration_structure_bindings
            .iter()
            .any(|(slot, _)| *slot == spec.binding)
        {
            continue;
        }
        let declared_name = spec.name.as_deref().ok_or_else(|| {
            Error::GpuError(format!(
                "this kernel holds an unnamed acceleration-structure binding at slot {}; \
                 reflection refuses these, so this kernel did not come through registration",
                spec.binding
            ))
        })?;
        return Err(Error::GpuError(format!(
            "binding `{declared_name}` was not supplied; bindings do not persist between traces, \
             so every trace supplies all of them"
        )));
    }

    let bound_inputs = resolve_planned_surface_bound_kernel_bindings(full, planned)?;
    transition_bound_kernel_inputs_into_descriptor_layouts(
        full,
        "escalate_ray_tracing_trace_input_layouts",
        VulkanStage::ALL_COMMANDS,
        &descriptor_layout_transition_pairs(&bound_inputs),
    )?;

    for (slot, tlas) in &acceleration_structure_bindings {
        kernel.set_acceleration_structure(*slot, tlas)?;
    }
    for binding in &bound_inputs {
        let texture = binding.registration.texture();
        match binding.planned.kind {
            SurfaceBoundKernelBindingKind::SampledTexture => {
                kernel.set_sampled_texture(binding.planned.binding_slot, texture)?
            }
            SurfaceBoundKernelBindingKind::StorageImage => {
                kernel.set_storage_image(binding.planned.binding_slot, texture)?
            }
        }
    }

    // A kernel that declares push constants must be given them even when the
    // payload is empty, so `set_push_constants` produces the size mismatch
    // rather than the trace running against whatever the kernel's staged buffer
    // last held.
    if kernel.push_constant_size() > 0 || !push_constants.is_empty() {
        kernel.set_push_constants(push_constants)?;
    }

    let traced = kernel.trace_rays(req.width, req.height, req.depth);
    if traced.is_ok() {
        publish_bound_surface_layouts_to_surface_share(
            full,
            &bound_surface_layout_publish_pairs(&bound_inputs),
        );
    }
    drop(bound_inputs);
    traced
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

/// The pipeline stage a ray-tracing wire stage's module fills.
#[cfg(target_os = "linux")]
fn ray_tracing_stage_from_wire(
    stage: EscalateRequestRegisterRayTracingKernelStageStage,
) -> crate::core::rhi::RayTracingShaderStage {
    use crate::core::rhi::RayTracingShaderStage;
    use EscalateRequestRegisterRayTracingKernelStageStage as W;
    match stage {
        W::RayGen => RayTracingShaderStage::RayGen,
        W::Miss => RayTracingShaderStage::Miss,
        W::ClosestHit => RayTracingShaderStage::ClosestHit,
        W::AnyHit => RayTracingShaderStage::AnyHit,
        W::Intersection => RayTracingShaderStage::Intersection,
        W::Callable => RayTracingShaderStage::Callable,
    }
}

/// One arm-for-arm mapping from a wire blend-factor enum to the RHI's.
///
/// A macro rather than a function per enum: the wire carries four separate
/// factor enums with identical arms, and four hand-written copies of the same
/// fifteen-arm match is four things to keep in step.
#[cfg(target_os = "linux")]
macro_rules! blend_factor_from_wire {
    ($enum:ident, $value:expr) => {{
        use crate::core::rhi::BlendFactor;
        use $enum as W;
        match $value {
            W::Zero => BlendFactor::Zero,
            W::One => BlendFactor::One,
            W::SrcColor => BlendFactor::SrcColor,
            W::OneMinusSrcColor => BlendFactor::OneMinusSrcColor,
            W::DstColor => BlendFactor::DstColor,
            W::OneMinusDstColor => BlendFactor::OneMinusDstColor,
            W::SrcAlpha => BlendFactor::SrcAlpha,
            W::OneMinusSrcAlpha => BlendFactor::OneMinusSrcAlpha,
            W::DstAlpha => BlendFactor::DstAlpha,
            W::OneMinusDstAlpha => BlendFactor::OneMinusDstAlpha,
            W::ConstantColor => BlendFactor::ConstantColor,
            W::OneMinusConstantColor => BlendFactor::OneMinusConstantColor,
            W::ConstantAlpha => BlendFactor::ConstantAlpha,
            W::OneMinusConstantAlpha => BlendFactor::OneMinusConstantAlpha,
            W::SrcAlphaSaturate => BlendFactor::SrcAlphaSaturate,
        }
    }};
}

/// One arm-for-arm mapping from a wire blend-op enum to the RHI's, for the same
/// reason [`blend_factor_from_wire`] is a macro.
#[cfg(target_os = "linux")]
macro_rules! blend_op_from_wire {
    ($enum:ident, $value:expr) => {{
        use crate::core::rhi::BlendOp;
        use $enum as W;
        match $value {
            W::Add => BlendOp::Add,
            W::Subtract => BlendOp::Subtract,
            W::ReverseSubtract => BlendOp::ReverseSubtract,
            W::Min => BlendOp::Min,
            W::Max => BlendOp::Max,
        }
    }};
}

/// Flatten the wire's one-level pipeline state into the RHI's nested one.
///
/// The wire is flat because JSON has no sum types: every field is present and
/// the flags decide which ones mean anything. The RHI's sum types are what the
/// pipeline is actually built from, so the two shapes meet here.
///
/// Refuses what a draw over this op has no path for — MSAA beyond one sample,
/// other than exactly one colour attachment, either half of a depth attachment,
/// either half of a vertex input, a colour format the texture vocabulary doesn't
/// name, and a write mask naming a bit no channel owns.
#[cfg(target_os = "linux")]
fn graphics_pipeline_state_from_wire(
    p: EscalateRequestRegisterGraphicsKernelPipelineState,
) -> std::result::Result<crate::core::rhi::GraphicsPipelineState, String> {
    use crate::core::rhi::{
        AttachmentFormats, ColorBlendAttachment, ColorBlendState, ColorWriteMask, CullMode,
        DepthStencilState, FrontFace, GraphicsDynamicState, GraphicsPipelineState,
        MultisampleState, PolygonMode, PrimitiveTopology, RasterizationState, VertexInputState,
    };

    if p.multisample_samples != 1 {
        return Err(format!(
            "multisample_samples is {}; the graphics kernel builds single-sampled pipelines only",
            p.multisample_samples
        ));
    }
    if p.attachment_color_formats.len() != 1 {
        return Err(format!(
            "attachment_color_formats names {} formats; the graphics kernel targets exactly one \
             colour attachment",
            p.attachment_color_formats.len()
        ));
    }
    // The offscreen pass a draw runs through attaches colour targets only, so a
    // pipeline declaring a depth attachment mismatches the rendering info at
    // every draw. `run_graphics_draw` refuses `depth_target_uuid` for the same
    // reason; refusing only there would let the mismatch be built at register
    // time and surface as a driver error a draw away from its cause.
    if p.depth_stencil_enabled {
        return Err(
            "depth_stencil_enabled is set, and the offscreen pass a draw runs through attaches \
             colour targets only — a depth-testing pipeline has no attachment to test against"
                .to_string(),
        );
    }
    if p.attachment_depth_format.is_some() {
        return Err(
            "attachment_depth_format names a depth attachment, and the offscreen pass a draw runs \
             through attaches colour targets only — the pipeline's formats would disagree with \
             the pass at every draw"
                .to_string(),
        );
    }

    // A pipeline pulling from a vertex binding could register and then never
    // draw: `run_graphics_draw` refuses `vertex_buffers` because no escalate op
    // mints a `VertexBuffer`, and the kernel refuses a declared binding whose
    // buffer was never set at every draw. Refused here, the caller meets the
    // reason where the shape is asked for rather than a submission away from it.
    if !p.vertex_input_bindings.is_empty() {
        return Err(format!(
            "vertex_input_bindings names {} binding(s), and no escalate op mints a VertexBuffer to \
             fill one — a helper can acquire a pixel buffer, a texture or an image, and the \
             vertex-buffer setter takes none of them, so this pipeline would register and then be \
             refused at every draw. Fabricate vertices from gl_VertexIndex instead",
            p.vertex_input_bindings.len()
        ));
    }
    if !p.vertex_input_attributes.is_empty() {
        return Err(format!(
            "vertex_input_attributes names {} attribute(s), and an attribute is pulled from a \
             vertex binding no escalate op can mint a buffer for. Fabricate vertices from \
             gl_VertexIndex instead",
            p.vertex_input_attributes.len()
        ));
    }

    let topology = match p.topology {
        EscalateRequestRegisterGraphicsKernelPipelineStateTopology::PointList => {
            PrimitiveTopology::PointList
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateTopology::LineList => {
            PrimitiveTopology::LineList
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateTopology::LineStrip => {
            PrimitiveTopology::LineStrip
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateTopology::TriangleList => {
            PrimitiveTopology::TriangleList
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateTopology::TriangleStrip => {
            PrimitiveTopology::TriangleStrip
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateTopology::TriangleFan => {
            PrimitiveTopology::TriangleFan
        }
    };

    let rasterization = RasterizationState {
        polygon_mode: match p.rasterization_polygon_mode {
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode::Fill => {
                PolygonMode::Fill
            }
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode::Line => {
                PolygonMode::Line
            }
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode::Point => {
                PolygonMode::Point
            }
        },
        cull_mode: match p.rasterization_cull_mode {
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode::None => {
                CullMode::None
            }
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode::Front => {
                CullMode::Front
            }
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode::Back => {
                CullMode::Back
            }
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode::FrontAndBack => {
                CullMode::FrontAndBack
            }
        },
        front_face: match p.rasterization_front_face {
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace::CounterClockwise => {
                FrontFace::CounterClockwise
            }
            EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace::Clockwise => {
                FrontFace::Clockwise
            }
        },
        line_width: p.rasterization_line_width,
    };

    let color_write_mask = ColorWriteMask::from_bits(p.color_write_mask).ok_or_else(|| {
        format!(
            "color_write_mask {:#b} sets a bit no colour channel owns (1 = R, 2 = G, 4 = B, \
             8 = A)",
            p.color_write_mask
        )
    })?;
    let color_blend = if p.color_blend_enabled {
        ColorBlendState::Enabled(ColorBlendAttachment {
            src_color_blend_factor: blend_factor_from_wire!(
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor,
                p.color_blend_src_color_factor
            ),
            dst_color_blend_factor: blend_factor_from_wire!(
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor,
                p.color_blend_dst_color_factor
            ),
            color_blend_op: blend_op_from_wire!(
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp,
                p.color_blend_color_op
            ),
            src_alpha_blend_factor: blend_factor_from_wire!(
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor,
                p.color_blend_src_alpha_factor
            ),
            dst_alpha_blend_factor: blend_factor_from_wire!(
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor,
                p.color_blend_dst_alpha_factor
            ),
            alpha_blend_op: blend_op_from_wire!(
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp,
                p.color_blend_alpha_op
            ),
            color_write_mask,
        })
    } else {
        ColorBlendState::Disabled { color_write_mask }
    };

    let mut color = Vec::with_capacity(p.attachment_color_formats.len());
    for format in &p.attachment_color_formats {
        color.push(
            parse_texture_format(format).map_err(|e| format!("attachment_color_formats: {e}"))?,
        );
    }
    let attachment_formats = AttachmentFormats { color, depth: None };

    let dynamic_state = match p.dynamic_state {
        EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState::None => {
            GraphicsDynamicState::None
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState::ViewportScissor => {
            GraphicsDynamicState::ViewportScissor
        }
    };

    Ok(GraphicsPipelineState {
        topology,
        vertex_input: VertexInputState::None,
        rasterization,
        multisample: MultisampleState {
            samples: p.multisample_samples,
        },
        depth_stencil: DepthStencilState::Disabled,
        color_blend,
        attachment_formats,
        dynamic_state,
    })
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

/// Drop `GpuContext`'s strong reference to an acceleration structure, answering
/// whether the id named one.
///
/// A structure the caller built and then let go of is the only escalate-minted
/// resource whose device memory is proportional to what the caller supplied, so
/// it is the one a long-running helper must be able to hand back. Off Linux
/// nothing can have built one, so nothing can be released.
fn release_acceleration_structure(sandbox: &GpuContextLimitedAccess, handle_id: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        sandbox
            .escalate(|full| Ok(full.release_acceleration_structure(handle_id)))
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (sandbox, handle_id);
        false
    }
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
    TextureFormat::from_wire_name(&normalized)
        .ok_or_else(|| format!("unknown texture format '{normalized}'"))
}

/// Which cross-process-importable allocation flavor an `acquire_texture`
/// request can take, derived from the request alone — never a Python dial.
///
/// Render-attachment requests need the explicit-modifier DMA-BUF flavor (the
/// OPAQUE_FD constructor's fixed usage set has no COLOR_ATTACHMENT), and only
/// single-plane formats take it — a multi-plane registration ships one fd
/// against N plane offsets, which every consumer import rejects. Requests
/// whose format is CUDA-mappable and whose usage fits the fixed set take
/// OPAQUE_FD when the device has the pool for it. Everything else keeps
/// today's non-importable allocation — a flavor the device or format cannot
/// take falls back rather than failing the acquire, and a later
/// cross-process import refuses by naming the flavor.
#[cfg(target_os = "linux")]
fn derive_texture_cross_process_importability(
    format: TextureFormat,
    usage: TextureUsages,
    render_target_modifier_available: bool,
    opaque_fd_image_pool_available: bool,
) -> TextureCrossProcessImportability {
    if usage.contains(TextureUsages::RENDER_ATTACHMENT) {
        let format_is_single_plane = format.plane_count() == 1;
        return if render_target_modifier_available && format_is_single_plane {
            TextureCrossProcessImportability::RenderTargetDmaBuf
        } else {
            TextureCrossProcessImportability::NotImportable
        };
    }
    let cuda_mappable = matches!(
        format,
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba16Float | TextureFormat::Rgba32Float
    );
    let opaque_fd_fixed_usage_set = TextureUsages::COPY_SRC
        | TextureUsages::COPY_DST
        | TextureUsages::TEXTURE_BINDING
        | TextureUsages::STORAGE_BINDING;
    if cuda_mappable && opaque_fd_image_pool_available && opaque_fd_fixed_usage_set.contains(usage)
    {
        return TextureCrossProcessImportability::OpaqueFd;
    }
    TextureCrossProcessImportability::NotImportable
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
        assert!(registry.remove_handle("missing").is_none());
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

    #[cfg(target_os = "linux")]
    #[test]
    fn a_render_attachment_request_takes_the_modifier_flavor_when_the_probe_has_one() {
        let usage = TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING;
        assert_eq!(
            derive_texture_cross_process_importability(
                TextureFormat::Rgba8Unorm,
                usage,
                true,
                true
            ),
            TextureCrossProcessImportability::RenderTargetDmaBuf,
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_render_attachment_request_without_a_modifier_stays_not_importable() {
        let usage = TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING;
        assert_eq!(
            derive_texture_cross_process_importability(
                TextureFormat::Rgba8Unorm,
                usage,
                false,
                true
            ),
            TextureCrossProcessImportability::NotImportable,
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_multi_plane_format_never_takes_the_modifier_flavor() {
        let usage = TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING;
        assert_eq!(
            derive_texture_cross_process_importability(TextureFormat::Nv12, usage, true, true),
            TextureCrossProcessImportability::NotImportable,
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_device_without_the_opaque_fd_pool_falls_back_to_not_importable() {
        let usage = TextureUsages::TEXTURE_BINDING | TextureUsages::STORAGE_BINDING;
        assert_eq!(
            derive_texture_cross_process_importability(
                TextureFormat::Rgba8Unorm,
                usage,
                false,
                false
            ),
            TextureCrossProcessImportability::NotImportable,
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_cuda_mappable_format_within_the_fixed_usage_set_takes_opaque_fd() {
        let usage = TextureUsages::TEXTURE_BINDING
            | TextureUsages::STORAGE_BINDING
            | TextureUsages::COPY_SRC
            | TextureUsages::COPY_DST;
        for format in [
            TextureFormat::Rgba8Unorm,
            TextureFormat::Rgba16Float,
            TextureFormat::Rgba32Float,
        ] {
            assert_eq!(
                derive_texture_cross_process_importability(format, usage, false, true),
                TextureCrossProcessImportability::OpaqueFd,
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_format_cuda_cannot_map_stays_not_importable_without_render_attachment() {
        for format in [
            TextureFormat::Bgra8Unorm,
            TextureFormat::Bgra8UnormSrgb,
            TextureFormat::Rgba8UnormSrgb,
            TextureFormat::Nv12,
        ] {
            assert_eq!(
                derive_texture_cross_process_importability(
                    format,
                    TextureUsages::TEXTURE_BINDING,
                    true,
                    true,
                ),
                TextureCrossProcessImportability::NotImportable,
            );
        }
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
            assert_eq!(planned[0].kind, SurfaceBoundKernelBindingKind::StorageImage);
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
                    .map(|b| (b.name.as_str(), b.kind.as_str()))
                    .collect::<Vec<_>>(),
                vec![
                    ("source_image", "sampled_texture"),
                    ("output_image", "storage_image"),
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

        /// Read a surface back, sourcing the readback barrier from the layout
        /// the surface is actually tracked in.
        ///
        /// Not a hardcoded `General`: a batch leaves a sampled binding in
        /// SHADER_READ_ONLY_OPTIMAL, and a barrier whose `oldLayout` disagrees
        /// with the image makes the contents undefined by spec — so a test
        /// asserting on those pixels would be reading what no driver owes it.
        fn read_back_rgba8(
            sandbox: &GpuContextLimitedAccess,
            surface_id: &str,
            texture: &crate::core::rhi::Texture,
            label: &str,
        ) -> Vec<u8> {
            use crate::core::rhi::TextureSourceLayout;
            sandbox
                .escalate(|full| {
                    let resting_layout = full
                        .resolve_texture_registration_by_surface_id(surface_id, None, 64, 64)?
                        .current_layout();
                    let source_layout = if resting_layout
                        == streamlib_consumer_rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL
                    {
                        TextureSourceLayout::ShaderReadOnly
                    } else {
                        TextureSourceLayout::General
                    };
                    let readback =
                        full.create_texture_readback(label, 64, 64, TextureFormat::Rgba8Unorm)?;
                    let ticket = readback.submit(texture, source_layout)?;
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
                &read_back_rgba8(
                    &sandbox,
                    "chain-doubled",
                    held[2].texture(),
                    "chain-readback",
                ),
                CHAIN_DOUBLED_RGBA,
                "the chain's output — pass 2 must have read pass 1's writes, not the \
                 seed and not an undefined intermediate",
            );
            assert_every_pixel_is(
                &read_back_rgba8(
                    &sandbox,
                    "chain-brightened",
                    held[1].texture(),
                    "chain-intermediate-readback",
                ),
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

            assert!(
                separate_submissions >= dispatches.len(),
                "the path the batch replaces submits at least once per dispatch, plus an \
                 input-layout transition when a binding arrives in the wrong layout — if \
                 this is fewer than the dispatch count the counter is not counting and \
                 the assertion above proves nothing: {separate_submissions}"
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
                            ("unbrightened_image", "refused-seed"),
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
                &read_back_rgba8(
                    &sandbox,
                    "refused-brightened",
                    held[1].texture(),
                    "refused-readback",
                ),
                CHAIN_BRIGHTENED_RGBA,
                // Weak on its own — dispatch 0 would have recomputed this same
                // value — so the claim that nothing ran rests on the
                // submission count above. This checks the refusal did not
                // leave the surface unreadable.
                "the intermediate, still readable after the refused batch",
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
                &read_back_rgba8(
                    &sandbox,
                    "refused-doubled",
                    held[2].texture(),
                    "after-readback",
                ),
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
                &read_back_rgba8(
                    &sandbox,
                    "aborted-middle",
                    held[1].texture(),
                    "after-abort-readback",
                ),
                CHAIN_BRIGHTENED_RGBA,
                "the batch that ran after the aborted one",
            );
            drop(held);
        }

        /// A dispatch that reads and writes one surface is refused, because the
        /// two descriptors want the image in two layouts at once.
        ///
        /// Reachable from Python — `bindings={"src": s, "dst": s}` is an
        /// ordinary-looking mapping — and the shader's own names differ, so the
        /// duplicate-name rule does not catch it. Left unrefused, the batch
        /// records two contradictory barriers and the dispatch runs with one
        /// descriptor's layout wrong; the single-dispatch path records no
        /// barriers at all and is wrong the same way. Refused for both.
        #[test]
        fn one_surface_bound_as_two_kinds_in_one_dispatch_is_refused() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("one surface at two kinds: no GPU — skipping");
                return;
            };
            let kernel_id = register_read_one_write_another(&sandbox).handle_id;
            let held = seeded_chain_textures(&sandbox, ["both-seed", "both-b", "both-c"]);

            let read_and_written = supplied(&[
                (
                    "source_image",
                    EscalateComputeBindingKind::SampledTexture,
                    "both-seed",
                ),
                (
                    "output_image",
                    EscalateComputeBindingKind::StorageImage,
                    "both-seed",
                ),
            ]);

            // Refused on the single-dispatch path...
            let single = handle_run_compute_kernel(
                &sandbox,
                "both-single".to_string(),
                EscalateRequestRunComputeKernel {
                    bindings: read_and_written.clone(),
                    group_count_x: 8,
                    group_count_y: 8,
                    group_count_z: 1,
                    kernel_id: kernel_id.clone(),
                    push_constants_hex: "00000000".to_string(),
                    request_id: "both-single".to_string(),
                },
            );
            let message = refusal_message(single);
            assert!(
                message.contains("both-seed")
                    && message.contains("`source_image`")
                    && message.contains("`output_image`"),
                "the refusal must name the surface and both bindings: {message}"
            );

            // ...and on the batch path, which shares the resolver.
            let submissions_before = sandbox.host_inner().queue_submission_count();
            let batched = handle_run_compute_kernel_batch(
                &sandbox,
                "both-batched".to_string(),
                EscalateRequestRunComputeKernelBatch {
                    dispatches: vec![EscalateRequestRunComputeKernelBatchDispatch {
                        bindings: read_and_written,
                        group_count_x: 8,
                        group_count_y: 8,
                        group_count_z: 1,
                        kernel_id: kernel_id.clone(),
                        push_constants_hex: "00000000".to_string(),
                    }],
                    request_id: "both-batched".to_string(),
                },
            );
            assert!(
                refusal_message(batched).contains("both-seed"),
                "the batch shares the resolver, so it refuses the same shape"
            );
            assert_eq!(
                sandbox.host_inner().queue_submission_count(),
                submissions_before,
                "and submits nothing"
            );

            // Two spellings, one texture. A published frame id resolves through
            // its pool slot's cache entry, so `both-seed` and `both-seed#3` are
            // the same image — and comparing the ids as strings would let this
            // pair through to the dispatch the arms above refuse.
            let two_spellings = handle_run_compute_kernel(
                &sandbox,
                "both-spellings".to_string(),
                EscalateRequestRunComputeKernel {
                    bindings: supplied(&[
                        (
                            "source_image",
                            EscalateComputeBindingKind::SampledTexture,
                            "both-seed",
                        ),
                        (
                            "output_image",
                            EscalateComputeBindingKind::StorageImage,
                            "both-seed#3",
                        ),
                    ]),
                    group_count_x: 8,
                    group_count_y: 8,
                    group_count_z: 1,
                    kernel_id,
                    push_constants_hex: "00000000".to_string(),
                    request_id: "both-spellings".to_string(),
                },
            );
            let message = refusal_message(two_spellings);
            assert!(
                message.contains("both-seed\"") && message.contains("both-seed#3"),
                "the refusal must name both spellings, since neither is wrong on its own: \
                 {message}"
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
    /// `run_graphics_draw` escalate handlers.
    ///
    /// Mirrors `compute_kernel_dispatch`: the binding planner and the wire→RHI
    /// pipeline-state translation are pure functions that run everywhere CI
    /// does, and only the tests that build a real pipeline need a device.
    #[cfg(target_os = "linux")]
    mod graphics_kernel_dispatch {
        use super::super::*;
        use super::EscalateHandleRegistry;

        use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
            EscalateRequestRegisterGraphicsKernelBinding,
            EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttribute,
            EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBinding,
            EscalateRequestRunGraphicsDrawBinding, EscalateRequestRunGraphicsDrawDraw,
            EscalateRequestRunGraphicsDrawIndexBuffer,
            EscalateRequestRunGraphicsDrawIndexBufferIndexType,
            EscalateRequestRunGraphicsDrawScissor, EscalateRequestRunGraphicsDrawVertexBuffer,
        };
        use crate::core::context::GpuContext;
        use crate::core::rhi::GraphicsBindingKind;

        /// Graphics is an always-present capability now, so there is no bridge
        /// to install — only a device to have or not have.
        fn make_gpu_sandbox_if_available() -> Option<GpuContextLimitedAccess> {
            GpuContext::init_for_platform_sync()
                .ok()
                .map(GpuContextLimitedAccess::new)
        }

        fn refusal_message(response: EscalateResponse) -> String {
            match response {
                EscalateResponse::Err(err) => err.message,
                other => panic!("expected Err, got {other:?}"),
            }
        }

        /// Fabricates a full-screen triangle out of `gl_VertexIndex` alone —
        /// the only vertex source a draw over this op can have, since no
        /// escalate op mints a vertex buffer.
        const FULL_SCREEN_TRIANGLE_VERTEX_GLSL: &str = "\
#version 450
void main() {
    vec2 corner = vec2((gl_VertexIndex << 1) & 2, gl_VertexIndex & 2);
    gl_Position = vec4(corner * 2.0 - 1.0, 0.0, 1.0);
}
";

        /// Inverts the sampled input's colour and keeps its alpha, so the
        /// rendered pixels prove which surface the named binding resolved to.
        const INVERT_SAMPLED_INPUT_FRAGMENT_GLSL: &str = "\
#version 450
layout(set = 0, binding = 0) uniform sampler2D source_image;
layout(location = 0) out vec4 painted_colour;
void main() {
    vec4 source = texelFetch(source_image, ivec2(gl_FragCoord.xy), 0);
    painted_colour = vec4(vec3(1.0) - source.rgb, source.a);
}
";

        /// The same pass with the fragment constant folded in, so registering
        /// it produces a different pipeline — and therefore a different kernel
        /// id — from [`INVERT_SAMPLED_INPUT_FRAGMENT_GLSL`].
        const HALVE_SAMPLED_INPUT_FRAGMENT_GLSL: &str = "\
#version 450
layout(set = 0, binding = 0) uniform sampler2D source_image;
layout(location = 0) out vec4 painted_colour;
void main() {
    vec4 source = texelFetch(source_image, ivec2(gl_FragCoord.xy), 0);
    painted_colour = vec4(source.rgb * 0.5, source.a);
}
";

        /// Each seed channel inverts exactly in unorm8: out = 255 - in.
        const SEED_RGBA: [u8; 4] = [10, 20, 30, 255];
        const INVERTED_RGBA: [u8; 4] = [245, 235, 225, 255];

        /// Seeded into a colour target no draw fully covers: neither stage of
        /// the kernel writes it, so a pixel still carrying it was loaded rather
        /// than cleared.
        const UNCOVERED_SENTINEL_RGBA: [u8; 4] = [3, 5, 7, 255];

        /// What the handler's clear colour leaves in a pixel the draw missed.
        const TRANSPARENT_BLACK_RGBA: [u8; 4] = [0, 0, 0, 0];

        /// The baseline pipeline state every register request starts from —
        /// TriangleList, no blending, no depth, one `rgba8_unorm` attachment.
        fn baseline_pipeline_state() -> EscalateRequestRegisterGraphicsKernelPipelineState {
            EscalateRequestRegisterGraphicsKernelPipelineState {
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
            }
        }

        /// A `register_graphics_kernel` request carrying pre-compiled SPIR-V
        /// hex for both stages. Tests that need a specific shape mutate fields
        /// after calling.
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
                pipeline_state: baseline_pipeline_state(),
            }
        }

        /// The same request built from GLSL, which is what the wire carries
        /// now that the engine owns compilation.
        fn register_from_glsl(
            request_id: &str,
            fragment_source: &str,
        ) -> EscalateRequestRegisterGraphicsKernel {
            let mut req = make_register_req(request_id, "", "");
            req.vertex_source = FULL_SCREEN_TRIANGLE_VERTEX_GLSL.to_string();
            req.fragment_source = fragment_source.to_string();
            req
        }

        /// Baseline `run_graphics_draw` request — vertex-fabricating (no vertex
        /// buffers, no index buffer), one colour target, a simple Draw of the
        /// full-screen triangle's three vertices.
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

        fn register_graphics_kernel_or_panic(
            sandbox: &GpuContextLimitedAccess,
            registry: &EscalateHandleRegistry,
            req: EscalateRequestRegisterGraphicsKernel,
        ) -> EscalateResponseOk {
            let response = handle_escalate_op(
                sandbox,
                registry,
                EscalateRequest::RegisterGraphicsKernel(req),
            )
            .expect("must produce a response");
            match response {
                EscalateResponse::Ok(ok) => ok,
                other => panic!("registering the graphics kernel failed: {other:?}"),
            }
        }

        // ----- the binding planner --------------------------------------
        //
        // Pure wire validation shared with the trace path, driven here through
        // the graphics kinds: no device, so these run everywhere CI does.

        fn declared_graphics_bindings(
            entries: &'static [(u32, &'static str, GraphicsBindingKind)],
        ) -> Vec<DeclaredKernelBindingUnderPlanning<'static>> {
            entries
                .iter()
                .map(
                    |(binding_slot, name, kind)| DeclaredKernelBindingUnderPlanning {
                        binding_slot: *binding_slot,
                        name: Some(name),
                        kind_wire_name: graphics_binding_kind_to_wire(*kind).wire_name(),
                        surface_bound_kind: surface_bound_graphics_binding_kind(*kind),
                    },
                )
                .collect()
        }

        /// The shape the planner tests measure against: one sampled input and
        /// one storage output, deliberately different kinds so binding by slot
        /// order rather than by name would swap them.
        const A_DRAWING_KERNELS_BINDINGS: &[(u32, &str, GraphicsBindingKind)] = &[
            (0, "source_image", GraphicsBindingKind::SampledTexture),
            (1, "painted_output", GraphicsBindingKind::StorageImage),
        ];

        /// A kernel whose one binding is a uniform buffer — a kind a draw
        /// cannot name a surface for.
        const A_TINTING_KERNELS_BINDINGS: &[(u32, &str, GraphicsBindingKind)] =
            &[(0, "tint_parameters", GraphicsBindingKind::UniformBuffer)];

        fn supplied_graphics_bindings<'a>(
            entries: &'a [(&'a str, EscalateGraphicsBindingKind, &'a str)],
        ) -> Vec<SuppliedKernelBindingUnderPlanning<'a>> {
            entries
                .iter()
                .map(
                    |(name, kind, target_id)| SuppliedKernelBindingUnderPlanning {
                        name,
                        target_id,
                        kind_wire_name: kind.wire_name(),
                    },
                )
                .collect()
        }

        fn draw_plan_refusal(
            declared: &'static [(u32, &'static str, GraphicsBindingKind)],
            supplied: &[(&str, EscalateGraphicsBindingKind, &str)],
        ) -> String {
            let declared = declared_graphics_bindings(declared);
            let supplied = supplied_graphics_bindings(supplied);
            plan_supplied_surface_bound_kernel_bindings("draw", &supplied, &declared)
                .err()
                .expect("expected the plan to be refused")
                .to_string()
        }

        #[test]
        fn a_complete_draw_resolves_every_name_to_its_slot() {
            let declared = declared_graphics_bindings(A_DRAWING_KERNELS_BINDINGS);
            let supplied = supplied_graphics_bindings(&[
                (
                    "painted_output",
                    EscalateGraphicsBindingKind::StorageImage,
                    "surface-out",
                ),
                (
                    "source_image",
                    EscalateGraphicsBindingKind::SampledTexture,
                    "surface-in",
                ),
            ]);
            let planned = plan_supplied_surface_bound_kernel_bindings("draw", &supplied, &declared)
                .expect("a complete, correctly-typed draw");

            // Resolution is by name, so the order the caller supplied them in
            // is not the order the shaders declared them in — and that is fine.
            assert_eq!(planned.len(), 2);
            assert_eq!(planned[0].name, "painted_output");
            assert_eq!(planned[0].binding_slot, 1);
            assert_eq!(planned[0].kind, SurfaceBoundKernelBindingKind::StorageImage);
            assert_eq!(planned[0].target_id, "surface-out");
            assert_eq!(planned[1].name, "source_image");
            assert_eq!(planned[1].binding_slot, 0);
            assert_eq!(
                planned[1].kind,
                SurfaceBoundKernelBindingKind::SampledTexture
            );
        }

        /// Not expressible in a Python mapping — a dict cannot carry one key
        /// twice — so the wire array is the only layer that can guard it.
        #[test]
        fn a_name_supplied_twice_is_refused() {
            let message = draw_plan_refusal(
                A_DRAWING_KERNELS_BINDINGS,
                &[
                    (
                        "source_image",
                        EscalateGraphicsBindingKind::SampledTexture,
                        "surface-in",
                    ),
                    (
                        "source_image",
                        EscalateGraphicsBindingKind::SampledTexture,
                        "surface-other",
                    ),
                    (
                        "painted_output",
                        EscalateGraphicsBindingKind::StorageImage,
                        "surface-out",
                    ),
                ],
            );
            assert!(
                message.contains("binding `source_image` was supplied twice"),
                "must name the duplicate, got: {message}"
            );
            assert!(
                message.contains("`source_image`, `painted_output`"),
                "must name the kernel's declared bindings, got: {message}"
            );
        }

        #[test]
        fn a_name_the_shaders_do_not_declare_is_refused() {
            let message = draw_plan_refusal(
                A_DRAWING_KERNELS_BINDINGS,
                &[
                    (
                        "source_image",
                        EscalateGraphicsBindingKind::SampledTexture,
                        "surface-in",
                    ),
                    (
                        "painted_output",
                        EscalateGraphicsBindingKind::StorageImage,
                        "surface-out",
                    ),
                    (
                        "sharpen_amount",
                        EscalateGraphicsBindingKind::UniformBuffer,
                        "surface-x",
                    ),
                ],
            );
            assert!(
                message.contains("binding `sharpen_amount` is not one this kernel declares"),
                "must name the unknown binding, got: {message}"
            );
            assert!(
                message.contains("`source_image`, `painted_output`"),
                "must name the kernel's declared bindings, got: {message}"
            );
        }

        /// No implicit default and no carried-over value: the kernel holds no
        /// binding state between draws to fall back on.
        #[test]
        fn a_declared_binding_left_out_is_refused() {
            let message = draw_plan_refusal(
                A_DRAWING_KERNELS_BINDINGS,
                &[(
                    "source_image",
                    EscalateGraphicsBindingKind::SampledTexture,
                    "surface-in",
                )],
            );
            assert!(
                message.contains("binding `painted_output` was not supplied"),
                "must name the missing binding, got: {message}"
            );
            assert!(
                message.contains("do not persist between draws"),
                "must say why there is no fallback, got: {message}"
            );
        }

        #[test]
        fn a_binding_supplied_as_the_wrong_kind_is_refused() {
            let message = draw_plan_refusal(
                A_DRAWING_KERNELS_BINDINGS,
                &[
                    (
                        "source_image",
                        EscalateGraphicsBindingKind::SampledTexture,
                        "surface-in",
                    ),
                    (
                        "painted_output",
                        EscalateGraphicsBindingKind::StorageBuffer,
                        "surface-out",
                    ),
                ],
            );
            assert!(
                message.contains("binding `painted_output` was supplied as storage_buffer"),
                "must name the binding and the kind supplied, got: {message}"
            );
            assert!(
                message.contains("declares it storage_image"),
                "must name the kind the kernel declares, got: {message}"
            );
        }

        /// A buffer binding is legal in a shader and legal on the wire, but no
        /// escalate op mints a buffer a descriptor can point at — so the draw
        /// that would need one is refused rather than silently unbound.
        #[test]
        fn a_binding_of_a_kind_no_surface_can_back_is_refused() {
            let message = draw_plan_refusal(
                A_TINTING_KERNELS_BINDINGS,
                &[(
                    "tint_parameters",
                    EscalateGraphicsBindingKind::UniformBuffer,
                    "surface-x",
                )],
            );
            assert!(
                message.contains("binding `tint_parameters` is uniform_buffer"),
                "must name the binding and its kind, got: {message}"
            );
            assert!(
                message.contains("storage_image and sampled_texture"),
                "must name the kinds a draw can bind, got: {message}"
            );
        }

        /// A kernel with no bindings at all draws — the empty case is not an
        /// error, and the "missing" rule has nothing to fire on.
        #[test]
        fn a_kernel_declaring_nothing_needs_nothing_supplied() {
            let planned = plan_supplied_surface_bound_kernel_bindings("draw", &[], &[])
                .expect("an unbound kernel draws");
            assert!(planned.is_empty());
        }

        // ----- wire → RHI pipeline state --------------------------------

        /// Lock in the wire→RHI pipeline-state translation. Mentally reverting
        /// any single arm of `graphics_pipeline_state_from_wire` (e.g. swapping
        /// `Add ↔ Subtract`) must fail this test — nothing else checks the
        /// ~200 lines of enum mapping in the handler, and a wrong arm builds a
        /// pipeline the caller did not ask for without complaint.
        #[test]
        fn pipeline_state_translates_every_enum_arm() {
            use crate::core::rhi::{
                BlendFactor, BlendOp, ColorBlendState, ColorWriteMask, CullMode, DepthStencilState,
                FrontFace, GraphicsDynamicState, PolygonMode, PrimitiveTopology, VertexInputState,
            };

            // Every value is chosen to differ from the matching default, so a
            // wrong arm in the translation lands in the wrong RHI variant and
            // the assertion fails.
            let mut wire = baseline_pipeline_state();
            wire.topology =
                EscalateRequestRegisterGraphicsKernelPipelineStateTopology::TriangleStrip;
            wire.rasterization_polygon_mode =
                EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode::Line;
            wire.rasterization_cull_mode =
                EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode::Back;
            wire.rasterization_front_face =
                EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace::Clockwise;
            wire.rasterization_line_width = 2.5;
            wire.color_blend_enabled = true;
            wire.color_write_mask = 0b0101; // R | B only
            wire.color_blend_src_color_factor =
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor::SrcAlpha;
            wire.color_blend_dst_color_factor =
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor::OneMinusSrcAlpha;
            wire.color_blend_color_op =
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp::Subtract;
            wire.color_blend_src_alpha_factor =
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor::ConstantAlpha;
            wire.color_blend_dst_alpha_factor =
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor::OneMinusConstantAlpha;
            wire.color_blend_alpha_op =
                EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp::Max;
            wire.attachment_color_formats = vec!["bgra8_unorm_srgb".to_string()];
            wire.dynamic_state =
                EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState::None;

            let state = graphics_pipeline_state_from_wire(wire).expect("a buildable shape");

            assert_eq!(state.topology, PrimitiveTopology::TriangleStrip);
            // Not a translated arm: both halves of a vertex input are refused
            // below, so the only vertex-input state this can produce is the
            // gl_VertexIndex-driven one.
            assert!(
                matches!(state.vertex_input, VertexInputState::None),
                "expected the gl_VertexIndex-driven shape, got {:?}",
                state.vertex_input
            );
            assert_eq!(state.rasterization.polygon_mode, PolygonMode::Line);
            assert_eq!(state.rasterization.cull_mode, CullMode::Back);
            assert_eq!(state.rasterization.front_face, FrontFace::Clockwise);
            assert_eq!(state.rasterization.line_width, 2.5);
            assert_eq!(state.multisample.samples, 1);
            // Not a translated arm: both halves of a depth attachment are
            // refused above, so the only depth state this can produce is off.
            assert_eq!(state.depth_stencil, DepthStencilState::Disabled);
            match state.color_blend {
                ColorBlendState::Enabled(attachment) => {
                    assert_eq!(attachment.src_color_blend_factor, BlendFactor::SrcAlpha);
                    assert_eq!(
                        attachment.dst_color_blend_factor,
                        BlendFactor::OneMinusSrcAlpha
                    );
                    assert_eq!(attachment.color_blend_op, BlendOp::Subtract);
                    assert_eq!(
                        attachment.src_alpha_blend_factor,
                        BlendFactor::ConstantAlpha
                    );
                    assert_eq!(
                        attachment.dst_alpha_blend_factor,
                        BlendFactor::OneMinusConstantAlpha
                    );
                    assert_eq!(attachment.alpha_blend_op, BlendOp::Max);
                    assert_eq!(
                        attachment.color_write_mask,
                        ColorWriteMask::R | ColorWriteMask::B
                    );
                }
                other => panic!("expected blending on, got {other:?}"),
            }
            assert_eq!(
                state.attachment_formats.color,
                vec![TextureFormat::Bgra8UnormSrgb]
            );
            assert_eq!(state.attachment_formats.depth, None);
            assert_eq!(state.dynamic_state, GraphicsDynamicState::None);
        }

        /// The four blend-factor fields and the two blend-op fields share one
        /// macro each, so a swapped arm there is wrong in every field at once
        /// and the single-value test above would only catch the arm it picked.
        #[test]
        fn every_blend_factor_and_blend_op_arm_reaches_the_rhi_attachment() {
            use crate::core::rhi::{BlendFactor, BlendOp, ColorBlendState};
            use EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp as AlphaOpWire;
            use EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp as ColorOpWire;
            use EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor as DstAlphaWire;
            use EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor as DstColorWire;
            use EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor as SrcAlphaWire;
            use EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor as SrcColorWire;

            let factor_arms = [
                (
                    SrcColorWire::Zero,
                    DstColorWire::Zero,
                    SrcAlphaWire::Zero,
                    DstAlphaWire::Zero,
                    BlendFactor::Zero,
                ),
                (
                    SrcColorWire::One,
                    DstColorWire::One,
                    SrcAlphaWire::One,
                    DstAlphaWire::One,
                    BlendFactor::One,
                ),
                (
                    SrcColorWire::SrcColor,
                    DstColorWire::SrcColor,
                    SrcAlphaWire::SrcColor,
                    DstAlphaWire::SrcColor,
                    BlendFactor::SrcColor,
                ),
                (
                    SrcColorWire::OneMinusSrcColor,
                    DstColorWire::OneMinusSrcColor,
                    SrcAlphaWire::OneMinusSrcColor,
                    DstAlphaWire::OneMinusSrcColor,
                    BlendFactor::OneMinusSrcColor,
                ),
                (
                    SrcColorWire::DstColor,
                    DstColorWire::DstColor,
                    SrcAlphaWire::DstColor,
                    DstAlphaWire::DstColor,
                    BlendFactor::DstColor,
                ),
                (
                    SrcColorWire::OneMinusDstColor,
                    DstColorWire::OneMinusDstColor,
                    SrcAlphaWire::OneMinusDstColor,
                    DstAlphaWire::OneMinusDstColor,
                    BlendFactor::OneMinusDstColor,
                ),
                (
                    SrcColorWire::SrcAlpha,
                    DstColorWire::SrcAlpha,
                    SrcAlphaWire::SrcAlpha,
                    DstAlphaWire::SrcAlpha,
                    BlendFactor::SrcAlpha,
                ),
                (
                    SrcColorWire::OneMinusSrcAlpha,
                    DstColorWire::OneMinusSrcAlpha,
                    SrcAlphaWire::OneMinusSrcAlpha,
                    DstAlphaWire::OneMinusSrcAlpha,
                    BlendFactor::OneMinusSrcAlpha,
                ),
                (
                    SrcColorWire::DstAlpha,
                    DstColorWire::DstAlpha,
                    SrcAlphaWire::DstAlpha,
                    DstAlphaWire::DstAlpha,
                    BlendFactor::DstAlpha,
                ),
                (
                    SrcColorWire::OneMinusDstAlpha,
                    DstColorWire::OneMinusDstAlpha,
                    SrcAlphaWire::OneMinusDstAlpha,
                    DstAlphaWire::OneMinusDstAlpha,
                    BlendFactor::OneMinusDstAlpha,
                ),
                (
                    SrcColorWire::ConstantColor,
                    DstColorWire::ConstantColor,
                    SrcAlphaWire::ConstantColor,
                    DstAlphaWire::ConstantColor,
                    BlendFactor::ConstantColor,
                ),
                (
                    SrcColorWire::OneMinusConstantColor,
                    DstColorWire::OneMinusConstantColor,
                    SrcAlphaWire::OneMinusConstantColor,
                    DstAlphaWire::OneMinusConstantColor,
                    BlendFactor::OneMinusConstantColor,
                ),
                (
                    SrcColorWire::ConstantAlpha,
                    DstColorWire::ConstantAlpha,
                    SrcAlphaWire::ConstantAlpha,
                    DstAlphaWire::ConstantAlpha,
                    BlendFactor::ConstantAlpha,
                ),
                (
                    SrcColorWire::OneMinusConstantAlpha,
                    DstColorWire::OneMinusConstantAlpha,
                    SrcAlphaWire::OneMinusConstantAlpha,
                    DstAlphaWire::OneMinusConstantAlpha,
                    BlendFactor::OneMinusConstantAlpha,
                ),
                (
                    SrcColorWire::SrcAlphaSaturate,
                    DstColorWire::SrcAlphaSaturate,
                    SrcAlphaWire::SrcAlphaSaturate,
                    DstAlphaWire::SrcAlphaSaturate,
                    BlendFactor::SrcAlphaSaturate,
                ),
            ];
            for (src_color, dst_color, src_alpha, dst_alpha, expected) in factor_arms {
                let mut wire = baseline_pipeline_state();
                wire.color_blend_enabled = true;
                wire.color_blend_src_color_factor = src_color;
                wire.color_blend_dst_color_factor = dst_color;
                wire.color_blend_src_alpha_factor = src_alpha;
                wire.color_blend_dst_alpha_factor = dst_alpha;
                let state = graphics_pipeline_state_from_wire(wire).expect("a buildable shape");
                match state.color_blend {
                    ColorBlendState::Enabled(attachment) => {
                        assert_eq!(attachment.src_color_blend_factor, expected);
                        assert_eq!(attachment.dst_color_blend_factor, expected);
                        assert_eq!(attachment.src_alpha_blend_factor, expected);
                        assert_eq!(attachment.dst_alpha_blend_factor, expected);
                    }
                    other => panic!("expected blending on, got {other:?}"),
                }
            }

            let op_arms = [
                (ColorOpWire::Add, AlphaOpWire::Add, BlendOp::Add),
                (
                    ColorOpWire::Subtract,
                    AlphaOpWire::Subtract,
                    BlendOp::Subtract,
                ),
                (
                    ColorOpWire::ReverseSubtract,
                    AlphaOpWire::ReverseSubtract,
                    BlendOp::ReverseSubtract,
                ),
                (ColorOpWire::Min, AlphaOpWire::Min, BlendOp::Min),
                (ColorOpWire::Max, AlphaOpWire::Max, BlendOp::Max),
            ];
            for (color_op, alpha_op, expected) in op_arms {
                let mut wire = baseline_pipeline_state();
                wire.color_blend_enabled = true;
                wire.color_blend_color_op = color_op;
                wire.color_blend_alpha_op = alpha_op;
                let state = graphics_pipeline_state_from_wire(wire).expect("a buildable shape");
                match state.color_blend {
                    ColorBlendState::Enabled(attachment) => {
                        assert_eq!(attachment.color_blend_op, expected);
                        assert_eq!(attachment.alpha_blend_op, expected);
                    }
                    other => panic!("expected blending on, got {other:?}"),
                }
            }
        }

        /// The wire promises these refusals and nothing downstream enforces
        /// them: an MSAA pipeline, a multi-attachment one, and either half of a
        /// depth attachment or of a vertex input are shapes a draw over this op
        /// has no path for.
        #[test]
        fn a_pipeline_state_the_kernel_cannot_build_is_refused() {
            let mut multisampled = baseline_pipeline_state();
            multisampled.multisample_samples = 4;
            let message = graphics_pipeline_state_from_wire(multisampled)
                .err()
                .expect("MSAA must be refused");
            assert!(message.contains("single-sampled"), "{message}");

            let mut two_attachments = baseline_pipeline_state();
            two_attachments.attachment_color_formats =
                vec!["rgba8_unorm".to_string(), "rgba8_unorm".to_string()];
            let message = graphics_pipeline_state_from_wire(two_attachments)
                .err()
                .expect("two colour attachments must be refused");
            assert!(message.contains("exactly one"), "{message}");

            // The draw op refuses `depth_target_uuid` for the same reason; a
            // pipeline built with depth state would otherwise disagree with the
            // colour-only pass at every draw, a submission away from its cause.
            let mut depth_testing = baseline_pipeline_state();
            depth_testing.depth_stencil_enabled = true;
            let message = graphics_pipeline_state_from_wire(depth_testing)
                .err()
                .expect("depth testing must be refused");
            assert!(message.contains("colour targets only"), "{message}");

            let mut depth_attachment = baseline_pipeline_state();
            depth_attachment.attachment_depth_format = Some(
                EscalateRequestRegisterGraphicsKernelPipelineStateAttachmentDepthFormat::D32Sfloat,
            );
            let message = graphics_pipeline_state_from_wire(depth_attachment)
                .err()
                .expect("a depth attachment must be refused");
            assert!(message.contains("colour targets only"), "{message}");

            let mut unowned_write_mask = baseline_pipeline_state();
            unowned_write_mask.color_write_mask = 0b1_0000;
            let message = graphics_pipeline_state_from_wire(unowned_write_mask)
                .err()
                .expect("a bit no channel owns must be refused");
            assert!(message.contains("no colour channel owns"), "{message}");

            // The draw op refuses `vertex_buffers` for the same reason. A
            // pipeline pulling from a vertex binding would otherwise register
            // and then be refused at every draw, for a buffer no escalate op
            // can mint to fill it.
            let mut buffer_fed_vertices = baseline_pipeline_state();
            buffer_fed_vertices.vertex_input_bindings = vec![
                EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBinding {
                    binding: 0,
                    stride: 12,
                    input_rate:
                        EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBindingInputRate::Vertex,
                },
            ];
            let message = graphics_pipeline_state_from_wire(buffer_fed_vertices)
                .err()
                .expect("a vertex binding no buffer can fill must be refused");
            assert!(
                message.contains("no escalate op mints a VertexBuffer"),
                "{message}"
            );
            assert!(message.contains("gl_VertexIndex"), "{message}");

            let mut unfed_attributes = baseline_pipeline_state();
            unfed_attributes.vertex_input_attributes = vec![
                EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttribute {
                    location: 0,
                    binding: 0,
                    format:
                        EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttributeFormat::Rgb32Float,
                    offset: 0,
                },
            ];
            let message = graphics_pipeline_state_from_wire(unfed_attributes)
                .err()
                .expect("an attribute with no binding it could be fed from must be refused");
            assert!(message.contains("pulled from a"), "{message}");
            assert!(message.contains("gl_VertexIndex"), "{message}");
        }

        // ----- the handlers ---------------------------------------------

        /// Both hex fields are decoded before the escalate hop, so a malformed
        /// one is refused without touching the GPU at all.
        #[test]
        fn register_with_invalid_vertex_hex_returns_err() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("register_with_invalid_vertex_hex: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let response = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::RegisterGraphicsKernel(make_register_req(
                    "req-bad-v",
                    "xyz123",
                    "cafebabe",
                )),
            )
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
        }

        #[test]
        fn register_with_invalid_fragment_hex_returns_err() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("register_with_invalid_fragment_hex: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let response = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::RegisterGraphicsKernel(make_register_req(
                    "req-bad-f",
                    "deadbeef",
                    "qq",
                )),
            )
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
        }

        #[test]
        fn run_with_invalid_push_constants_hex_returns_err() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("run_with_invalid_push_constants_hex: no GPU — skipping");
                return;
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
        }

        /// The three shapes the wire carries that the host has no path for.
        /// Each is refused rather than silently dropped: a caller who sent one
        /// would otherwise get a draw that ignored half of what it asked for.
        #[test]
        fn a_draw_naming_a_resource_no_escalate_op_mints_is_refused() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("a_draw_naming_an_unmintable_resource: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();

            let mut with_vertex_buffer = make_run_req("req-vb", "kernel-x", "surface-y");
            with_vertex_buffer.vertex_buffers = vec![EscalateRequestRunGraphicsDrawVertexBuffer {
                binding: 0,
                surface_uuid: "vb-uuid".to_string(),
                offset: "128".to_string(),
            }];
            let message = refusal_message(
                handle_escalate_op(
                    &sandbox,
                    &registry,
                    EscalateRequest::RunGraphicsDraw(with_vertex_buffer),
                )
                .expect("must produce a response"),
            );
            assert!(
                message.contains("no escalate op mints a VertexBuffer"),
                "must say what is missing, got: {message}"
            );

            let mut indexed = make_run_req("req-ib", "kernel-x", "surface-y");
            indexed.index_buffer = Some(EscalateRequestRunGraphicsDrawIndexBuffer {
                surface_uuid: "ib-uuid".to_string(),
                offset: "64".to_string(),
                index_type: EscalateRequestRunGraphicsDrawIndexBufferIndexType::Uint32,
            });
            let message = refusal_message(
                handle_escalate_op(
                    &sandbox,
                    &registry,
                    EscalateRequest::RunGraphicsDraw(indexed),
                )
                .expect("must produce a response"),
            );
            assert!(
                message.contains("an indexed draw needs an IndexBuffer"),
                "must say what is missing, got: {message}"
            );

            // The index buffer is what a `draw_indexed` names its indices in,
            // so the draw kind alone is refused for the same reason.
            let mut indexed_without_a_buffer = make_run_req("req-ib-kind", "kernel-x", "surface-y");
            indexed_without_a_buffer.draw.kind =
                EscalateRequestRunGraphicsDrawDrawKind::DrawIndexed;
            let message = refusal_message(
                handle_escalate_op(
                    &sandbox,
                    &registry,
                    EscalateRequest::RunGraphicsDraw(indexed_without_a_buffer),
                )
                .expect("must produce a response"),
            );
            assert!(
                message.contains("an indexed draw needs an IndexBuffer"),
                "must say what is missing, got: {message}"
            );
        }

        /// The offscreen pass attaches colour targets only, so a depth target
        /// would never be tested against — and a caller who set one is asking
        /// for depth testing that would not happen.
        #[test]
        fn a_draw_naming_a_depth_target_is_refused() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("a_draw_naming_a_depth_target: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let mut req = make_run_req("req-depth", "kernel-x", "surface-y");
            req.depth_target_uuid = Some("depth-uuid".to_string());
            let message = refusal_message(
                handle_escalate_op(&sandbox, &registry, EscalateRequest::RunGraphicsDraw(req))
                    .expect("must produce a response"),
            );
            assert!(
                message.contains("depth_target_uuid is set"),
                "must name the field, got: {message}"
            );
            assert!(
                message.contains("colour targets only"),
                "must say why, got: {message}"
            );
        }

        #[test]
        fn a_draw_naming_other_than_one_colour_target_is_refused() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("a_draw_naming_other_than_one_colour_target: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let mut req = make_run_req("req-targets", "kernel-x", "surface-y");
            req.color_target_uuids = vec!["a".to_string(), "b".to_string()];
            let message = refusal_message(
                handle_escalate_op(&sandbox, &registry, EscalateRequest::RunGraphicsDraw(req))
                    .expect("must produce a response"),
            );
            assert!(
                message.contains("exactly one colour attachment"),
                "got: {message}"
            );
        }

        #[test]
        fn drawing_with_an_unregistered_kernel_id_is_refused() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("drawing_with_an_unregistered_kernel_id: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let message = refusal_message(
                handle_escalate_op(
                    &sandbox,
                    &registry,
                    EscalateRequest::RunGraphicsDraw(make_run_req(
                        "req-bad-id",
                        "never-registered",
                        "surface-y",
                    )),
                )
                .expect("must produce a response"),
            );
            assert!(
                message.contains("no kernel registered under id")
                    && message.contains("never-registered"),
                "got: {message}"
            );
        }

        /// A stage mask is a bitfield the caller writes by hand, and a bit
        /// outside vertex|fragment names a stage a graphics pipeline has no
        /// module for at all.
        #[test]
        fn a_binding_declared_for_a_stage_no_graphics_pipeline_has_is_refused() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("a_binding_declared_for_an_unowned_stage: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let mut req = register_from_glsl("req-stage", INVERT_SAMPLED_INPUT_FRAGMENT_GLSL);
            req.bindings = vec![EscalateRequestRegisterGraphicsKernelBinding {
                kind: EscalateGraphicsBindingKind::SampledTexture,
                name: "source_image".to_string(),
                stages: 0b100,
            }];
            let message = refusal_message(
                handle_escalate_op(
                    &sandbox,
                    &registry,
                    EscalateRequest::RegisterGraphicsKernel(req),
                )
                .expect("must produce a response"),
            );
            assert!(
                message.contains("no graphics stage owns"),
                "must say the bit belongs to no stage, got: {message}"
            );
        }

        /// GLSL where bytes used to go: the engine compiles each stage itself,
        /// and what it hands the pipeline is a module rather than the text.
        #[test]
        fn glsl_for_each_stage_reaches_the_engine_as_compiled_spirv() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("glsl_for_each_stage_reaches_the_engine: no GPU — skipping");
                return;
            };
            for (field_prefix, source, stage) in [
                (
                    "vertex_",
                    FULL_SCREEN_TRIANGLE_VERTEX_GLSL,
                    GlslCompilationTargetStage::Vertex,
                ),
                (
                    "fragment_",
                    INVERT_SAMPLED_INPUT_FRAGMENT_GLSL,
                    GlslCompilationTargetStage::Fragment,
                ),
            ] {
                let compiled = registered_shader_stage_source(field_prefix, source, "", stage, "")
                    .expect("GLSL alone is one of the two alternatives")
                    .spirv(&sandbox)
                    .expect("the engine compiles it");
                assert_eq!(
                    compiled.get(..4),
                    Some(&SPIRV_MAGIC_LE[..]),
                    "the {stage:?} stage reached the pipeline as something other than SPIR-V"
                );
            }
        }

        /// Registration hands back the shape a draw needs: the shaders' own
        /// names, each with the kind only the shaders know. No bridge is
        /// installed — graphics is a capability the context always has.
        #[test]
        fn registration_answers_with_the_shaders_binding_names_and_kinds() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("registration_answers_with_the_shaders_bindings: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let ok = register_graphics_kernel_or_panic(
                &sandbox,
                &registry,
                register_from_glsl("reg", INVERT_SAMPLED_INPUT_FRAGMENT_GLSL),
            );
            let bindings = ok.bindings.expect("a register response carries the shape");
            assert_eq!(
                bindings
                    .iter()
                    .map(|binding| (binding.name.as_str(), binding.kind.as_str()))
                    .collect::<Vec<_>>(),
                vec![("source_image", "sampled_texture")],
                "the fragment shader's own binding, named and kinded as it declares it"
            );
        }

        /// Re-registering an identical kernel is free and keeps its id; a
        /// different fragment stage is a different pipeline and gets its own.
        #[test]
        fn an_identical_registration_keeps_its_kernel_id_and_a_different_one_gets_its_own() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("an_identical_registration_keeps_its_kernel_id: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let first = register_graphics_kernel_or_panic(
                &sandbox,
                &registry,
                register_from_glsl("a", INVERT_SAMPLED_INPUT_FRAGMENT_GLSL),
            )
            .handle_id;
            let second = register_graphics_kernel_or_panic(
                &sandbox,
                &registry,
                register_from_glsl("b", INVERT_SAMPLED_INPUT_FRAGMENT_GLSL),
            )
            .handle_id;
            assert_eq!(
                first, second,
                "an identical descriptor must produce the same kernel_id"
            );

            let other = register_graphics_kernel_or_panic(
                &sandbox,
                &registry,
                register_from_glsl("c", HALVE_SAMPLED_INPUT_FRAGMENT_GLSL),
            )
            .handle_id;
            assert_ne!(
                first, other,
                "a different fragment stage must produce a different kernel_id"
            );

            let held = sandbox
                .escalate(|full| {
                    Ok((
                        full.graphics_kernel_by_id(&first),
                        full.graphics_kernel_by_id(&second),
                    ))
                })
                .expect("the cache answers inside an escalate scope");
            let (a, b) = (held.0.expect("cached"), held.1.expect("cached"));
            assert!(
                std::sync::Arc::ptr_eq(&a, &b),
                "the second registration must reuse the first kernel, not build another"
            );
        }

        /// The cache key covers the shaders and the pipeline, not the caller's
        /// assertion — so a wrong declaration refuses identically whether or
        /// not somebody registered this kernel first.
        #[test]
        fn a_wrong_declaration_is_refused_even_when_the_kernel_is_cached() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("a_wrong_declaration_is_refused_when_cached: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            register_graphics_kernel_or_panic(
                &sandbox,
                &registry,
                register_from_glsl("warm", INVERT_SAMPLED_INPUT_FRAGMENT_GLSL),
            );

            let mut req = register_from_glsl("reg-wrong", INVERT_SAMPLED_INPUT_FRAGMENT_GLSL);
            req.bindings = vec![EscalateRequestRegisterGraphicsKernelBinding {
                kind: EscalateGraphicsBindingKind::StorageBuffer,
                name: "sharpen_amount".to_string(),
                stages: 0,
            }];
            let message = refusal_message(
                handle_escalate_op(
                    &sandbox,
                    &registry,
                    EscalateRequest::RegisterGraphicsKernel(req),
                )
                .expect("must produce a response"),
            );
            assert!(
                message.contains("`sharpen_amount`") && message.contains("`source_image`"),
                "the refusal must name the bogus binding and the shaders' own: {message}"
            );
        }

        /// The op end to end, over a real device: a draw resolves its binding
        /// by the fragment shader's own name, renders into the surface the
        /// request named, and leaves the engine's layout record agreeing with
        /// the layout the pass left the image in.
        ///
        /// The source is seeded with a known value and the target is read back
        /// and compared against the shader's own arithmetic, so a draw that
        /// bound nothing — or bound the target to itself — fails on the pixels
        /// rather than passing silently. The seeded source is then moved to
        /// `GENERAL`, which a combined image sampler does not satisfy: a draw
        /// that did not barrier its bound inputs would read it through a
        /// descriptor its layout disagrees with, and would leave the engine's
        /// record still saying `GENERAL`.
        #[test]
        fn a_draw_reads_the_surface_its_binding_names_and_publishes_the_targets_layout() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("a_draw_reads_the_surface_its_binding_names: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let kernel_id = register_graphics_kernel_or_panic(
                &sandbox,
                &registry,
                register_from_glsl("reg-draw", INVERT_SAMPLED_INPUT_FRAGMENT_GLSL),
            )
            .handle_id;

            // Held for the draw: dropping a pooled handle hands its slot back,
            // and the registration would then name a recycled texture.
            let held = sandbox
                .escalate(|full| {
                    let source = full.acquire_texture(
                        &TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                            .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST),
                    )?;
                    let target = full.acquire_texture(
                        &TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                            .with_usage(TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC),
                    )?;
                    full.register_texture("draw-source", source.texture().clone());
                    full.register_texture("draw-target", target.texture().clone());

                    let (_pool_id, seed_buffer) =
                        full.acquire_pixel_buffer(64, 64, PixelFormat::Rgba32)?;
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
                        "draw-source",
                        64,
                        64,
                    )?;

                    // The seed publishes SHADER_READ_ONLY_OPTIMAL — the very
                    // layout a sampled binding wants — so the draw's input
                    // barrier would have nothing to do and this test would end
                    // by re-reading what setup established. GENERAL is a layout
                    // the descriptor does not satisfy, which is the state a
                    // storage-image producer upstream leaves behind.
                    let mut recorder = full.create_command_recorder("draw_source_into_general")?;
                    recorder.begin()?;
                    recorder.record_image_barrier(
                        source.texture(),
                        crate::core::rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                        crate::core::rhi::VulkanLayout::GENERAL,
                        crate::vulkan::rhi::VulkanStage::ALL_COMMANDS,
                        crate::vulkan::rhi::VulkanStage::ALL_COMMANDS,
                        crate::vulkan::rhi::VulkanAccess::MEMORY_WRITE,
                        crate::vulkan::rhi::VulkanAccess::MEMORY_READ,
                    )?;
                    recorder.submit_and_wait()?;
                    full.resolve_texture_registration_by_surface_id("draw-source", None, 64, 64)?
                        .update_layout(crate::core::rhi::VulkanLayout::GENERAL);
                    Ok((source, target))
                })
                .expect("a seeded source and a colour target");

            let mut run = make_run_req("run-draw", &kernel_id, "draw-target");
            run.bindings = vec![EscalateRequestRunGraphicsDrawBinding {
                kind: EscalateGraphicsBindingKind::SampledTexture,
                name: "source_image".to_string(),
                surface_uuid: "draw-source".to_string(),
            }];
            run.extent_width = 64;
            run.extent_height = 64;
            let response =
                handle_escalate_op(&sandbox, &registry, EscalateRequest::RunGraphicsDraw(run))
                    .expect("must produce a response");
            match response {
                EscalateResponse::Ok(ok) => {
                    assert_eq!(ok.request_id, "run-draw");
                    assert_eq!(
                        ok.handle_id, kernel_id,
                        "the run response echoes the kernel_id"
                    );
                    assert!(
                        ok.timeline_value.is_none(),
                        "run_graphics_draw responses carry no timeline"
                    );
                }
                other => panic!("the draw failed: {other:?}"),
            }

            // Asserted before the readback, which transitions the image itself:
            // `offscreen_render` leaves every colour target in
            // COLOR_ATTACHMENT_OPTIMAL and tells no registration, so an
            // unpublished layout would leave the next consumer's barrier
            // naming an oldLayout the image has already left.
            let published = sandbox
                .escalate(|full| {
                    Ok(full
                        .resolve_texture_registration_by_surface_id("draw-target", None, 64, 64)?
                        .current_layout())
                })
                .expect("the colour target still resolves");
            assert_eq!(
                published,
                streamlib_consumer_rhi::VulkanLayout::COLOR_ATTACHMENT_OPTIMAL,
                "the draw must publish the layout it left the colour target in"
            );

            let rendered = sandbox
                .escalate(|full| {
                    let readback = full.create_texture_readback(
                        "draw-readback",
                        64,
                        64,
                        TextureFormat::Rgba8Unorm,
                    )?;
                    let ticket = readback.submit(
                        held.1.texture(),
                        crate::core::rhi::TextureSourceLayout::ColorAttachment,
                    )?;
                    Ok(readback.wait_and_read(ticket, 2_000_000_000)?.to_vec())
                })
                .expect("the colour target reads back");
            for (pixel_index, pixel) in rendered.chunks_exact(4).enumerate() {
                assert_eq!(
                    pixel, INVERTED_RGBA,
                    "pixel {pixel_index} must be the inverted seed — the draw read \
                     `source_image`, by name, and painted the target it was given"
                );
            }

            // The bound input left GENERAL for the layout its descriptor
            // required, which is where the next consumer's barrier starts from.
            let source_layout = sandbox
                .escalate(|full| {
                    Ok(full
                        .resolve_texture_registration_by_surface_id("draw-source", None, 64, 64)?
                        .current_layout())
                })
                .expect("the source still resolves");
            assert_eq!(
                source_layout,
                streamlib_consumer_rhi::VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
                "a sampled binding is barriered out of GENERAL into the layout its descriptor \
                 requires"
            );
            drop(held);
        }

        /// The pixels a draw does not cover are the load op's, and this op
        /// carries no clear colour of its own — so the handler's choice of
        /// transparent black over `LOAD` is what they read.
        ///
        /// The draw is scissored to the left half of a target seeded with a
        /// sentinel no stage writes, so nothing but the load op ever touches the
        /// right half. `LOAD` there reads an attachment the pass has just
        /// transitioned from `UNDEFINED`, whose contents the spec stops defining
        /// at that point — on this device the seeded sentinel survives it, which
        /// is what makes the assertion discriminate.
        #[test]
        fn the_pixels_a_draw_does_not_cover_read_transparent_black() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("the_pixels_a_draw_does_not_cover: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let kernel_id = register_graphics_kernel_or_panic(
                &sandbox,
                &registry,
                register_from_glsl("reg-scissored", INVERT_SAMPLED_INPUT_FRAGMENT_GLSL),
            )
            .handle_id;

            let held = sandbox
                .escalate(|full| {
                    let source = full.acquire_texture(
                        &TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
                            .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST),
                    )?;
                    let target = full.acquire_texture(
                        &TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm).with_usage(
                            TextureUsages::RENDER_ATTACHMENT
                                | TextureUsages::COPY_SRC
                                | TextureUsages::COPY_DST,
                        ),
                    )?;
                    full.register_texture("scissored-source", source.texture().clone());
                    full.register_texture("scissored-target", target.texture().clone());

                    for (texture, surface_id, seed) in [
                        (source.texture(), "scissored-source", SEED_RGBA),
                        (
                            target.texture(),
                            "scissored-target",
                            UNCOVERED_SENTINEL_RGBA,
                        ),
                    ] {
                        let (_pool_id, seed_buffer) =
                            full.acquire_pixel_buffer(64, 64, PixelFormat::Rgba32)?;
                        let plane = seed_buffer.buffer_ref().plane_base_address(0);
                        unsafe {
                            for pixel in 0..(64 * 64) {
                                std::ptr::copy_nonoverlapping(
                                    seed.as_ptr(),
                                    plane.add(pixel * 4),
                                    4,
                                );
                            }
                        }
                        full.copy_pixel_buffer_to_texture(
                            &seed_buffer,
                            texture,
                            surface_id,
                            64,
                            64,
                        )?;
                    }
                    Ok((source, target))
                })
                .expect("a seeded source and a seeded colour target");

            let mut run = make_run_req("run-scissored", &kernel_id, "scissored-target");
            run.bindings = vec![EscalateRequestRunGraphicsDrawBinding {
                kind: EscalateGraphicsBindingKind::SampledTexture,
                name: "source_image".to_string(),
                surface_uuid: "scissored-source".to_string(),
            }];
            run.extent_width = 64;
            run.extent_height = 64;
            run.scissor = Some(EscalateRequestRunGraphicsDrawScissor {
                x: 0,
                y: 0,
                width: 32,
                height: 64,
            });
            match handle_escalate_op(&sandbox, &registry, EscalateRequest::RunGraphicsDraw(run))
                .expect("must produce a response")
            {
                EscalateResponse::Ok(_) => {}
                other => panic!("the scissored draw failed: {other:?}"),
            }

            let rendered = sandbox
                .escalate(|full| {
                    let readback = full.create_texture_readback(
                        "scissored-readback",
                        64,
                        64,
                        TextureFormat::Rgba8Unorm,
                    )?;
                    let ticket = readback.submit(
                        held.1.texture(),
                        crate::core::rhi::TextureSourceLayout::ColorAttachment,
                    )?;
                    Ok(readback.wait_and_read(ticket, 2_000_000_000)?.to_vec())
                })
                .expect("the colour target reads back");
            for (pixel_index, pixel) in rendered.chunks_exact(4).enumerate() {
                if pixel_index % 64 < 32 {
                    assert_eq!(
                        pixel, INVERTED_RGBA,
                        "pixel {pixel_index} is inside the scissor and must be the inverted seed"
                    );
                } else {
                    assert_eq!(
                        pixel, TRANSPARENT_BLACK_RGBA,
                        "pixel {pixel_index} is outside the scissor, so nothing painted it — the \
                         pass must have cleared it rather than loaded contents its own transition \
                         from UNDEFINED had already discarded"
                    );
                }
            }
            drop(held);
        }

        /// A draw whose binding and colour target are one texture is refused:
        /// the pass discards a colour target's contents on entry, so the
        /// binding would read pixels the draw has already thrown away.
        #[test]
        fn a_draw_binding_its_own_colour_target_is_refused() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("a_draw_binding_its_own_colour_target: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let kernel_id = register_graphics_kernel_or_panic(
                &sandbox,
                &registry,
                register_from_glsl("reg-alias", INVERT_SAMPLED_INPUT_FRAGMENT_GLSL),
            )
            .handle_id;
            let held = sandbox
                .escalate(|full| {
                    let texture = full.acquire_texture(
                        &TexturePoolDescriptor::new(64, 64, TextureFormat::Rgba8Unorm).with_usage(
                            TextureUsages::TEXTURE_BINDING | TextureUsages::RENDER_ATTACHMENT,
                        ),
                    )?;
                    full.register_texture("alias-surface", texture.texture().clone());
                    Ok(texture)
                })
                .expect("one texture to name twice");

            let mut run = make_run_req("run-alias", &kernel_id, "alias-surface");
            run.bindings = vec![EscalateRequestRunGraphicsDrawBinding {
                kind: EscalateGraphicsBindingKind::SampledTexture,
                name: "source_image".to_string(),
                surface_uuid: "alias-surface".to_string(),
            }];
            run.extent_width = 64;
            run.extent_height = 64;
            let message = refusal_message(
                handle_escalate_op(&sandbox, &registry, EscalateRequest::RunGraphicsDraw(run))
                    .expect("must produce a response"),
            );
            assert!(
                message.contains("already thrown away"),
                "must say why the alias is refused, got: {message}"
            );
            drop(held);
        }
    }

    /// Host-Rust unit tests for the acceleration-structure and ray-tracing
    /// escalate handlers.
    ///
    /// Mirrors `graphics_kernel_dispatch`: the wire validation that raises
    /// before the device gate runs everywhere CI does, and everything that
    /// builds a structure or a pipeline gates on a device that exposes the
    /// ray-tracing extension chain.
    #[cfg(target_os = "linux")]
    mod ray_tracing_kernel_dispatch {
        use super::super::*;
        use super::EscalateHandleRegistry;

        use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
            EscalateRequestRegisterAccelerationStructureTlasInstance,
            EscalateRequestRegisterRayTracingKernelBinding,
            EscalateRequestRegisterRayTracingKernelGroup,
            EscalateRequestRegisterRayTracingKernelStage,
            EscalateRequestRunRayTracingKernelBinding,
        };
        use crate::core::context::GpuContext;
        use crate::core::rhi::{RayTracingShaderStage, RayTracingShaderStageFlags};

        /// Ray tracing is a device capability rather than an installed bridge,
        /// so there is nothing to set up — only a device to have or not have.
        fn make_gpu_sandbox_if_available() -> Option<GpuContextLimitedAccess> {
            GpuContext::init_for_platform_sync()
                .ok()
                .map(GpuContextLimitedAccess::new)
        }

        /// A sandbox whose device exposes the `VK_KHR_ray_tracing_pipeline`
        /// chain — what every structure build and every pipeline build needs.
        fn make_ray_tracing_sandbox_if_available() -> Option<GpuContextLimitedAccess> {
            let sandbox = make_gpu_sandbox_if_available()?;
            let ray_tracing_capable = sandbox
                .escalate(|full| Ok(full.supports_ray_tracing_pipeline()))
                .unwrap_or(false);
            ray_tracing_capable.then_some(sandbox)
        }

        fn refusal_message(response: EscalateResponse) -> String {
            match response {
                EscalateResponse::Err(err) => err.message,
                other => panic!("expected Err, got {other:?}"),
            }
        }

        /// Traces one ray per pixel straight down `-Z` at the bound structure
        /// and writes whatever the hit or miss stage left in the payload. Both
        /// bindings are resolved by the names this source gives them.
        const TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL: &str = "\
#version 460
#extension GL_EXT_ray_tracing : require
layout(set = 0, binding = 0) uniform accelerationStructureEXT scene_geometry;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D traced_output;
layout(location = 0) rayPayloadEXT vec3 traced_colour;
void main() {
    vec2 pixel_centre = vec2(gl_LaunchIDEXT.xy) + vec2(0.5);
    vec2 normalized_device_coordinate =
        pixel_centre / vec2(gl_LaunchSizeEXT.xy) * 2.0 - 1.0;
    traced_colour = vec3(0.0);
    traceRayEXT(
        scene_geometry,
        gl_RayFlagsOpaqueEXT,
        0xff,
        0, 0, 0,
        vec3(normalized_device_coordinate.x, -normalized_device_coordinate.y, 1.0),
        0.001,
        vec3(0.0, 0.0, -1.0),
        100.0,
        0
    );
    imageStore(traced_output, ivec2(gl_LaunchIDEXT.xy), vec4(traced_colour, 1.0));
}
";

        /// The same pass with the payload inverted before it is stored, so
        /// registering it produces a different pipeline — and therefore a
        /// different kernel id — from [`TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL`].
        const TRACE_AND_INVERT_RAY_GEN_GLSL: &str = "\
#version 460
#extension GL_EXT_ray_tracing : require
layout(set = 0, binding = 0) uniform accelerationStructureEXT scene_geometry;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D traced_output;
layout(location = 0) rayPayloadEXT vec3 traced_colour;
void main() {
    vec2 pixel_centre = vec2(gl_LaunchIDEXT.xy) + vec2(0.5);
    vec2 normalized_device_coordinate =
        pixel_centre / vec2(gl_LaunchSizeEXT.xy) * 2.0 - 1.0;
    traced_colour = vec3(0.0);
    traceRayEXT(
        scene_geometry,
        gl_RayFlagsOpaqueEXT,
        0xff,
        0, 0, 0,
        vec3(normalized_device_coordinate.x, -normalized_device_coordinate.y, 1.0),
        0.001,
        vec3(0.0, 0.0, -1.0),
        100.0,
        0
    );
    imageStore(
        traced_output,
        ivec2(gl_LaunchIDEXT.xy),
        vec4(vec3(1.0) - traced_colour, 1.0)
    );
}
";

        /// A ray that hit nothing paints black.
        const MISS_PAINTS_BLACK_GLSL: &str = "\
#version 460
#extension GL_EXT_ray_tracing : require
layout(location = 0) rayPayloadInEXT vec3 traced_colour;
void main() {
    traced_colour = vec3(0.0);
}
";

        /// A ray that hit the scene's one triangle paints white, so a traced
        /// pixel says which of the two stages ran for it.
        const CLOSEST_HIT_PAINTS_WHITE_GLSL: &str = "\
#version 460
#extension GL_EXT_ray_tracing : require
layout(location = 0) rayPayloadInEXT vec3 traced_colour;
void main() {
    traced_colour = vec3(1.0);
}
";

        /// A pixel the ray hit, a pixel it missed, and the sentinel the storage
        /// image is seeded with so an untouched pixel is distinguishable from
        /// either.
        const HIT_RGBA: [u8; 4] = [255, 255, 255, 255];
        const MISSED_RGBA: [u8; 4] = [0, 0, 0, 255];
        const UNTRACED_SENTINEL_RGBA: [u8; 4] = [255, 0, 255, 255];

        /// One triangle facing the launch grid, centred on the origin so a
        /// trace over the whole grid both hits and misses it.
        const A_SCENES_TRIANGLE_VERTICES: &[f32] = &[0.0, -0.5, 0.0, -0.5, 0.5, 0.0, 0.5, 0.5, 0.0];
        const A_SCENES_TRIANGLE_INDICES: &[u32] = &[0, 1, 2];

        const TRACED_GRID_WIDTH: u32 = 64;
        const TRACED_GRID_HEIGHT: u32 = 64;

        fn bytes_to_hex(bytes: &[u8]) -> String {
            let mut hex = String::with_capacity(bytes.len() * 2);
            for byte in bytes {
                hex.push_str(&format!("{byte:02x}"));
            }
            hex
        }

        /// Encode `[f32]` as the lowercase hex blob the wire expects.
        fn vertex_hex(vertices: &[f32]) -> String {
            let mut bytes = Vec::with_capacity(vertices.len() * 4);
            for vertex in vertices {
                bytes.extend_from_slice(&vertex.to_le_bytes());
            }
            bytes_to_hex(&bytes)
        }

        /// Encode `[u32]` as the lowercase hex blob the wire expects.
        fn index_hex(indices: &[u32]) -> String {
            let mut bytes = Vec::with_capacity(indices.len() * 4);
            for index in indices {
                bytes.extend_from_slice(&index.to_le_bytes());
            }
            bytes_to_hex(&bytes)
        }

        fn make_blas_req(
            request_id: &str,
            vertices_hex: &str,
            indices_hex: &str,
        ) -> EscalateRequestRegisterAccelerationStructureBlas {
            EscalateRequestRegisterAccelerationStructureBlas {
                request_id: request_id.to_string(),
                label: "a-scenes-triangle".to_string(),
                vertices_hex: vertices_hex.to_string(),
                indices_hex: indices_hex.to_string(),
            }
        }

        fn make_tlas_req(
            request_id: &str,
            blas_id: &str,
        ) -> EscalateRequestRegisterAccelerationStructureTlas {
            EscalateRequestRegisterAccelerationStructureTlas {
                request_id: request_id.to_string(),
                label: "a-scenes-instance".to_string(),
                instances: vec![EscalateRequestRegisterAccelerationStructureTlasInstance {
                    blas_id: blas_id.to_string(),
                    transform: vec![
                        1.0, 0.0, 0.0, 0.0, //
                        0.0, 1.0, 0.0, 0.0, //
                        0.0, 0.0, 1.0, 0.0,
                    ],
                    custom_index: 7,
                    mask: 0xff,
                    sbt_record_offset: 0,
                    flags: 0,
                }],
            }
        }

        fn general_group(stage_index: u32) -> EscalateRequestRegisterRayTracingKernelGroup {
            EscalateRequestRegisterRayTracingKernelGroup {
                kind: EscalateRequestRegisterRayTracingKernelGroupKind::General,
                general_stage: stage_index,
                closest_hit_stage: RAY_TRACING_STAGE_INDEX_NONE,
                any_hit_stage: RAY_TRACING_STAGE_INDEX_NONE,
                intersection_stage: RAY_TRACING_STAGE_INDEX_NONE,
            }
        }

        fn triangles_hit_group(
            closest_hit_stage_index: u32,
        ) -> EscalateRequestRegisterRayTracingKernelGroup {
            EscalateRequestRegisterRayTracingKernelGroup {
                kind: EscalateRequestRegisterRayTracingKernelGroupKind::TrianglesHit,
                general_stage: RAY_TRACING_STAGE_INDEX_NONE,
                closest_hit_stage: closest_hit_stage_index,
                any_hit_stage: RAY_TRACING_STAGE_INDEX_NONE,
                intersection_stage: RAY_TRACING_STAGE_INDEX_NONE,
            }
        }

        fn stage_from_glsl(
            stage: EscalateRequestRegisterRayTracingKernelStageStage,
            source: &str,
        ) -> EscalateRequestRegisterRayTracingKernelStage {
            EscalateRequestRegisterRayTracingKernelStage {
                entry_point: String::new(),
                source: source.to_string(),
                spv_hex: String::new(),
                stage,
            }
        }

        /// The three-stage kernel every registration test starts from, built
        /// from GLSL — which is what the wire carries now that the engine owns
        /// compilation. Tests that need a specific shape mutate fields after
        /// calling.
        fn register_from_glsl(
            request_id: &str,
            ray_gen_source: &str,
        ) -> EscalateRequestRegisterRayTracingKernel {
            EscalateRequestRegisterRayTracingKernel {
                bindings: vec![
                    EscalateRequestRegisterRayTracingKernelBinding {
                        kind: EscalateRayTracingBindingKind::AccelerationStructure,
                        name: "scene_geometry".to_string(),
                        stages: RayTracingShaderStageFlags::RAYGEN.bits(),
                    },
                    EscalateRequestRegisterRayTracingKernelBinding {
                        kind: EscalateRayTracingBindingKind::StorageImage,
                        name: "traced_output".to_string(),
                        stages: RayTracingShaderStageFlags::RAYGEN.bits(),
                    },
                ],
                groups: vec![general_group(0), general_group(1), triangles_hit_group(2)],
                label: "a-tracing-kernel".to_string(),
                max_recursion_depth: 1,
                push_constant_size: 0,
                push_constant_stages: 0,
                request_id: request_id.to_string(),
                stages: vec![
                    stage_from_glsl(
                        EscalateRequestRegisterRayTracingKernelStageStage::RayGen,
                        ray_gen_source,
                    ),
                    stage_from_glsl(
                        EscalateRequestRegisterRayTracingKernelStageStage::Miss,
                        MISS_PAINTS_BLACK_GLSL,
                    ),
                    stage_from_glsl(
                        EscalateRequestRegisterRayTracingKernelStageStage::ClosestHit,
                        CLOSEST_HIT_PAINTS_WHITE_GLSL,
                    ),
                ],
            }
        }

        /// Baseline `run_ray_tracing_kernel` request — the scene bound by the
        /// raygen's own name for it, the storage image by its own.
        fn make_run_req(
            request_id: &str,
            kernel_id: &str,
            tlas_id: &str,
            output_surface_uuid: &str,
        ) -> EscalateRequestRunRayTracingKernel {
            EscalateRequestRunRayTracingKernel {
                bindings: vec![
                    EscalateRequestRunRayTracingKernelBinding {
                        kind: EscalateRayTracingBindingKind::AccelerationStructure,
                        name: "scene_geometry".to_string(),
                        target_id: tlas_id.to_string(),
                    },
                    EscalateRequestRunRayTracingKernelBinding {
                        kind: EscalateRayTracingBindingKind::StorageImage,
                        name: "traced_output".to_string(),
                        target_id: output_surface_uuid.to_string(),
                    },
                ],
                depth: 1,
                height: TRACED_GRID_HEIGHT,
                kernel_id: kernel_id.to_string(),
                push_constants_hex: String::new(),
                request_id: request_id.to_string(),
                width: TRACED_GRID_WIDTH,
            }
        }

        fn register_ray_tracing_kernel_or_panic(
            sandbox: &GpuContextLimitedAccess,
            registry: &EscalateHandleRegistry,
            req: EscalateRequestRegisterRayTracingKernel,
        ) -> EscalateResponseOk {
            let response = handle_escalate_op(
                sandbox,
                registry,
                EscalateRequest::RegisterRayTracingKernel(req),
            )
            .expect("must produce a response");
            match response {
                EscalateResponse::Ok(ok) => ok,
                other => panic!("registering the ray-tracing kernel failed: {other:?}"),
            }
        }

        fn register_acceleration_structure_or_panic(
            sandbox: &GpuContextLimitedAccess,
            registry: &EscalateHandleRegistry,
            req: EscalateRequest,
        ) -> String {
            let response =
                handle_escalate_op(sandbox, registry, req).expect("must produce a response");
            match response {
                EscalateResponse::Ok(ok) => ok.handle_id,
                other => panic!("registering the acceleration structure failed: {other:?}"),
            }
        }

        /// One registered kernel over one registered scene, writing into one
        /// registered storage image seeded with [`UNTRACED_SENTINEL_RGBA`].
        ///
        /// The pooled handle is held for the caller's lifetime: dropping it
        /// hands the slot back, and the registration would then name a recycled
        /// texture.
        struct ARayTracedSceneUnderTest {
            sandbox: GpuContextLimitedAccess,
            registry: std::sync::Arc<EscalateHandleRegistry>,
            kernel_id: String,
            blas_id: String,
            tlas_id: String,
            _held_output: PooledTextureHandle,
        }

        const A_TRACED_SCENES_OUTPUT_SURFACE_UUID: &str = "traced-output-surface";

        fn make_ray_traced_scene_if_available() -> Option<ARayTracedSceneUnderTest> {
            let sandbox = make_ray_tracing_sandbox_if_available()?;
            let registry = EscalateHandleRegistry::new();
            let blas_id = register_acceleration_structure_or_panic(
                &sandbox,
                &registry,
                EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                    "scene-blas",
                    &vertex_hex(A_SCENES_TRIANGLE_VERTICES),
                    &index_hex(A_SCENES_TRIANGLE_INDICES),
                )),
            );
            let tlas_id = register_acceleration_structure_or_panic(
                &sandbox,
                &registry,
                EscalateRequest::RegisterAccelerationStructureTlas(make_tlas_req(
                    "scene-tlas",
                    &blas_id,
                )),
            );
            let kernel_id = register_ray_tracing_kernel_or_panic(
                &sandbox,
                &registry,
                register_from_glsl("scene-kernel", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL),
            )
            .handle_id;

            let held_output = sandbox
                .escalate(|full| {
                    let output = full.acquire_texture(
                        &TexturePoolDescriptor::new(
                            TRACED_GRID_WIDTH,
                            TRACED_GRID_HEIGHT,
                            TextureFormat::Rgba8Unorm,
                        )
                        .with_usage(
                            TextureUsages::STORAGE_BINDING
                                | TextureUsages::COPY_DST
                                | TextureUsages::COPY_SRC,
                        ),
                    )?;
                    full.register_texture(
                        A_TRACED_SCENES_OUTPUT_SURFACE_UUID,
                        output.texture().clone(),
                    );

                    let (_pool_id, seed_buffer) = full.acquire_pixel_buffer(
                        TRACED_GRID_WIDTH,
                        TRACED_GRID_HEIGHT,
                        PixelFormat::Rgba32,
                    )?;
                    let plane = seed_buffer.buffer_ref().plane_base_address(0);
                    unsafe {
                        for pixel in 0..(TRACED_GRID_WIDTH as usize * TRACED_GRID_HEIGHT as usize) {
                            std::ptr::copy_nonoverlapping(
                                UNTRACED_SENTINEL_RGBA.as_ptr(),
                                plane.add(pixel * 4),
                                4,
                            );
                        }
                    }
                    full.copy_pixel_buffer_to_texture(
                        &seed_buffer,
                        output.texture(),
                        A_TRACED_SCENES_OUTPUT_SURFACE_UUID,
                        TRACED_GRID_WIDTH,
                        TRACED_GRID_HEIGHT,
                    )?;
                    Ok(output)
                })
                .expect("a seeded storage image to trace into");

            Some(ARayTracedSceneUnderTest {
                sandbox,
                registry,
                kernel_id,
                blas_id,
                tlas_id,
                _held_output: held_output,
            })
        }

        impl ARayTracedSceneUnderTest {
            fn run_req(&self, request_id: &str) -> EscalateRequestRunRayTracingKernel {
                make_run_req(
                    request_id,
                    &self.kernel_id,
                    &self.tlas_id,
                    A_TRACED_SCENES_OUTPUT_SURFACE_UUID,
                )
            }

            fn trace(&self, req: EscalateRequestRunRayTracingKernel) -> EscalateResponse {
                handle_escalate_op(
                    &self.sandbox,
                    &self.registry,
                    EscalateRequest::RunRayTracingKernel(req),
                )
                .expect("must produce a response")
            }
        }

        // ----- wire validation, before any device -----------------------

        /// Both blobs are decoded before the escalate hop, so a malformed one
        /// is refused without touching the GPU at all.
        #[test]
        fn register_blas_with_invalid_vertex_hex_returns_err() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("register_blas_with_invalid_vertex_hex: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let message = refusal_message(
                handle_escalate_op(
                    &sandbox,
                    &registry,
                    EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                        "blas-bad-vertices",
                        "xyz123",
                        &index_hex(A_SCENES_TRIANGLE_INDICES),
                    )),
                )
                .expect("must produce a response"),
            );
            assert!(message.contains("vertices_hex"), "got: {message}");
        }

        #[test]
        fn register_blas_with_invalid_index_hex_returns_err() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("register_blas_with_invalid_index_hex: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let message = refusal_message(
                handle_escalate_op(
                    &sandbox,
                    &registry,
                    EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                        "blas-bad-indices",
                        &vertex_hex(A_SCENES_TRIANGLE_VERTICES),
                        "xyz123",
                    )),
                )
                .expect("must produce a response"),
            );
            assert!(message.contains("indices_hex"), "got: {message}");
        }

        /// A blob that is not a whole number of vertices — or of triangles —
        /// names geometry that does not exist, and is refused before a build.
        #[test]
        fn register_blas_with_a_partial_vertex_or_triangle_is_refused() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("register_blas_with_a_partial_vertex_or_triangle: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            for (request_id, vertices_hex, indices_hex) in [
                (
                    "blas-partial-vertex",
                    "00".repeat(11),
                    index_hex(A_SCENES_TRIANGLE_INDICES),
                ),
                (
                    "blas-partial-triangle",
                    vertex_hex(A_SCENES_TRIANGLE_VERTICES),
                    "00".repeat(8),
                ),
            ] {
                let message = refusal_message(
                    handle_escalate_op(
                        &sandbox,
                        &registry,
                        EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                            request_id,
                            &vertices_hex,
                            &indices_hex,
                        )),
                    )
                    .expect("must produce a response"),
                );
                assert!(
                    message.contains("multiple of 12"),
                    "{request_id} got: {message}"
                );
            }
        }

        #[test]
        fn register_tlas_with_no_instances_is_refused() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("register_tlas_with_no_instances: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let mut req = make_tlas_req("tlas-empty", "unused");
            req.instances.clear();
            let message = refusal_message(
                handle_escalate_op(
                    &sandbox,
                    &registry,
                    EscalateRequest::RegisterAccelerationStructureTlas(req),
                )
                .expect("must produce a response"),
            );
            assert!(message.contains("at least one instance"), "got: {message}");
        }

        #[test]
        fn register_tlas_with_a_transform_that_is_not_a_row_major_3x4_is_refused() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("register_tlas_with_a_wrong_length_transform: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let mut req = make_tlas_req("tlas-bad-transform", "unused");
            req.instances[0].transform = vec![1.0; 11];
            let message = refusal_message(
                handle_escalate_op(
                    &sandbox,
                    &registry,
                    EscalateRequest::RegisterAccelerationStructureTlas(req),
                )
                .expect("must produce a response"),
            );
            assert!(message.contains("transform"), "got: {message}");
        }

        #[test]
        fn register_tlas_with_a_mask_wider_than_eight_bits_is_refused() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("register_tlas_with_an_oversized_mask: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let mut req = make_tlas_req("tlas-bad-mask", "unused");
            req.instances[0].mask = 0xfff;
            let message = refusal_message(
                handle_escalate_op(
                    &sandbox,
                    &registry,
                    EscalateRequest::RegisterAccelerationStructureTlas(req),
                )
                .expect("must produce a response"),
            );
            assert!(message.contains("mask"), "got: {message}");
        }

        #[test]
        fn run_with_invalid_push_constants_hex_returns_err() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("run_with_invalid_push_constants_hex: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let mut req = make_run_req("trace-bad-push", "kernel-x", "tlas-x", "surface-x");
            req.push_constants_hex = "qq".to_string();
            let message = refusal_message(
                handle_escalate_op(
                    &sandbox,
                    &registry,
                    EscalateRequest::RunRayTracingKernel(req),
                )
                .expect("must produce a response"),
            );
            assert!(message.contains("push_constants_hex"), "got: {message}");
        }

        /// A stage mask is a bitfield the caller writes by hand, and a bit
        /// outside the six ray-tracing stages names a stage no pipeline has a
        /// module for at all.
        #[test]
        fn a_stage_mask_naming_a_bit_no_ray_tracing_stage_owns_is_refused() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("a_stage_mask_naming_an_unowned_bit: no GPU — skipping");
                return;
            };
            let bit_no_stage_owns = RayTracingShaderStageFlags::ALL.bits() + 1;

            let mut declaration = register_from_glsl("k", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL);
            declaration.bindings[0].stages = bit_no_stage_owns;
            let message = prepare_ray_tracing_kernel_registration(&sandbox, declaration)
                .err()
                .expect("a binding declared for a stage no pipeline has must be refused");
            assert!(
                message.contains("scene_geometry") && message.contains("no ray-tracing stage owns"),
                "must name the binding and why the mask is wrong, got: {message}"
            );

            let mut push_constants = register_from_glsl("k", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL);
            push_constants.push_constant_stages = bit_no_stage_owns;
            let message = prepare_ray_tracing_kernel_registration(&sandbox, push_constants)
                .err()
                .expect("a push-constant range declared for the same stage must be refused");
            assert!(
                message.contains("push_constant_stages"),
                "must name the field, got: {message}"
            );
        }

        /// A procedural hit group without an intersection stage is a group with
        /// nothing to intersect, and the sentinel is what "absent" looks like on
        /// a wire where the field is always present.
        #[test]
        fn a_procedural_group_leaving_intersection_at_the_sentinel_is_refused() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("a_procedural_group_without_an_intersection: no GPU — skipping");
                return;
            };
            let mut req = register_from_glsl("k-proc", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL);
            req.groups[2] = EscalateRequestRegisterRayTracingKernelGroup {
                kind: EscalateRequestRegisterRayTracingKernelGroupKind::ProceduralHit,
                general_stage: RAY_TRACING_STAGE_INDEX_NONE,
                closest_hit_stage: 2,
                any_hit_stage: RAY_TRACING_STAGE_INDEX_NONE,
                intersection_stage: RAY_TRACING_STAGE_INDEX_NONE,
            };
            let message = prepare_ray_tracing_kernel_registration(&sandbox, req)
                .err()
                .expect("a procedural group with no intersection stage must be refused");
            assert!(message.contains("procedural_hit"), "got: {message}");
        }

        /// A stage's hex is decoded before the escalate hop, and the refusal
        /// names the stage it came from rather than just "the kernel".
        #[test]
        fn register_with_invalid_stage_hex_returns_err() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("register_with_invalid_stage_hex: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let mut req = register_from_glsl("k-bad-hex", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL);
            req.stages[1].source = String::new();
            req.stages[1].spv_hex = "qq".to_string();
            let message = refusal_message(
                handle_escalate_op(
                    &sandbox,
                    &registry,
                    EscalateRequest::RegisterRayTracingKernel(req),
                )
                .expect("must produce a response"),
            );
            assert!(message.contains("stages[1].spv_hex"), "got: {message}");
        }

        /// Every ray-tracing stage the wire can name compiles for the stage it
        /// names, and reaches the kernel classified as that stage.
        ///
        /// Two separate six-arm mappings run per stage —
        /// `ray_tracing_pipeline_stage_from_wire` picks what the compiler
        /// targets and `ray_tracing_stage_from_wire` picks what the shader
        /// group is built from — so a swapped pair in either would build a miss
        /// shader as a closest-hit without complaint. Each body below is legal
        /// only in its own stage: `rayPayloadEXT` is raygen-only,
        /// `rayPayloadInEXT` is miss/hit-only, `reportIntersectionEXT` is
        /// intersection-only and `callableDataInEXT` is callable-only, so a
        /// mis-mapped compile target fails to compile rather than quietly
        /// producing the wrong module.
        #[test]
        fn every_ray_tracing_wire_stage_compiles_for_the_stage_it_names() {
            let Some(sandbox) = make_gpu_sandbox_if_available() else {
                println!("every_ray_tracing_wire_stage_compiles: no GPU — skipping");
                return;
            };
            let stages = [
                (
                    EscalateRequestRegisterRayTracingKernelStageStage::RayGen,
                    RayTracingShaderStage::RayGen,
                    "layout(location = 0) rayPayloadEXT vec3 payload;\nvoid main() { payload = vec3(1.0); }",
                ),
                (
                    EscalateRequestRegisterRayTracingKernelStageStage::Miss,
                    RayTracingShaderStage::Miss,
                    "layout(location = 0) rayPayloadInEXT vec3 payload;\nvoid main() { payload = vec3(0.0); }",
                ),
                (
                    EscalateRequestRegisterRayTracingKernelStageStage::ClosestHit,
                    RayTracingShaderStage::ClosestHit,
                    "layout(location = 0) rayPayloadInEXT vec3 payload;\nvoid main() { payload = vec3(0.5); }",
                ),
                (
                    EscalateRequestRegisterRayTracingKernelStageStage::AnyHit,
                    RayTracingShaderStage::AnyHit,
                    "layout(location = 0) rayPayloadInEXT vec3 payload;\nvoid main() { ignoreIntersectionEXT; }",
                ),
                (
                    EscalateRequestRegisterRayTracingKernelStageStage::Intersection,
                    RayTracingShaderStage::Intersection,
                    "hitAttributeEXT vec2 barycentric;\nvoid main() { reportIntersectionEXT(1.0, 0u); }",
                ),
                (
                    EscalateRequestRegisterRayTracingKernelStageStage::Callable,
                    RayTracingShaderStage::Callable,
                    "layout(location = 0) callableDataInEXT vec3 callable_payload;\nvoid main() { callable_payload = vec3(1.0); }",
                ),
            ];
            for (index, (wire_stage, expected_stage, body)) in stages.into_iter().enumerate() {
                let mut req = register_from_glsl(
                    &format!("rt-glsl-{index}"),
                    TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL,
                );
                req.bindings = Vec::new();
                req.groups = vec![general_group(0)];
                req.stages = vec![stage_from_glsl(
                    wire_stage,
                    &format!("#version 460\n#extension GL_EXT_ray_tracing : require\n{body}\n"),
                )];
                let prepared = prepare_ray_tracing_kernel_registration(&sandbox, req)
                    .unwrap_or_else(|e| panic!("{wire_stage:?} did not compile as itself: {e}"));
                assert_eq!(
                    prepared.stages[0].spirv.get(..4),
                    Some(&SPIRV_MAGIC_LE[..]),
                    "{wire_stage:?} reached the kernel as something other than SPIR-V"
                );
                assert_eq!(
                    prepared.stages[0].stage, expected_stage,
                    "{wire_stage:?} was classified as the wrong pipeline stage"
                );
            }
        }

        // ----- registration, over a ray-tracing device ------------------

        /// Registration hands back the shape a trace needs: the shaders' own
        /// names, each with the kind only the shaders know.
        #[test]
        fn registration_answers_with_the_shaders_binding_names_and_kinds() {
            let Some(sandbox) = make_ray_tracing_sandbox_if_available() else {
                println!("registration_answers_with_the_shaders_bindings: no RT device — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let ok = register_ray_tracing_kernel_or_panic(
                &sandbox,
                &registry,
                register_from_glsl("reg", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL),
            );
            let bindings = ok.bindings.expect("a register response carries the shape");
            assert_eq!(
                bindings
                    .iter()
                    .map(|binding| (binding.name.as_str(), binding.kind.as_str()))
                    .collect::<Vec<_>>(),
                vec![
                    ("scene_geometry", "acceleration_structure"),
                    ("traced_output", "storage_image"),
                ],
                "the raygen's own bindings, named and kinded as it declares them"
            );
        }

        /// Re-registering an identical kernel is free and keeps its id; a
        /// different raygen stage is a different pipeline and gets its own.
        #[test]
        fn an_identical_registration_keeps_its_kernel_id_and_a_different_one_gets_its_own() {
            let Some(sandbox) = make_ray_tracing_sandbox_if_available() else {
                println!("an_identical_registration_keeps_its_kernel_id: no RT device — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let first = register_ray_tracing_kernel_or_panic(
                &sandbox,
                &registry,
                register_from_glsl("a", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL),
            )
            .handle_id;
            let second = register_ray_tracing_kernel_or_panic(
                &sandbox,
                &registry,
                register_from_glsl("b", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL),
            )
            .handle_id;
            assert_eq!(
                first, second,
                "an identical descriptor must produce the same kernel_id"
            );

            let other = register_ray_tracing_kernel_or_panic(
                &sandbox,
                &registry,
                register_from_glsl("c", TRACE_AND_INVERT_RAY_GEN_GLSL),
            )
            .handle_id;
            assert_ne!(
                first, other,
                "a different raygen stage must produce a different kernel_id"
            );

            let held = sandbox
                .escalate(|full| {
                    Ok((
                        full.ray_tracing_kernel_by_id(&first),
                        full.ray_tracing_kernel_by_id(&second),
                    ))
                })
                .expect("the cache answers inside an escalate scope");
            let (a, b) = (held.0.expect("cached"), held.1.expect("cached"));
            assert!(
                std::sync::Arc::ptr_eq(&a, &b),
                "the second registration must reuse the first kernel, not build another"
            );
        }

        /// The cache key covers the shaders and the pipeline, not the caller's
        /// assertion — so a wrong declaration refuses identically whether or
        /// not somebody registered this kernel first.
        #[test]
        fn a_wrong_declaration_is_refused_even_when_the_kernel_is_cached() {
            let Some(sandbox) = make_ray_tracing_sandbox_if_available() else {
                println!("a_wrong_declaration_is_refused_when_cached: no RT device — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            register_ray_tracing_kernel_or_panic(
                &sandbox,
                &registry,
                register_from_glsl("warm", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL),
            );

            let mut req = register_from_glsl("reg-wrong", TRACE_ONE_RAY_PER_PIXEL_RAY_GEN_GLSL);
            req.bindings = vec![EscalateRequestRegisterRayTracingKernelBinding {
                kind: EscalateRayTracingBindingKind::StorageBuffer,
                name: "scene_parameters".to_string(),
                stages: 0,
            }];
            let message = refusal_message(
                handle_escalate_op(
                    &sandbox,
                    &registry,
                    EscalateRequest::RegisterRayTracingKernel(req),
                )
                .expect("must produce a response"),
            );
            assert!(
                message.contains("`scene_parameters`") && message.contains("`scene_geometry`"),
                "the refusal must name the bogus binding and the shaders' own: {message}"
            );
        }

        // ----- acceleration structures, over a ray-tracing device --------

        /// Unlike a kernel, a structure holds device memory proportional to its
        /// mesh — so every registration mints its own id rather than colliding
        /// on content, and a TLAS is a different structure from its BLAS.
        #[test]
        fn every_acceleration_structure_registration_gets_its_own_id() {
            let Some(sandbox) = make_ray_tracing_sandbox_if_available() else {
                println!("every_acceleration_structure_registration: no RT device — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let first_blas = register_acceleration_structure_or_panic(
                &sandbox,
                &registry,
                EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                    "blas-a",
                    &vertex_hex(A_SCENES_TRIANGLE_VERTICES),
                    &index_hex(A_SCENES_TRIANGLE_INDICES),
                )),
            );
            let second_blas = register_acceleration_structure_or_panic(
                &sandbox,
                &registry,
                EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                    "blas-b",
                    &vertex_hex(A_SCENES_TRIANGLE_VERTICES),
                    &index_hex(A_SCENES_TRIANGLE_INDICES),
                )),
            );
            assert_ne!(
                first_blas, second_blas,
                "an identical mesh registered twice is two structures, not one"
            );

            let tlas = register_acceleration_structure_or_panic(
                &sandbox,
                &registry,
                EscalateRequest::RegisterAccelerationStructureTlas(make_tlas_req(
                    "tlas-a",
                    &first_blas,
                )),
            );
            assert_ne!(tlas, first_blas);
            assert_ne!(tlas, second_blas);
        }

        /// A structure is the one escalate-minted resource whose device memory
        /// is proportional to what the caller supplied, so a long-running helper
        /// has to be able to hand it back — the same `release_handle` a surface
        /// is handed back through, since nothing else would reach the registry.
        #[test]
        fn a_registered_acceleration_structure_is_released_through_release_handle() {
            let Some(sandbox) = make_ray_tracing_sandbox_if_available() else {
                println!(
                    "a_registered_acceleration_structure_is_released: no RT device — skipping"
                );
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let blas = register_acceleration_structure_or_panic(
                &sandbox,
                &registry,
                EscalateRequest::RegisterAccelerationStructureBlas(make_blas_req(
                    "blas-to-release",
                    &vertex_hex(A_SCENES_TRIANGLE_VERTICES),
                    &index_hex(A_SCENES_TRIANGLE_INDICES),
                )),
            );

            let released = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
                    request_id: "release-blas".to_string(),
                    handle_id: blas.clone(),
                }),
            )
            .expect("must produce a response");
            assert!(
                matches!(released, EscalateResponse::Ok(_)),
                "releasing a registered structure must succeed: {released:?}"
            );

            // The id is gone, not merely unreferenced: a second release finds
            // nothing, and a trace naming it would too.
            let released_twice = handle_escalate_op(
                &sandbox,
                &registry,
                EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
                    request_id: "release-blas-again".to_string(),
                    handle_id: blas,
                }),
            )
            .expect("must produce a response");
            let message = refusal_message(released_twice);
            assert!(message.contains("not found in registry"), "{message}");
        }

        #[test]
        fn a_tlas_instance_naming_an_unregistered_structure_is_refused() {
            let Some(sandbox) = make_ray_tracing_sandbox_if_available() else {
                println!(
                    "a_tlas_instance_naming_an_unregistered_structure: no RT device — skipping"
                );
                return;
            };
            let registry = EscalateHandleRegistry::new();
            let message = refusal_message(
                handle_escalate_op(
                    &sandbox,
                    &registry,
                    EscalateRequest::RegisterAccelerationStructureTlas(make_tlas_req(
                        "tlas-unknown",
                        "definitely-not-a-registered-structure",
                    )),
                )
                .expect("must produce a response"),
            );
            assert!(
                message.contains("names no acceleration structure registered under id"),
                "got: {message}"
            );
        }

        /// A TLAS instance references a bottom-level structure. Naming a
        /// top-level one is a caller mistake the registry can catch, and every
        /// id looks alike from the outside.
        #[test]
        fn a_tlas_instance_naming_a_top_level_structure_is_refused() {
            let Some(scene) = make_ray_traced_scene_if_available() else {
                println!("a_tlas_instance_naming_a_top_level_structure: no RT device — skipping");
                return;
            };
            let message = refusal_message(
                handle_escalate_op(
                    &scene.sandbox,
                    &scene.registry,
                    EscalateRequest::RegisterAccelerationStructureTlas(make_tlas_req(
                        "tlas-over-tlas",
                        &scene.tlas_id,
                    )),
                )
                .expect("must produce a response"),
            );
            assert!(
                message.contains("is a top-level structure"),
                "got: {message}"
            );
        }

        // ----- tracing, over a ray-tracing device ------------------------

        #[test]
        fn tracing_with_an_unregistered_kernel_id_is_refused() {
            let Some(scene) = make_ray_traced_scene_if_available() else {
                println!("tracing_with_an_unregistered_kernel_id: no RT device — skipping");
                return;
            };
            let mut req = scene.run_req("trace-unknown-kernel");
            req.kernel_id = "definitely-not-a-registered-kernel".to_string();
            let message = refusal_message(scene.trace(req));
            assert!(
                message.contains("no kernel registered under id"),
                "got: {message}"
            );
        }

        /// An acceleration-structure binding resolves through the structure
        /// registry rather than through a surface, so an id no registration
        /// minted is refused there rather than falling through to the surface
        /// planner and getting a surface's error text.
        #[test]
        fn a_trace_naming_an_unregistered_acceleration_structure_is_refused() {
            let Some(scene) = make_ray_traced_scene_if_available() else {
                println!("a_trace_naming_an_unregistered_structure: no RT device — skipping");
                return;
            };
            let mut req = scene.run_req("trace-unknown-structure");
            req.bindings[0].target_id = "definitely-not-a-registered-structure".to_string();
            let message = refusal_message(scene.trace(req));
            assert!(
                message.contains("binding `scene_geometry` names no acceleration structure"),
                "must name the binding, got: {message}"
            );
        }

        /// The structure a trace binds is the top-level one a
        /// `register_acceleration_structure_tlas` returned; a BLAS id is the
        /// same shape of string and would otherwise reach the descriptor.
        #[test]
        fn a_trace_binding_a_bottom_level_structure_is_refused() {
            let Some(scene) = make_ray_traced_scene_if_available() else {
                println!("a_trace_binding_a_bottom_level_structure: no RT device — skipping");
                return;
            };
            let mut req = scene.run_req("trace-blas");
            req.bindings[0].target_id = scene.blas_id.clone();
            let message = refusal_message(scene.trace(req));
            assert!(
                message.contains("is a bottom-level structure"),
                "got: {message}"
            );
        }

        /// The acceleration structure never reaches the surface planner, so its
        /// missing-binding rule is enforced separately — and has to fire.
        #[test]
        fn a_declared_acceleration_structure_left_out_is_refused() {
            let Some(scene) = make_ray_traced_scene_if_available() else {
                println!("a_declared_acceleration_structure_left_out: no RT device — skipping");
                return;
            };
            let mut req = scene.run_req("trace-no-structure");
            req.bindings
                .retain(|binding| binding.name != "scene_geometry");
            let message = refusal_message(scene.trace(req));
            assert!(
                message.contains("binding `scene_geometry` was not supplied"),
                "must name the missing binding, got: {message}"
            );
            assert!(
                message.contains("do not persist between traces"),
                "must say why there is no fallback, got: {message}"
            );
        }

        /// Supplying the structure under another kind would otherwise send it
        /// to the surface planner, which would look for a surface named by an
        /// `as_id`.
        #[test]
        fn an_acceleration_structure_supplied_as_another_kind_is_refused() {
            let Some(scene) = make_ray_traced_scene_if_available() else {
                println!("an_acceleration_structure_supplied_as_another_kind: no RT — skipping");
                return;
            };
            let mut req = scene.run_req("trace-wrong-kind");
            req.bindings[0].kind = EscalateRayTracingBindingKind::StorageImage;
            let message = refusal_message(scene.trace(req));
            assert!(
                message.contains("binding `scene_geometry` was supplied as storage_image"),
                "must name the binding and the kind supplied, got: {message}"
            );
            assert!(
                message.contains("declares it acceleration_structure"),
                "must name the kind the kernel declares, got: {message}"
            );
        }

        /// The whole array is checked before the acceleration structures are
        /// split out of it, so a name supplied twice is refused whichever half
        /// the second copy would land in — and by the one rule the surface
        /// planner spells, not a second wording of it.
        #[test]
        fn a_name_supplied_twice_is_refused() {
            let Some(scene) = make_ray_traced_scene_if_available() else {
                println!("a_name_supplied_twice: no RT device — skipping");
                return;
            };
            let mut req = scene.run_req("trace-twice");
            let structure_binding = req.bindings[0].clone();
            assert_eq!(
                structure_binding.name, "scene_geometry",
                "the duplicate has to be the structure, which the surface planner never sees"
            );
            req.bindings.push(structure_binding);
            let message = refusal_message(scene.trace(req));
            assert!(
                message.contains("binding `scene_geometry` was supplied twice"),
                "must name the duplicate, got: {message}"
            );
            assert!(
                message.contains("`traced_output`"),
                "must name every binding this kernel declares, got: {message}"
            );
            assert!(
                message.contains("exactly once per trace"),
                "must state the rule in the caller's own noun, got: {message}"
            );
        }

        /// The op end to end, over a real device: a trace resolves the scene
        /// and the storage image by the raygen's own names for them, launches
        /// the grid, and leaves the engine's layout record agreeing with the
        /// layout the trace left the image in.
        ///
        /// The storage image is seeded by a transfer with a sentinel no stage
        /// can produce, so a trace that bound nothing fails on the pixels
        /// rather than passing on undefined contents — and because that seed
        /// leaves the image in `TRANSFER_DST_OPTIMAL`, a trace that did not
        /// barrier its bound inputs would write it through a descriptor its
        /// layout does not satisfy. Both the hit and the miss stage must have
        /// run, which is what proves the structure reached the descriptor.
        #[test]
        fn a_trace_resolves_its_bindings_by_name_and_writes_the_storage_image() {
            let Some(scene) = make_ray_traced_scene_if_available() else {
                println!("a_trace_resolves_its_bindings_by_name: no RT device — skipping");
                return;
            };
            match scene.trace(scene.run_req("trace-scene")) {
                EscalateResponse::Ok(ok) => {
                    assert_eq!(ok.request_id, "trace-scene");
                    assert_eq!(
                        ok.handle_id, scene.kernel_id,
                        "the trace response echoes the kernel_id"
                    );
                    assert!(
                        ok.timeline_value.is_none(),
                        "run_ray_tracing_kernel responses carry no timeline"
                    );
                }
                other => panic!("the trace failed: {other:?}"),
            }

            // Asserted before the readback, which transitions the image itself:
            // an unpublished layout would leave the next consumer's barrier
            // naming an oldLayout the image has already left.
            let published = scene
                .sandbox
                .escalate(|full| {
                    Ok(full
                        .resolve_texture_registration_by_surface_id(
                            A_TRACED_SCENES_OUTPUT_SURFACE_UUID,
                            None,
                            TRACED_GRID_WIDTH,
                            TRACED_GRID_HEIGHT,
                        )?
                        .current_layout())
                })
                .expect("the storage image still resolves");
            assert_eq!(
                published,
                streamlib_consumer_rhi::VulkanLayout::GENERAL,
                "the trace must publish the layout it left the storage image in"
            );

            let traced = scene
                .sandbox
                .escalate(|full| {
                    let readback = full.create_texture_readback(
                        "trace-readback",
                        TRACED_GRID_WIDTH,
                        TRACED_GRID_HEIGHT,
                        TextureFormat::Rgba8Unorm,
                    )?;
                    let ticket = readback.submit(
                        scene._held_output.texture(),
                        crate::core::rhi::TextureSourceLayout::General,
                    )?;
                    Ok(readback.wait_and_read(ticket, 2_000_000_000)?.to_vec())
                })
                .expect("the storage image reads back");

            let mut hit_pixels = 0usize;
            let mut missed_pixels = 0usize;
            for (pixel_index, pixel) in traced.chunks_exact(4).enumerate() {
                if pixel == HIT_RGBA {
                    hit_pixels += 1;
                } else if pixel == MISSED_RGBA {
                    missed_pixels += 1;
                } else {
                    panic!(
                        "pixel {pixel_index} is {pixel:?}, which no stage of this kernel writes — \
                         the trace left the seeded sentinel, so `traced_output` was never written"
                    );
                }
            }
            assert!(
                hit_pixels > 0,
                "no pixel hit the scene's triangle — `scene_geometry` did not reach the descriptor"
            );
            assert!(
                missed_pixels > 0,
                "every pixel hit, so the launch grid never left the triangle and the miss stage \
                 proved nothing"
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
