// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan Video Encode/Decode Roundtrip Pipeline — camera-as-cdylib variant.
//!
//! Sibling of `examples/vulkan-video-roundtrip` that loads the
//! `@tatolab/camera` processor via `runtime.load_project(...)` (the
//! cdylib plugin path) instead of taking `streamlib-camera` as a
//! Rust dep. Exists to exercise the v12 + v13 + Phase E Slice A
//! (`RhiColorConverterMethodsVTable`) + Phase E Slice B
//! (`RhiCommandRecorderMethodsVTable`) cdylib dispatch end-to-end
//! through the encode → decode → display pipeline.
//!
//!   CameraProcessor (cdylib) → Encoder → Decoder → Display
//!
//! Usage:
//!   cargo run -p vulkan-video-roundtrip-cdylib-camera -- h264 [device] [seconds]
//!   cargo run -p vulkan-video-roundtrip-cdylib-camera -- h265 /dev/video0 10

use std::path::{Path, PathBuf};

use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::graph::{OutputLinkPortRef, ProcessorUniqueId};
use streamlib::sdk::processors::{input, output, ProcessorSpec};
use streamlib::sdk::runtime::{host_target_triple, Runner};
use streamlib::sdk::schema_ident;
use streamlib_display::DisplayProcessor;
use streamlib_h264::{H264DecoderProcessor, H264EncoderProcessor};
use streamlib_h265::{H265DecoderProcessor, H265EncoderProcessor};

fn copy_dir_contents(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dst_entry = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_contents(&entry.path(), &dst_entry)?;
        } else {
            std::fs::copy(entry.path(), &dst_entry)?;
        }
    }
    Ok(())
}

