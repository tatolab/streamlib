// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Polyglot escalate-on-behalf IPC for Python and Deno subprocess host
//! processors. The subprocess can only see a `GpuContextLimitedAccess`
//! sandbox; when it needs the privileged GPU surface it sends an
//! [`EscalateRequest`] to the host over its stdout, the host executes the
//! operation inside [`GpuContextLimitedAccess::escalate`], and replies with
//! an [`EscalateResponse`] on the subprocess's stdin.
//!
//! Wire format is the existing length-prefixed JSON stdio bridge used for
//! lifecycle commands (see `SubprocessBridge`). Requests and responses are
//! discriminated by `op` and `result` fields respectively; see the
//! `schemas/com.streamlib.escalate_{request,response}@1.0.0.yaml` design
//! documents for the JTD source of truth.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::core::context::GpuContextLimitedAccess;
use crate::core::rhi::{PixelFormat, RhiPixelBuffer};

#[cfg(test)]
use crate::core::error::{Result, StreamError};

/// Wire tag marking a message as an escalate request. Bridges demux on this
/// before falling through to lifecycle dispatch.
pub(crate) const ESCALATE_REQUEST_RPC: &str = "escalate_request";

/// Wire tag for responses written back to the subprocess.
pub(crate) const ESCALATE_RESPONSE_RPC: &str = "escalate_response";

/// Escalate request shape: `{ rpc: "escalate_request", op: "…", request_id, … }`.
///
/// Mirrors `com.streamlib.escalate_request@1.0.0.yaml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum EscalateOp {
    AcquirePixelBuffer {
        request_id: String,
        width: u32,
        height: u32,
        format: String,
    },
    ReleaseHandle {
        request_id: String,
        handle_id: String,
    },
}

impl EscalateOp {
    pub(crate) fn request_id(&self) -> &str {
        match self {
            EscalateOp::AcquirePixelBuffer { request_id, .. } => request_id,
            EscalateOp::ReleaseHandle { request_id, .. } => request_id,
        }
    }
}

/// Escalate response shape written back to the subprocess.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub(crate) enum EscalateResult {
    Ok {
        request_id: String,
        handle_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        format: Option<String>,
    },
    Err {
        request_id: String,
        message: String,
    },
}

/// Tracks resources acquired on behalf of a subprocess so `release_handle` —
/// or subprocess death — can drop the host's strong reference. Buffers stay
/// alive for the duration of the host pool; this map simply prevents the
/// buffer from being immediately recycled while the subprocess still references
/// it by ID.
#[derive(Default)]
pub(crate) struct EscalateHandleRegistry {
    buffers: Mutex<HashMap<String, RhiPixelBuffer>>,
}

impl EscalateHandleRegistry {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn insert_buffer(&self, handle_id: String, buffer: RhiPixelBuffer) {
        let mut map = self.buffers.lock().expect("poisoned");
        map.insert(handle_id, buffer);
    }

    pub(crate) fn remove_buffer(&self, handle_id: &str) -> bool {
        let mut map = self.buffers.lock().expect("poisoned");
        map.remove(handle_id).is_some()
    }

    pub(crate) fn clear(&self) {
        let mut map = self.buffers.lock().expect("poisoned");
        map.clear();
    }

    /// Number of currently-held handles; visible for tests.
    #[cfg(test)]
    pub(crate) fn buffer_count(&self) -> usize {
        self.buffers.lock().expect("poisoned").len()
    }
}

/// Dispatch an [`EscalateOp`] against `sandbox` and produce a response payload.
///
/// Never panics — errors inside `escalate()` become [`EscalateResult::Err`]
/// with the original request_id preserved so the subprocess can correlate.
pub(crate) fn handle_escalate_op(
    sandbox: &GpuContextLimitedAccess,
    registry: &EscalateHandleRegistry,
    op: EscalateOp,
) -> EscalateResult {
    let request_id = op.request_id().to_string();
    match op {
        EscalateOp::AcquirePixelBuffer {
            request_id: _,
            width,
            height,
            format,
        } => match parse_pixel_format(&format) {
            Ok(parsed) => {
                match sandbox.escalate(|full| full.acquire_pixel_buffer(width, height, parsed)) {
                    Ok((pool_id, buffer)) => {
                        let handle_id = pool_id.as_str().to_string();
                        registry.insert_buffer(handle_id.clone(), buffer);
                        EscalateResult::Ok {
                            request_id,
                            handle_id,
                            width: Some(width),
                            height: Some(height),
                            format: Some(pixel_format_to_wire(parsed).to_string()),
                        }
                    }
                    Err(e) => EscalateResult::Err {
                        request_id,
                        message: format!("acquire_pixel_buffer failed: {e}"),
                    },
                }
            }
            Err(e) => EscalateResult::Err {
                request_id,
                message: e,
            },
        },
        EscalateOp::ReleaseHandle {
            request_id: _,
            handle_id,
        } => {
            let removed = registry.remove_buffer(&handle_id);
            if removed {
                EscalateResult::Ok {
                    request_id,
                    handle_id,
                    width: None,
                    height: None,
                    format: None,
                }
            } else {
                EscalateResult::Err {
                    request_id,
                    message: format!("handle_id '{handle_id}' not found in registry"),
                }
            }
        }
    }
}

