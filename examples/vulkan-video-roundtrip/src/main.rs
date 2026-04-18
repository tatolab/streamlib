// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan Video Encode/Decode Roundtrip Pipeline
//!
//! Captures from a V4L2 camera (vivid virtual device or real camera),
//! encodes via Vulkan Video hardware, decodes back, and writes the
//! decoded frames to MP4 via ffmpeg.
//!
//!   CameraProcessor → Encoder → Decoder → MP4Writer
//!
//! Usage:
//!   cargo run -p vulkan-video-roundtrip --release -- h265 [device] [seconds]
//!   cargo run -p vulkan-video-roundtrip --release -- h264 /dev/video2 10

use streamlib::{
    input, output,
    CameraProcessor,
    H264EncoderProcessor, H265EncoderProcessor,
    H264DecoderProcessor, H265DecoderProcessor,
    LinuxMp4WriterProcessor, Result, StreamRuntime,
};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let codec = args.get(1).map(|s| s.as_str()).unwrap_or("h265");
    let device = args.get(2).map(|s| s.as_str()).unwrap_or("/dev/video2");
    let duration_secs: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(10);
    let is_h265 = codec == "h265";

    let fps: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(60);
    let output_path = format!("/tmp/streamlib_live_{codec}.mp4");

    println!("=== Vulkan Video {} Roundtrip ===", codec.to_uppercase());
    println!("Camera:   {device}");
    println!("Output:   {output_path}");
    println!("FPS:      {fps}");
    println!("Duration: {duration_secs}s\n");

    let runtime = StreamRuntime::new()?;

    // --- Camera: captures from V4L2 device ---
    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: Some(device.to_string()),
        ..Default::default()
    }))?;
    println!("+ Camera: {camera}");

    // --- Encoder: Vulkan Video hardware encode ---
    let encoder = if is_h265 {
        runtime.add_processor(H265EncoderProcessor::node(
            H265EncoderProcessor::Config {
                width: Some(1920),
                height: Some(1080),
                ..Default::default()
            },
        ))?
    } else {
        runtime.add_processor(H264EncoderProcessor::node(
            H264EncoderProcessor::Config {
                width: Some(1920),
                height: Some(1080),
                ..Default::default()
            },
        ))?
    };
    println!("+ {}Encoder: {encoder}", codec.to_uppercase());

    // --- Decoder: Vulkan Video hardware decode (roundtrip verification) ---
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

    // --- MP4 Writer: takes decoded Videoframe, ffmpeg encodes + muxes ---
    let mp4_writer = runtime.add_processor(LinuxMp4WriterProcessor::node(
        LinuxMp4WriterProcessor::Config {
            output_path: output_path.clone(),
            fps,
            duration_secs: Some(duration_secs),
        },
    ))?;
    println!("+ LinuxMp4Writer: {mp4_writer}");

    // --- Wire: Camera → Encoder → Decoder → MP4Writer ---
    if is_h265 {
        runtime.connect(
            output::<CameraProcessor::OutputLink::video>(&camera),
            input::<H265EncoderProcessor::InputLink::video_in>(&encoder),
        )?;
        runtime.connect(
            output::<H265EncoderProcessor::OutputLink::encoded_video_out>(&encoder),
            input::<H265DecoderProcessor::InputLink::encoded_video_in>(&decoder),
        )?;
        runtime.connect(
            output::<H265DecoderProcessor::OutputLink::video_out>(&decoder),
            input::<LinuxMp4WriterProcessor::InputLink::video_in>(&mp4_writer),
        )?;
    } else {
        runtime.connect(
            output::<CameraProcessor::OutputLink::video>(&camera),
            input::<H264EncoderProcessor::InputLink::video_in>(&encoder),
        )?;
        runtime.connect(
            output::<H264EncoderProcessor::OutputLink::encoded_video_out>(&encoder),
            input::<H264DecoderProcessor::InputLink::encoded_video_in>(&decoder),
        )?;
        runtime.connect(
            output::<H264DecoderProcessor::OutputLink::video_out>(&decoder),
            input::<LinuxMp4WriterProcessor::InputLink::video_in>(&mp4_writer),
        )?;
    }
    println!("\nPipeline: camera -> encoder -> decoder -> mp4_writer");

    // --- Run for duration then stop ---
    println!("Starting pipeline for {duration_secs}s...\n");
    runtime.start()?;

    std::thread::sleep(std::time::Duration::from_secs(duration_secs as u64 + 2));

    println!("\nStopping pipeline...");
    runtime.stop()?;

    // Convert the raw decoded NV12 dump into MP4 using ffmpeg.
    // This is exactly what nvpro did: decoder NV12 output → ffmpeg → MP4.
    let nv12_path = "/tmp/streamlib_decoded_nv12.raw";
    let decoded_output = format!("/tmp/streamlib_decoded_{codec}.mp4");
    if std::path::Path::new(nv12_path).exists() {
        // Compute actual fps from decoded frame count and pipeline duration.
        let nv12_file_size = std::fs::metadata(nv12_path).map(|m| m.len()).unwrap_or(0);
        let frame_size = 1920u64 * 1088 * 3 / 2; // NV12 frame size
        let decoded_frame_count = if frame_size > 0 { nv12_file_size / frame_size } else { 0 };
        let actual_fps = if decoded_frame_count > 0 && duration_secs > 0 {
            (decoded_frame_count as u32) / duration_secs
        } else {
            fps
        };
        let fps_str = actual_fps.max(1).to_string();
        // Decoder outputs 1088 height (H.265 16-pixel alignment of 1080)
        let size_str = "1920x1088".to_string();
        let duration_str = duration_secs.to_string();
        let mux_status = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-f", "rawvideo",
                "-pix_fmt", "nv12",
                "-s", &size_str,
                "-r", &fps_str,
                "-i", nv12_path,
                "-f", "lavfi",
                "-t", &duration_str,
                "-i", &format!("anullsrc=r=48000:cl=stereo:d={duration_str}"),
                "-c:v", "mpeg4",
                "-q:v", "1",
                "-c:a", "aac",
                "-shortest",
                "-movflags", "+faststart",
                &decoded_output,
            ])
            .output();

        match mux_status {
            Ok(output) if output.status.success() => {
                println!("Decoded output: {decoded_output}");
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                eprintln!("ffmpeg decoded mux failed: {stderr}");
            }
            Err(e) => eprintln!("ffmpeg not available: {e}"),
        }
        // let _ = std::fs::remove_file(nv12_path);
    }

    Ok(())
}
