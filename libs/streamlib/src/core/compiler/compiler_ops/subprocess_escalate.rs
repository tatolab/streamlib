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
    EscalateRequestAcquireCpuReadback, EscalateRequestAcquireCpuReadbackMode,
    EscalateRequestAcquireImage, EscalateRequestAcquirePixelBuffer, EscalateRequestAcquireTexture,
    EscalateRequestLog, EscalateRequestLogLevel, EscalateRequestLogSource,
    EscalateRequestReleaseHandle, EscalateRequestTryAcquireCpuReadback,
    EscalateRequestTryAcquireCpuReadbackMode,
};
use crate::_generated_::com_streamlib_escalate_response::{
    EscalateResponseContended, EscalateResponseErr, EscalateResponseOk,
    EscalateResponseOkCpuReadbackPlane,
};
use crate::_generated_::{EscalateRequest, EscalateResponse};
use crate::core::context::{PooledTextureHandle, TexturePoolDescriptor};
use crate::core::context::GpuContextLimitedAccess;
#[cfg(target_os = "linux")]
use crate::core::context::{CpuReadbackAccessMode, CpuReadbackAcquired, CpuReadbackBridge};
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
        EscalateRequest::AcquireCpuReadback(p) => Some(&p.request_id),
        EscalateRequest::TryAcquireCpuReadback(p) => Some(&p.request_id),
        EscalateRequest::ReleaseHandle(p) => Some(&p.request_id),
        EscalateRequest::Log(_) => None,
    }
}

/// Resource kept alive on behalf of a subprocess by
/// [`EscalateHandleRegistry`]. The fields are only read via the `Drop`
/// side-effect that releases them back to the host pool when removed
/// from the registry — the map keeps the resource live, the resource's
/// destructor does the release on removal.
pub(crate) enum RegisteredHandle {
    #[allow(dead_code)]
    PixelBuffer(RhiPixelBuffer),
    #[allow(dead_code)]
    Texture(PooledTextureHandle),
    /// cpu-readback acquire: holds the adapter guard plus the per-plane
    /// surface-share registrations the escalate handler created. Both
    /// are torn down together — see [`CpuReadbackHandle::drop`].
    #[cfg(target_os = "linux")]
    #[allow(dead_code)]
    CpuReadback(CpuReadbackHandle),
}

/// Lifetime bundle for a single cpu-readback acquire. Drop runs the
/// adapter's release-side work first (CPU→GPU flush on write + timeline
/// signal), then releases the per-plane surface-share entries so the
/// subprocess can no longer `check_out` stale staging buffers.
#[cfg(target_os = "linux")]
pub(crate) struct CpuReadbackHandle {
    /// Type-erased `ReadGuard` / `WriteGuard` from the cpu-readback
    /// adapter. `Option` so Drop can take it before the surface-share
    /// release loop, keeping ordering explicit.
    guard: Option<Box<dyn Send + Sync>>,
    /// Per-plane surface-share IDs registered for this acquire.
    staging_surface_ids: Vec<String>,
    /// Surface-share service handle, cloned at insert time so Drop
    /// doesn't need a sandbox reference.
    surface_store: crate::core::context::SurfaceStore,
}

#[cfg(target_os = "linux")]
impl Drop for CpuReadbackHandle {
    fn drop(&mut self) {
        // Drop the adapter guard first so the CPU→GPU flush + timeline
        // signal complete before the staging buffers are unregistered.
        self.guard.take();
        for id in self.staging_surface_ids.drain(..) {
            if let Err(e) = self.surface_store.release(&id) {
                tracing::debug!(
                    "[escalate] cpu-readback staging release for '{}' returned error: {}",
                    id,
                    e
                );
            }
        }
    }
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

    /// Register a cpu-readback acquire so the host keeps the adapter
    /// guard + per-plane surface-share entries alive until the
    /// subprocess sends `release_handle`. Linux-only: the cpu-readback
    /// adapter is Linux-only.
    #[cfg(target_os = "linux")]
    pub(crate) fn insert_cpu_readback(&self, handle_id: String, handle: CpuReadbackHandle) {
        let mut map = self.handles.lock().expect("poisoned");
        map.insert(handle_id, RegisteredHandle::CpuReadback(handle));
    }