fn stage_camera_project(workspace_root: &Path) -> Result<PathBuf> {
    let dylib_ext = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    };
    let dylib_name = format!("libstreamlib_camera.{dylib_ext}");
    let built_dylib = workspace_root.join("target").join("debug").join(&dylib_name);
    if !built_dylib.exists() {
        return Err(Error::Configuration(format!(
            "camera cdylib not at {} — run \
             `cargo build -p streamlib-camera --features plugin` first",
            built_dylib.display()
        )));
    }

    let tmp = tempfile::tempdir()
        .map_err(|e| Error::Configuration(format!("tempdir: {e}")))?;
    let staged_root = tmp.keep();

    let camera_src = workspace_root.join("packages/camera");
    let core_src = workspace_root.join("packages/core");
    let camera_dst = staged_root.join("packages/camera");
    let core_dst = staged_root.join("packages/core");

    std::fs::create_dir_all(&camera_dst)
        .map_err(|e| Error::Configuration(format!("mkdir camera dst: {e}")))?;
    std::fs::copy(
        camera_src.join("streamlib.yaml"),
        camera_dst.join("streamlib.yaml"),
    )
    .map_err(|e| Error::Configuration(format!("copy camera streamlib.yaml: {e}")))?;
    copy_dir_contents(&camera_src.join("schemas"), &camera_dst.join("schemas"))
        .map_err(|e| Error::Configuration(format!("copy camera schemas: {e}")))?;

    std::fs::create_dir_all(&core_dst)
        .map_err(|e| Error::Configuration(format!("mkdir core dst: {e}")))?;
    std::fs::copy(
        core_src.join("streamlib.yaml"),
        core_dst.join("streamlib.yaml"),
    )
    .map_err(|e| Error::Configuration(format!("copy core streamlib.yaml: {e}")))?;
    copy_dir_contents(&core_src.join("schemas"), &core_dst.join("schemas"))
        .map_err(|e| Error::Configuration(format!("copy core schemas: {e}")))?;

    let triple_dir = camera_dst.join("lib").join(host_target_triple());
    std::fs::create_dir_all(&triple_dir)
        .map_err(|e| Error::Configuration(format!("mkdir triple dir: {e}")))?;
    std::fs::copy(&built_dylib, triple_dir.join(&dylib_name))
        .map_err(|e| Error::Configuration(format!("copy camera cdylib: {e}")))?;

    Ok(camera_dst)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let codec = args.get(1).map(|s| s.as_str()).unwrap_or("h264");
    let device = args.get(2).map(|s| s.as_str()).unwrap_or("/dev/video0");
    let duration_secs: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(30);
    let is_h265 = codec == "h265";

    println!(
        "=== Vulkan Video {} Roundtrip (cdylib camera) ===",
        codec.to_uppercase()
    );
    println!("Camera:   {device} (loaded as cdylib via runtime.load_project)");
    println!("Duration: {duration_secs}s\n");

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| {
            Error::Configuration("CARGO_MANIFEST_DIR has no two parents".into())
        })?;

    let runtime = Runner::new()?;

    // 1) Stage @tatolab/camera as a project with its cdylib placed
    //    under `lib/<target-triple>/`, then load it.
    let camera_project = stage_camera_project(workspace_root)?;
    println!("+ Camera staged at: {}", camera_project.display());
    runtime.load_project(&camera_project)?;
    println!("+ Camera project loaded (cdylib path)");

    // 2) Also load the example's own streamlib.yaml so the
    //    `@tatolab/core` wire vocabulary is registered (the
    //    vulkan-video-roundtrip baseline does this too — see its
    //    comment on the EncodedVideoFrame max-payload-bytes hookup).
    runtime.load_project(env!("CARGO_MANIFEST_DIR"))?;
    println!("+ Wire vocabulary registered\n");

    // 3) Camera processor — minted from the cdylib's registration,
    //    addressed by its structured schema_ident, configured via
    //    JSON payload (matches camera_config.yaml).
    let camera_ident = schema_ident!("tatolab", "camera", "Camera", "1.0.0");
    let camera_config = serde_json::json!({
        "device_id": device,
        "max_width": std::env::var("STREAMLIB_CAMERA_MAX_WIDTH")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(1920),
        "max_height": std::env::var("STREAMLIB_CAMERA_MAX_HEIGHT")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(1080),
    });
    let camera = runtime.add_processor(ProcessorSpec::new(camera_ident, camera_config))?;
    println!("+ Camera (cdylib): {camera}");

    // 4) Encoder / decoder / display — typed Rust deps as in the
    //    baseline vulkan-video-roundtrip.
    let effort_level: Option<u32> = std::env::var("STREAMLIB_ENCODER_EFFORT_LEVEL")
        .ok()
        .and_then(|s| s.parse().ok());

    let encoder = if is_h265 {
        runtime.add_processor(H265EncoderProcessor::node(
            H265EncoderProcessor::Config {
                effort_level,
                ..Default::default()
            },
        ))?
    } else {
        runtime.add_processor(H264EncoderProcessor::node(
            H264EncoderProcessor::Config {
                effort_level,
                ..Default::default()
            },
        ))?
    };
    println!("+ {}Encoder: {encoder}", codec.to_uppercase());

    let decoder = if is_h265 {
        runtime.add_processor(H265DecoderProcessor::node(
            H265DecoderProcessor::Config::default(),
        ))?
    } else {
        runtime.add_processor(H264DecoderProcessor::node(
            H264DecoderProcessor::Config::default(),
        ))?
    };
    println!("+ {}Decoder: {decoder}", codec.to_uppercase());

    let display = runtime.add_processor(DisplayProcessor::node(DisplayProcessor::Config {
        width: 1920,
        height: 1080,
        title: Some(format!("streamlib {} Roundtrip (cdylib camera)", codec.to_uppercase())),
        ..Default::default()
    }))?;
    println!("+ Display: {display}");

    // 5) Wire — the camera output uses the runtime-typed
    //    `OutputLinkPortRef::new(camera_id, "video")` form since the
    //    cdylib-loaded camera has no compile-time-typed
    //    `CameraProcessor::OutputLink::video` symbol in this crate.
    if is_h265 {
        runtime.connect(
            OutputLinkPortRef::new(&camera, "video"),
            input::<H265EncoderProcessor::InputLink::video_in>(&encoder),
        )?;
        runtime.connect(
            output_typed_h265_encoder(&encoder),
            input::<H265DecoderProcessor::InputLink::encoded_video_in>(&decoder),
        )?;
        runtime.connect(
            output_typed_h265_decoder(&decoder),
            input::<DisplayProcessor::InputLink::video>(&display),
        )?;
    } else {
        runtime.connect(
            OutputLinkPortRef::new(&camera, "video"),
            input::<H264EncoderProcessor::InputLink::video_in>(&encoder),
        )?;
        runtime.connect(
            output_typed_h264_encoder(&encoder),
            input::<H264DecoderProcessor::InputLink::encoded_video_in>(&decoder),
        )?;
        runtime.connect(
            output_typed_h264_decoder(&decoder),
            input::<DisplayProcessor::InputLink::video>(&display),
        )?;
    }
    println!("\nPipeline: camera(cdylib) -> encoder -> decoder -> display");

    println!("Starting pipeline for {duration_secs}s...\n");
    runtime.start()?;

    std::thread::sleep(std::time::Duration::from_secs(duration_secs as u64));

    println!("\nStopping pipeline...");
    runtime.stop()?;

    Ok(())
}

// Small typed-output helpers so the main flow reads symmetrically
// with the runtime-typed camera output.

fn output_typed_h264_encoder(
    encoder: &ProcessorUniqueId,
) -> impl Into<OutputLinkPortRef> {
    output::<H264EncoderProcessor::OutputLink::encoded_video_out>(
        encoder,
    )
}

fn output_typed_h264_decoder(
    decoder: &ProcessorUniqueId,
) -> impl Into<OutputLinkPortRef> {
    output::<H264DecoderProcessor::OutputLink::video_out>(decoder)
}

fn output_typed_h265_encoder(
    encoder: &ProcessorUniqueId,
) -> impl Into<OutputLinkPortRef> {
    output::<H265EncoderProcessor::OutputLink::encoded_video_out>(
        encoder,
    )
}

fn output_typed_h265_decoder(
    decoder: &ProcessorUniqueId,
) -> impl Into<OutputLinkPortRef> {
    output::<H265DecoderProcessor::OutputLink::video_out>(decoder)
}
