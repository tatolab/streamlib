// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "macos")]

//! An encoder block configured with a knob the VideoToolbox arm has no
//! property for never reaches Running: `setup()` refuses it by name, before a
//! frame arrives — the session mint on the first frame would latch the same
//! refusal silently, which is the path this rules out.
//!
//! Rig tier: `App::new()` brings up a real `GpuContext`.

use std::sync::Once;
use std::time::Duration;

use streamlib::sdk::App;
use streamlib::sdk::descriptors::ProcessorClassImportPath;
use streamlib_media_builtins::{
    H264Encoder, H265Encoder, TestPatternSource, register_media_builtin_processor_types,
};

/// Setup is refused before any GPU work, so this bounds only a stalled graph.
const READINESS_TIMEOUT: Duration = Duration::from_secs(20);

fn ensure_the_media_builtins_are_registered() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(register_media_builtin_processor_types);
}

/// The refusal readiness reports for an encoder added with `encoder_config`.
fn readiness_refusal_of_an_encoder_configured_with(
    encoder_class_import_path: ProcessorClassImportPath,
    encoder_config: serde_json::Value,
) -> String {
    ensure_the_media_builtins_are_registered();
    let app = App::new().expect("a runtime");
    let source = app
        .add(
            TestPatternSource::Processor::processor_class_import_path(),
            serde_json::json!({ "width": 320, "height": 180 }),
            Some("pattern"),
        )
        .expect("the test pattern");
    let encoder = app
        .add(encoder_class_import_path, encoder_config, Some("encoder"))
        .expect("the encoder is added; its config is judged at setup");
    app.connect((&source, "video"), (&encoder, "video"))
        .expect("the pattern to the encoder");
    app.runner().start().expect("the graph starts");
    let readiness = app
        .runner()
        .wait_until_every_processor_is_running(READINESS_TIMEOUT);
    let _ = app.runner().stop();
    readiness
        .expect_err("an encoder with a knob VideoToolbox does not honour must not reach Running")
        .to_string()
}

#[test]
fn an_effort_level_keeps_the_h264_encoder_from_running_and_is_named() {
    let refusal = readiness_refusal_of_an_encoder_configured_with(
        H264Encoder::Processor::processor_class_import_path(),
        serde_json::json!({ "effort_level": 2 }),
    );
    assert!(refusal.contains("effort_level = 2"), "{refusal}");
}

#[test]
fn a_zero_keyframe_interval_keeps_the_h265_encoder_from_running_and_is_named() {
    let refusal = readiness_refusal_of_an_encoder_configured_with(
        H265Encoder::Processor::processor_class_import_path(),
        serde_json::json!({ "keyframe_interval_seconds": 0 }),
    );
    assert!(
        refusal.contains("keyframe_interval_seconds = 0"),
        "{refusal}"
    );
}