    /// Remove a handle by id without distinguishing variant. Visible
    /// for tests; the production `release_handle` path uses
    /// [`Self::remove_handle_typed`] so it can branch on whether the
    /// removed entry was a cpu-readback acquire.
    #[cfg(test)]
    pub(crate) fn remove_handle(&self, handle_id: &str) -> bool {
        let mut map = self.handles.lock().expect("poisoned");
        map.remove(handle_id).is_some()
    }

    /// Like [`Self::remove_handle`] but reports whether the removed
    /// entry was a cpu-readback handle. Used by the escalate
    /// `release_handle` path to skip the generic surface-share release
    /// (cpu-readback handles clean up their own surface-share entries
    /// in `Drop`).
    pub(crate) fn remove_handle_typed(&self, handle_id: &str) -> RemovedHandle {
        let mut map = self.handles.lock().expect("poisoned");
        match map.remove(handle_id) {
            None => RemovedHandle::NotFound,
            #[cfg(target_os = "linux")]
            Some(RegisteredHandle::CpuReadback(_)) => RemovedHandle::CpuReadback,
            Some(_) => RemovedHandle::Generic,
        }
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

/// Discriminator returned by [`EscalateHandleRegistry::remove_handle_typed`].
pub(crate) enum RemovedHandle {
    NotFound,
    Generic,
    #[cfg(target_os = "linux")]
    CpuReadback,
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
                            cpu_readback_planes: None,
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
                        cpu_readback_planes: None,
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
                        cpu_readback_planes: None,
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
        EscalateRequest::AcquireCpuReadback(EscalateRequestAcquireCpuReadback {
            request_id: _,
            surface_id,
            mode,
        }) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_acquire_cpu_readback(sandbox, registry, rid, &surface_id, mode))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (surface_id, mode);
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "acquire_cpu_readback is only available on Linux".to_string(),
                }))
            }
        }
        EscalateRequest::TryAcquireCpuReadback(EscalateRequestTryAcquireCpuReadback {
            request_id: _,
            surface_id,
            mode,
        }) => {
            #[cfg(target_os = "linux")]
            {
                Some(handle_try_acquire_cpu_readback(
                    sandbox,
                    registry,
                    rid,
                    &surface_id,
                    mode,
                ))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (surface_id, mode);
                Some(EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: "try_acquire_cpu_readback is only available on Linux".to_string(),
                }))
            }
        }
        EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
            request_id: _,
            handle_id,
        }) => {
            let removed = registry.remove_handle_typed(&handle_id);
            Some(match removed {
                RemovedHandle::NotFound => EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: format!("handle_id '{handle_id}' not found in registry"),
                }),
                RemovedHandle::Generic => {
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
                        cpu_readback_planes: None,
                    })
                }
                #[cfg(target_os = "linux")]
                RemovedHandle::CpuReadback => {
                    // `CpuReadbackHandle::drop` already released the
                    // per-plane surface-share registrations and ran the
                    // adapter's release-side work. Nothing else to do.
                    EscalateResponse::Ok(EscalateResponseOk {
                        request_id: rid,
                        handle_id,
                        width: None,
                        height: None,
                        format: None,
                        usage: None,
                        cpu_readback_planes: None,
                    })
                }
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
            store.register_texture(&handle_id, texture.texture(), None)?;
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
        store.register_texture(&handle_id, texture, None)?;
    }
    Ok(handle_id)
}

