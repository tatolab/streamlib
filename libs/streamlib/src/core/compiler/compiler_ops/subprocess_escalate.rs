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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use crate::_generated_::com_streamlib_escalate_request::{
    EscalateRequestAcquirePixelBuffer, EscalateRequestAcquireTexture,
    EscalateRequestReleaseHandle,
};
use crate::_generated_::com_streamlib_escalate_response::{
    EscalateResponseErr, EscalateResponseOk,
};
use crate::_generated_::{EscalateRequest, EscalateResponse};
use crate::core::context::{PooledTextureHandle, TexturePoolDescriptor};
use crate::core::context::GpuContextLimitedAccess;
use crate::core::rhi::{PixelFormat, RhiPixelBuffer, TextureFormat, TextureUsages};

#[cfg(test)]
use crate::core::error::{Result, StreamError};

/// Wire tag marking a message as an escalate request. Bridges demux on this
/// before falling through to lifecycle dispatch.
pub(crate) const ESCALATE_REQUEST_RPC: &str = "escalate_request";

/// Wire tag for responses written back to the subprocess.
pub(crate) const ESCALATE_RESPONSE_RPC: &str = "escalate_response";

fn request_id(op: &EscalateRequest) -> &str {
    match op {
        EscalateRequest::AcquirePixelBuffer(p) => &p.request_id,
        EscalateRequest::AcquireTexture(p) => &p.request_id,
        EscalateRequest::ReleaseHandle(p) => &p.request_id,
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

/// Dispatch an [`EscalateRequest`] against `sandbox` and produce a response.
///
/// Never panics — errors inside `escalate()` become [`EscalateResponse::Err`]
/// with the original request_id preserved so the subprocess can correlate.
pub(crate) fn handle_escalate_op(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    op: EscalateRequest,
) -> EscalateResponse {
    let rid = request_id(&op).to_string();
    match op {
        EscalateRequest::AcquirePixelBuffer(EscalateRequestAcquirePixelBuffer {
            request_id: _,
            width,
            height,
            format,
        }) => match parse_pixel_format(&format) {
            Ok(parsed) => {
                match sandbox.escalate(|full| full.acquire_pixel_buffer(width, height, parsed)) {
                    Ok((pool_id, buffer)) => {
                        let handle_id = pool_id.as_str().to_string();
                        registry.insert_buffer(handle_id.clone(), buffer);
                        EscalateResponse::Ok(EscalateResponseOk {
                            request_id: rid,
                            handle_id,
                            width: Some(width),
                            height: Some(height),
                            format: Some(pixel_format_to_wire(parsed).to_string()),
                            usage: None,
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
        },
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
                    return EscalateResponse::Err(EscalateResponseErr {
                        request_id: rid,
                        message: e,
                    });
                }
            };
            let parsed_usage = match parse_texture_usages(&usage) {
                Ok(u) => u,
                Err(e) => {
                    return EscalateResponse::Err(EscalateResponseErr {
                        request_id: rid,
                        message: e,
                    });
                }
            };
            let desc = TexturePoolDescriptor::new(width, height, parsed_format)
                .with_usage(parsed_usage);
            match sandbox.escalate(|full| full.acquire_texture(&desc)) {
                Ok(texture) => {
                    let handle_id = Uuid::new_v4().to_string();
                    registry.insert_texture(handle_id.clone(), texture);
                    EscalateResponse::Ok(EscalateResponseOk {
                        request_id: rid,
                        handle_id,
                        width: Some(width),
                        height: Some(height),
                        format: Some(texture_format_to_wire(parsed_format).to_string()),
                        usage: Some(texture_usages_to_wire(parsed_usage)),
                    })
                }
                Err(e) => EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: format!("acquire_texture failed: {e}"),
                }),
            }
        }
        EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
            request_id: _,
            handle_id,
        }) => {
            let removed = registry.remove_handle(&handle_id);
            if removed {
                EscalateResponse::Ok(EscalateResponseOk {
                    request_id: rid,
                    handle_id,
                    width: None,
                    height: None,
                    format: None,
                    usage: None,
                })
            } else {
                EscalateResponse::Err(EscalateResponseErr {
                    request_id: rid,
                    message: format!("handle_id '{handle_id}' not found in registry"),
                })
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
        Ok(op) => handle_escalate_op(sandbox, registry, op),
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
    fn envelope_response_tags_rpc() {
        let resp = EscalateResponse::Ok(EscalateResponseOk {
            request_id: "r-1".into(),
            handle_id: "h-1".into(),
            width: Some(16),
            height: Some(16),
            format: Some("bgra32".into()),
            usage: None,
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
        let response = handle_escalate_op(&sandbox, &registry, acquire);
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
        let response = handle_escalate_op(&sandbox, &registry, acquire_tex);
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
        };
        assert_eq!(registry.handle_count(), 2);

        let release_tex = EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
            request_id: "req-tex-rel".to_string(),
            handle_id: texture_handle_id.clone(),
        });
        match handle_escalate_op(&sandbox, &registry, release_tex) {
            EscalateResponse::Ok(ok) => {
                assert_eq!(ok.request_id, "req-tex-rel");
                assert_eq!(ok.handle_id, texture_handle_id);
            }
            EscalateResponse::Err(err) => panic!("release_handle (texture) failed: {}", err.message),
        }
        assert_eq!(registry.handle_count(), 1);

        let release = EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
            request_id: "req-2".to_string(),
            handle_id: buffer_handle_id.clone(),
        });
        let response = handle_escalate_op(&sandbox, &registry, release);
        match response {
            EscalateResponse::Ok(ok) => {
                assert_eq!(ok.request_id, "req-2");
                assert_eq!(ok.handle_id, buffer_handle_id);
            }
            EscalateResponse::Err(err) => panic!("release_handle failed: {}", err.message),
        }
        assert_eq!(registry.handle_count(), 0);

        let release_unknown =
            EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
                request_id: "req-3".to_string(),
                handle_id: "never-existed".to_string(),
            });
        match handle_escalate_op(&sandbox, &registry, release_unknown) {
            EscalateResponse::Err(err) => {
                assert_eq!(err.request_id, "req-3");
                assert!(err.message.contains("not found"));
            }
            EscalateResponse::Ok(_) => panic!("unknown handle should not succeed"),
        }
    }
}