/// Wrap a [`EscalateResult`] in the outer `{ rpc, payload… }` envelope the
/// bridge reader writes to the subprocess stdin.
pub(crate) fn envelope_response(result: EscalateResult) -> serde_json::Value {
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
/// The wire format uses lowercase snake-case names (`bgra32`, `nv12_video_range`,
/// etc.) so Python / Deno callers don't have to know FourCC codes. Also
/// accepts the mnemonic `"bgra"` for [`PixelFormat::Bgra32`], matching the
/// existing `NativeGpu.acquire_surface(format="bgra")` default on the Python
/// side.
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

/// Try to parse an incoming bridge message as an [`EscalateOp`]. Returns
/// `None` when the message isn't an escalate request (lifecycle traffic).
/// Returns `Some(Err(...))` when the message was tagged as an escalate
/// request but the payload couldn't be decoded — the bridge still replies
/// with an `Err` response keyed by `request_id` if possible.
pub(crate) fn try_parse_escalate_request(
    value: &serde_json::Value,
) -> Option<std::result::Result<EscalateOp, EscalateParseError>> {
    let rpc = value.get("rpc").and_then(|v| v.as_str())?;
    if rpc != ESCALATE_REQUEST_RPC {
        return None;
    }
    let request_id = value
        .get("request_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    match serde_json::from_value::<EscalateOp>(value.clone()) {
        Ok(op) => Some(Ok(op)),
        Err(e) => Some(Err(EscalateParseError {
            request_id,
            message: format!("failed to decode escalate_request: {e}"),
        })),
    }
}

/// Error detail for a malformed escalate request. The bridge converts this
/// into an [`EscalateResult::Err`] response so the subprocess doesn't block
/// forever waiting on a correlated response.
pub(crate) struct EscalateParseError {
    pub(crate) request_id: Option<String>,
    pub(crate) message: String,
}

impl EscalateParseError {
    pub(crate) fn into_response(self) -> EscalateResult {
        EscalateResult::Err {
            request_id: self.request_id.unwrap_or_default(),
            message: self.message,
        }
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
pub(crate) fn parse_op_for_tests(value: &serde_json::Value) -> Result<EscalateOp> {
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
            EscalateOp::AcquirePixelBuffer {
                request_id,
                width,
                height,
                format,
            } => {
                assert_eq!(request_id, "r-1");
                assert_eq!(width, 640);
                assert_eq!(height, 480);
                assert_eq!(format, "bgra");
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
            EscalateOp::ReleaseHandle {
                request_id,
                handle_id,
            } => {
                assert_eq!(request_id, "r-2");
                assert_eq!(handle_id, "h-abc");
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
        let resp = EscalateResult::Ok {
            request_id: "r-1".into(),
            handle_id: "h-1".into(),
            width: Some(16),
            height: Some(16),
            format: Some("bgra32".into()),
        };
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
        assert_eq!(registry.buffer_count(), 0);
        assert!(!registry.remove_buffer("missing"));
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

        // Acquire a pixel buffer via escalate — registers a handle.
        let acquire = EscalateOp::AcquirePixelBuffer {
            request_id: "req-1".to_string(),
            width: 320,
            height: 240,
            format: "bgra".to_string(),
        };
        let response = handle_escalate_op(&sandbox, &registry, acquire);
        let handle_id = match response {
            EscalateResult::Ok {
                ref request_id,
                ref handle_id,
                width,
                height,
                ref format,
            } => {
                assert_eq!(request_id, "req-1");
                assert_eq!(width, Some(320));
                assert_eq!(height, Some(240));
                assert_eq!(format.as_deref(), Some("bgra32"));
                assert!(!handle_id.is_empty(), "handle id should not be empty");
                handle_id.clone()
            }
            EscalateResult::Err { message, .. } => {
                panic!("acquire_pixel_buffer escalate failed: {message}");
            }
        };
        assert_eq!(registry.buffer_count(), 1);

        // Release the handle → registry drains.
        let release = EscalateOp::ReleaseHandle {
            request_id: "req-2".to_string(),
            handle_id: handle_id.clone(),
        };
        let response = handle_escalate_op(&sandbox, &registry, release);
        match response {
            EscalateResult::Ok {
                request_id,
                handle_id: echoed,
                ..
            } => {
                assert_eq!(request_id, "req-2");
                assert_eq!(echoed, handle_id);
            }
            EscalateResult::Err { message, .. } => panic!("release_handle failed: {message}"),
        }
        assert_eq!(registry.buffer_count(), 0);

        // Releasing an unknown handle is an Err response, not a panic.
        let release_unknown = EscalateOp::ReleaseHandle {
            request_id: "req-3".to_string(),
            handle_id: "never-existed".to_string(),
        };
        match handle_escalate_op(&sandbox, &registry, release_unknown) {
            EscalateResult::Err {
                request_id,
                message,
            } => {
                assert_eq!(request_id, "req-3");
                assert!(message.contains("not found"));
            }
            EscalateResult::Ok { .. } => panic!("unknown handle should not succeed"),
        }
    }
}