/// Map a wire-format `acquire_cpu_readback` request through the
/// registered [`CpuReadbackBridge`] and surface the per-plane staging
/// buffers via the host's surface-share service.
///
/// Failure modes (each surfaced as an [`EscalateResponse::Err`] keyed
/// by the original request_id):
///
/// 1. `surface_id` doesn't parse as a `u64` — wire format is decimal.
/// 2. No bridge is registered — the host runtime didn't wire a
///    cpu-readback adapter into [`crate::core::context::GpuContext::set_cpu_readback_bridge`].
/// 3. Bridge `acquire` returned an error — typically "surface not
///    registered" or a Vulkan submit failure inside the adapter.
/// 4. Surface-share service is not initialized on the host.
/// 5. `check_in` for any plane staging buffer failed — fully unwinds
///    any prior plane registrations and drops the adapter guard before
///    returning so the host doesn't leak DMA-BUF FDs.
#[cfg(target_os = "linux")]
fn handle_acquire_cpu_readback(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    rid: String,
    surface_id_str: &str,
    mode: EscalateRequestAcquireCpuReadbackMode,
) -> EscalateResponse {
    let bridge_mode = match mode {
        EscalateRequestAcquireCpuReadbackMode::Read => CpuReadbackAccessMode::Read,
        EscalateRequestAcquireCpuReadbackMode::Write => CpuReadbackAccessMode::Write,
    };
    dispatch_cpu_readback(
        sandbox,
        registry,
        rid,
        surface_id_str,
        "acquire_cpu_readback",
        |bridge, surface_id| match bridge.acquire(surface_id, bridge_mode) {
            Ok(a) => Ok(Some(a)),
            Err(msg) => Err(msg),
        },
    )
}

/// Map a wire-format `try_acquire_cpu_readback` request through the
/// registered [`CpuReadbackBridge`]. Behaviour matches
/// [`handle_acquire_cpu_readback`] on success and on hard error
/// (parse / no bridge / bridge error / surface-share check_in fail), but
/// surfaces an [`EscalateResponse::Contended`] response when the bridge
/// reports `Ok(None)` — i.e. a competing reader/writer is already
/// holding the surface. Contention does NOT register any surface-share
/// entries and does NOT insert a registry handle, so the subprocess has
/// nothing to release on its end.
#[cfg(target_os = "linux")]
fn handle_try_acquire_cpu_readback(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    rid: String,
    surface_id_str: &str,
    mode: EscalateRequestTryAcquireCpuReadbackMode,
) -> EscalateResponse {
    let bridge_mode = match mode {
        EscalateRequestTryAcquireCpuReadbackMode::Read => CpuReadbackAccessMode::Read,
        EscalateRequestTryAcquireCpuReadbackMode::Write => CpuReadbackAccessMode::Write,
    };
    dispatch_cpu_readback(
        sandbox,
        registry,
        rid,
        surface_id_str,
        "try_acquire_cpu_readback",
        |bridge, surface_id| bridge.try_acquire(surface_id, bridge_mode),
    )
}

