// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::handle_lifecycle::EscalateHandleRegistry;
use super::{
    ESCALATE_OP_ANSWERED_BY_NOTHING, ESCALATE_REQUEST_RPC, envelope_response, parse_op_for_tests,
    request_id, try_parse_escalate_request, *,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestAcquirePixelBuffer, EscalateRequestAcquireTexture, EscalateRequestReleaseHandle,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_response::EscalateResponseOk;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::{
    EscalateRequest, EscalateResponse,
};
use crate::core::runtime::mesh::a_mesh_link_ingress_table_carrying_nothing;

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

/// The `op` tag the bridge dispatches on its reader decodes as
/// [`EscalateRequest::Log`]. Fail-without-fix: rename the variant's serde
/// tag alone and log records queue behind GPU work again, unnoticed.
#[test]
fn log_frame_parses_as_escalate_request_log_variant() {
    let log_frame = serde_json::json!({
        "rpc": "escalate_request",
        "op": ESCALATE_OP_ANSWERED_BY_NOTHING,
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
fn handle_escalate_op_end_to_end() {
    use crate::core::context::{GpuContext, GpuContextLimitedAccess};

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
    let response = handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        acquire,
    )
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
    };
    assert_eq!(registry.handle_count(), 1);

    let acquire_tex = EscalateRequest::AcquireTexture(EscalateRequestAcquireTexture {
        request_id: "req-tex".to_string(),
        width: 256,
        height: 128,
        format: "rgba8_unorm".to_string(),
        usage: vec!["texture_binding".to_string(), "copy_src".to_string()],
    });
    let response = handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        acquire_tex,
    )
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
    };
    assert_eq!(registry.handle_count(), 2);

    let release_tex = EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
        request_id: "req-tex-rel".to_string(),
        handle_id: texture_handle_id.clone(),
    });
    match handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        release_tex,
    )
    .expect("release_handle must produce a response")
    {
        EscalateResponse::Ok(ok) => {
            assert_eq!(ok.request_id, "req-tex-rel");
            assert_eq!(ok.handle_id, texture_handle_id);
        }
        EscalateResponse::Err(err) => {
            panic!("release_handle (texture) failed: {}", err.message)
        }
    }
    assert_eq!(registry.handle_count(), 1);

    let release = EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
        request_id: "req-2".to_string(),
        handle_id: buffer_handle_id.clone(),
    });
    let response = handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        release,
    )
    .expect("release_handle must produce a response");
    match response {
        EscalateResponse::Ok(ok) => {
            assert_eq!(ok.request_id, "req-2");
            assert_eq!(ok.handle_id, buffer_handle_id);
        }
        EscalateResponse::Err(err) => panic!("release_handle failed: {}", err.message),
    }
    assert_eq!(registry.handle_count(), 0);

    let release_unknown = EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
        request_id: "req-3".to_string(),
        handle_id: "never-existed".to_string(),
    });
    match handle_escalate_op(
        &sandbox,
        &registry,
        &a_mesh_link_ingress_table_carrying_nothing(),
        release_unknown,
    )
    .expect("release_handle must produce a response")
    {
        EscalateResponse::Err(err) => {
            assert_eq!(err.request_id, "req-3");
            assert!(err.message.contains("not found"));
        }
        EscalateResponse::Ok(_) => panic!("unknown handle should not succeed"),
    }
}