/// Shared dispatch path for blocking and non-blocking cpu-readback
/// acquires. `op_label` is the wire op name used in error messages so
/// failures stay traceable to the request the customer issued.
/// `bridge_call` returns:
///   - `Ok(Some(_))` → produce an [`EscalateResponse::Ok`] with planes;
///   - `Ok(None)`    → produce an [`EscalateResponse::Contended`];
///   - `Err(msg)`    → produce an [`EscalateResponse::Err`].
#[cfg(target_os = "linux")]
fn dispatch_cpu_readback<F>(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    rid: String,
    surface_id_str: &str,
    op_label: &str,
    bridge_call: F,
) -> EscalateResponse
where
    F: FnOnce(
        &dyn CpuReadbackBridge,
        streamlib_adapter_abi::SurfaceId,
    ) -> std::result::Result<Option<CpuReadbackAcquired>, String>,
{
    use std::sync::Arc;

    use crate::core::rhi::{RhiPixelBuffer, RhiPixelBufferRef};

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

    let acquired = match bridge_call(bridge.as_ref(), surface_id) {
        Ok(Some(a)) => a,
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

    let surface_store = match sandbox.surface_store() {
        Some(s) => s,
        None => {
            return EscalateResponse::Err(EscalateResponseErr {
                request_id: rid,
                message: format!(
                    "{op_label}: surface-share service not initialized on host"
                ),
            });
        }
    };

    let mut planes_wire =
        Vec::<EscalateResponseOkCpuReadbackPlane>::with_capacity(acquired.planes.len());
    let mut registered_ids: Vec<String> = Vec::with_capacity(acquired.planes.len());

    for plane in &acquired.planes {
        // Wrap the staging HostVulkanPixelBuffer in an RhiPixelBuffer so
        // surface-share's check_in (which exports per-plane DMA-BUF FDs)
        // can register it. The Arc keeps the staging buffer alive for
        // the lifetime of the surface-share entry.
        let pixel_buffer = RhiPixelBuffer::new(RhiPixelBufferRef {
            inner: Arc::clone(&plane.staging),
        });
        match surface_store.check_in(&pixel_buffer) {
            Ok(plane_surface_id) => {
                planes_wire.push(EscalateResponseOkCpuReadbackPlane {
                    staging_surface_id: plane_surface_id.clone(),
                    width: plane.width,
                    height: plane.height,
                    bytes_per_pixel: plane.bytes_per_pixel,
                });
                registered_ids.push(plane_surface_id);
            }
            Err(e) => {
                // Unwind: release any prior plane registrations and
                // drop the adapter guard before returning so the host
                // doesn't end up holding the surface against the next
                // acquire from the same subprocess.
                for id in &registered_ids {
                    let _ = surface_store.release(id);
                }
                drop(acquired);
                return EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: format!(
                        "{op_label}: surface-share check_in failed for plane: {e}"
                    ),
                });
            }
        }
    }

    let handle_id = uuid::Uuid::new_v4().to_string();
    let CpuReadbackAcquired {
        width,
        height,
        format,
        planes: _,
        guard,
    } = acquired;
    registry.insert_cpu_readback(
        handle_id.clone(),
        CpuReadbackHandle {
            guard: Some(guard),
            staging_surface_ids: registered_ids,
            surface_store,
        },
    );

    EscalateResponse::Ok(EscalateResponseOk {
        request_id: rid,
        handle_id,
        width: Some(width),
        height: Some(height),
        format: Some(surface_format_to_wire(format).to_string()),
        usage: None,
        cpu_readback_planes: Some(planes_wire),
    })
}

/// Wire-format name for a [`SurfaceFormat`], matching the lowercase
/// snake-case convention used by the rest of the escalate schema.
#[cfg(target_os = "linux")]
fn surface_format_to_wire(fmt: streamlib_adapter_abi::SurfaceFormat) -> &'static str {
    match fmt {
        streamlib_adapter_abi::SurfaceFormat::Bgra8 => "bgra8",
        streamlib_adapter_abi::SurfaceFormat::Rgba8 => "rgba8",
        streamlib_adapter_abi::SurfaceFormat::Nv12 => "nv12",
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
            cpu_readback_planes: None,
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

    /// Wire-format names for `SurfaceFormat`. Locks the contract the
    /// Python and Deno cpu-readback runtimes parse against.
    #[cfg(target_os = "linux")]
    #[test]
    fn surface_format_to_wire_uses_lowercase_snake_case() {
        use streamlib_adapter_abi::SurfaceFormat;
        assert_eq!(super::surface_format_to_wire(SurfaceFormat::Bgra8), "bgra8");
        assert_eq!(super::surface_format_to_wire(SurfaceFormat::Rgba8), "rgba8");
        assert_eq!(super::surface_format_to_wire(SurfaceFormat::Nv12), "nv12");
    }

    /// `AcquireCpuReadback` with no bridge registered must surface a
    /// clean error response (not a panic) so the subprocess can
    /// translate it into a Python/Deno exception. Gated on the GPU-
    /// init path because constructing a `GpuContextLimitedAccess`
    /// without a real device is not supported on this platform.
    #[cfg(target_os = "linux")]
    #[test]
    fn acquire_cpu_readback_without_bridge_returns_err() {
        use crate::core::context::{GpuContext, GpuContextLimitedAccess};

        let gpu = match GpuContext::init_for_platform_sync() {
            Ok(g) => g,
            Err(_) => {
                println!(
                    "acquire_cpu_readback_without_bridge_returns_err: no GPU device — skipping"
                );
                return;
            }
        };
        let sandbox = GpuContextLimitedAccess::new(gpu);
        let registry = EscalateHandleRegistry::new();

        let req = EscalateRequest::AcquireCpuReadback(EscalateRequestAcquireCpuReadback {
            request_id: "req-cpu-1".to_string(),
            surface_id: "42".to_string(),
            mode: EscalateRequestAcquireCpuReadbackMode::Read,
        });
        let response = handle_escalate_op(&sandbox, &registry, req)
            .expect("acquire_cpu_readback always produces a response");
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
                panic!("acquire_cpu_readback must fail when no bridge is registered")
            }
            EscalateResponse::Contended(_) => {
                panic!("blocking acquire_cpu_readback must never return Contended")
            }
        }
        assert_eq!(
            registry.handle_count(),
            0,
            "no handle should be registered on the failure path"
        );
    }

    /// `TryAcquireCpuReadback` parse / no-bridge / contended dispatch
    /// path. Mirrors the blocking-variant tests above, plus a stub
    /// bridge that returns `Ok(None)` to exercise the new
    /// [`EscalateResponse::Contended`] arm without requiring a real
    /// host-side surface registration.
    #[cfg(target_os = "linux")]
    mod try_acquire_dispatch {
        use super::super::*;
        use super::EscalateHandleRegistry;
        use std::sync::Arc;

        use crate::core::context::{
            CpuReadbackAccessMode, CpuReadbackAcquired, CpuReadbackBridge, GpuContext,
            GpuContextLimitedAccess,
        };
        use streamlib_adapter_abi::SurfaceId;

        struct AlwaysContendedBridge;
        impl CpuReadbackBridge for AlwaysContendedBridge {
            fn acquire(
                &self,
                _surface_id: SurfaceId,
                _mode: CpuReadbackAccessMode,
            ) -> std::result::Result<CpuReadbackAcquired, String> {
                Err(
                    "AlwaysContendedBridge does not implement blocking acquire"
                        .to_string(),
                )
            }
            fn try_acquire(
                &self,
                _surface_id: SurfaceId,
                _mode: CpuReadbackAccessMode,
            ) -> std::result::Result<Option<CpuReadbackAcquired>, String> {
                Ok(None)
            }
        }

        struct AlwaysErrBridge;
        impl CpuReadbackBridge for AlwaysErrBridge {
            fn acquire(
                &self,
                _surface_id: SurfaceId,
                _mode: CpuReadbackAccessMode,
            ) -> std::result::Result<CpuReadbackAcquired, String> {
                Err("blocking path not exercised in this test".into())
            }
            fn try_acquire(
                &self,
                _surface_id: SurfaceId,
                _mode: CpuReadbackAccessMode,
            ) -> std::result::Result<Option<CpuReadbackAcquired>, String> {
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

        /// Bridge `Ok(None)` → `EscalateResponse::Contended`. Registry
        /// gains no handle, request_id round-trips.
        #[test]
        fn contended_response_when_bridge_returns_none() {
            let Some(sandbox) = make_sandbox_with_bridge(Some(Arc::new(AlwaysContendedBridge))) else {
                println!("contended_response_when_bridge_returns_none: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();

            let req = EscalateRequest::TryAcquireCpuReadback(
                EscalateRequestTryAcquireCpuReadback {
                    request_id: "req-try-contended".into(),
                    surface_id: "1".into(),
                    mode: EscalateRequestTryAcquireCpuReadbackMode::Write,
                },
            );
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("try_acquire_cpu_readback always produces a response");
            match response {
                EscalateResponse::Contended(c) => {
                    assert_eq!(c.request_id, "req-try-contended");
                }
                other => panic!(
                    "expected Contended response, got {other:?}"
                ),
            }
            assert_eq!(
                registry.handle_count(),
                0,
                "contended response must not register any host-side handle"
            );
        }

        /// Bridge `Err(_)` → `EscalateResponse::Err`, NOT
        /// `Contended`. Hard adapter failures must remain
        /// distinguishable from contention.
        #[test]
        fn err_response_when_bridge_returns_err() {
            let Some(sandbox) = make_sandbox_with_bridge(Some(Arc::new(AlwaysErrBridge))) else {
                println!("err_response_when_bridge_returns_err: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();

            let req = EscalateRequest::TryAcquireCpuReadback(
                EscalateRequestTryAcquireCpuReadback {
                    request_id: "req-try-err".into(),
                    surface_id: "1".into(),
                    mode: EscalateRequestTryAcquireCpuReadbackMode::Read,
                },
            );
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("try_acquire_cpu_readback always produces a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-try-err");
                    assert!(
                        err.message.contains("synthetic adapter failure"),
                        "expected synthetic-failure message, got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err response, got {other:?}"),
            }
            assert_eq!(registry.handle_count(), 0);
        }

        /// `try_acquire_cpu_readback` with no bridge installed surfaces
        /// the same Configuration error shape as the blocking variant.
        #[test]
        fn err_when_no_bridge_registered() {
            let Some(sandbox) = make_sandbox_with_bridge(None) else {
                println!("err_when_no_bridge_registered: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();

            let req = EscalateRequest::TryAcquireCpuReadback(
                EscalateRequestTryAcquireCpuReadback {
                    request_id: "req-try-no-bridge".into(),
                    surface_id: "1".into(),
                    mode: EscalateRequestTryAcquireCpuReadbackMode::Read,
                },
            );
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("try_acquire_cpu_readback always produces a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-try-no-bridge");
                    assert!(
                        err.message.contains("CpuReadbackBridge"),
                        "expected bridge-missing message, got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err response, got {other:?}"),
            }
        }

        /// `try_acquire_cpu_readback` with a malformed `surface_id`
        /// must report a parse error keyed by the original request_id,
        /// without ever hitting the bridge.
        #[test]
        fn err_when_surface_id_malformed() {
            let Some(sandbox) = make_sandbox_with_bridge(Some(Arc::new(AlwaysContendedBridge))) else {
                println!("err_when_surface_id_malformed: no GPU — skipping");
                return;
            };
            let registry = EscalateHandleRegistry::new();

            let req = EscalateRequest::TryAcquireCpuReadback(
                EscalateRequestTryAcquireCpuReadback {
                    request_id: "req-try-bad-id".into(),
                    surface_id: "abc".into(),
                    mode: EscalateRequestTryAcquireCpuReadbackMode::Read,
                },
            );
            let response = handle_escalate_op(&sandbox, &registry, req)
                .expect("try_acquire_cpu_readback always produces a response");
            match response {
                EscalateResponse::Err(err) => {
                    assert_eq!(err.request_id, "req-try-bad-id");
                    assert!(
                        err.message.contains("not a u64") || err.message.contains("invalid"),
                        "expected parse-error, got: {}",
                        err.message
                    );
                }
                other => panic!("expected Err response, got {other:?}"),
            }
        }
    }

    /// `AcquireCpuReadback` with malformed `surface_id` must report a
    /// parse error keyed by the original request_id.
    #[cfg(target_os = "linux")]
    #[test]
    fn acquire_cpu_readback_malformed_surface_id_returns_err() {
        use crate::core::context::{GpuContext, GpuContextLimitedAccess};

        let gpu = match GpuContext::init_for_platform_sync() {
            Ok(g) => g,
            Err(_) => {
                println!(
                    "acquire_cpu_readback_malformed_surface_id_returns_err: no GPU — skipping"
                );
                return;
            }
        };
        let sandbox = GpuContextLimitedAccess::new(gpu);
        let registry = EscalateHandleRegistry::new();

        let req = EscalateRequest::AcquireCpuReadback(EscalateRequestAcquireCpuReadback {
            request_id: "req-cpu-bad".to_string(),
            surface_id: "not-a-u64".to_string(),
            mode: EscalateRequestAcquireCpuReadbackMode::Write,
        });
        let response = handle_escalate_op(&sandbox, &registry, req)
            .expect("acquire_cpu_readback must produce a response");
        match response {
            EscalateResponse::Err(err) => {
                assert_eq!(err.request_id, "req-cpu-bad");
                assert!(
                    err.message.contains("not a u64") || err.message.contains("invalid"),
                    "expected parse-error message, got: {}",
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

        const HELPER_PREAMBLE: &str = r#"
import sys
from streamlib import log
from streamlib.escalate import EscalateChannel
channel = EscalateChannel(sys.stdin.buffer, sys.stdout.buffer)
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
