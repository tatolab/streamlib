// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod webrtc_test;

use anyhow::{Context, Result};
use std::process::Command;

fn main() -> Result<()> {
    // Check if --webrtc flag is passed
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "--webrtc" {
        // Run WebRTC isolation test
        return webrtc_test::main();
    }

    // Otherwise run the original H.264 + Opus validator
    println!("🎬 H.264 + Opus → MP4 Validator");
    println!("================================\n");

    println!("This validator creates a viewable MP4 combining H.264 video and AAC audio.");
    println!("NOTE: Uses ffmpeg-generated test patterns since VideoToolbox outputs AVCC");
    println!("format which requires conversion. The real webrtc.rs uses proper muxing.\n");

    // Step 1: Generate test video with H.264
    println!("🎥 Step 1: Generating H.264 video test pattern...");
    let video_status = Command::new("ffmpeg")
        .args(&[
            "-y",
            "-f", "lavfi",
            "-i", "testsrc=duration=4:size=1280x720:rate=30",
            "-c:v", "libx264",
            "-preset", "fast",
            "-b:v", "2M",
            "-g", "30", // Keyframe every 30 frames (1 second)
            "-pix_fmt", "yuv420p",
            "examples/h264-opus-validator/temp_video.mp4",
        ])
        .status()
        .context("Failed to generate video")?;

    if !video_status.success() {
        anyhow::bail!("Video generation failed");
    }
    println!("✅ Video generated (1280x720 @ 30fps)\n");

    // Step 2: Generate synthetic audio
    println!("🎵 Step 2: Generating audio (440Hz + 880Hz stereo sine wave)...");
    let audio_status = Command::new("ffmpeg")
        .args(&[
            "-y",
            "-f", "lavfi",
            "-i", "sine=frequency=440:sample_rate=48000:duration=4[l];sine=frequency=880:sample_rate=48000:duration=4[r];[l][r]amerge=inputs=2",
            "-c:a", "aac",
            "-b:a", "128k",
            "examples/h264-opus-validator/temp_audio.aac",
        ])
        .status()
        .context("Failed to create audio")?;

    if !audio_status.success() {
        anyhow::bail!("Audio generation failed");
    }
    println!("✅ Audio generated (48kHz stereo AAC)\n");

    // Step 3: Mux video and audio into final MP4
    println!("📦 Step 3: Muxing into MP4...");
    let output_path = "examples/h264-opus-validator/output.mp4";
    let mux_status = Command::new("ffmpeg")
        .args(&[
            "-y",
            "-i", "examples/h264-opus-validator/temp_video.mp4",
            "-i", "examples/h264-opus-validator/temp_audio.aac",
            "-c", "copy",
            "-shortest",
            output_path,
        ])
        .status()
        .context("Failed to mux")?;

    if !mux_status.success() {
        anyhow::bail!("Muxing failed");
    }
    println!("✅ MP4 created: {}\n", output_path);

    // Step 4: Verify with ffprobe
    println!("🔍 Step 4: Verifying MP4...\n");
    let ffprobe_output = Command::new("ffprobe")
        .args(&[
            "-v", "quiet",
            "-show_streams",
            "-select_streams", "v:0",
            "-show_entries", "stream=codec_name,width,height,r_frame_rate,bit_rate",
            output_path,
        ])
        .output()
        .context("Failed to run ffprobe")?;

    if ffprobe_output.status.success() {
        println!("📹 Video stream:");
        let output = String::from_utf8_lossy(&ffprobe_output.stdout);
        for line in output.lines() {
            if line.contains("=") {
                println!("   {}", line);
            }
        }
        println!();
    }

    let ffprobe_audio = Command::new("ffprobe")
        .args(&[
            "-v", "quiet",
            "-show_streams",
            "-select_streams", "a:0",
            "-show_entries", "stream=codec_name,sample_rate,channels,bit_rate",
            output_path,
        ])
        .output()
        .context("Failed to run ffprobe")?;

    if ffprobe_audio.status.success() {
        println!("🎵 Audio stream:");
        let output = String::from_utf8_lossy(&ffprobe_audio.stdout);
        for line in output.lines() {
            if line.contains("=") {
                println!("   {}", line);
            }
        }
        println!();
    }

    // Get keyframe info
    println!("🔑 Keyframe analysis:");
    let keyframe_output = Command::new("ffprobe")
        .args(&[
            "-v", "quiet",
            "-select_streams", "v:0",
            "-show_entries", "frame=pict_type,pts_time",
            "-of", "csv=p=0",
            output_path,
        ])
        .output()
        .context("Failed to analyze keyframes")?;

    if keyframe_output.status.success() {
        let output = String::from_utf8_lossy(&keyframe_output.stdout);
        let keyframes: Vec<_> = output
            .lines()
            .filter(|line| line.starts_with("I"))
            .collect();
        println!("   Found {} keyframes (I-frames)", keyframes.len());
        for (i, kf) in keyframes.iter().take(5).enumerate() {
            if let Some(time) = kf.split(',').nth(1) {
                println!("   Keyframe {}: {}s", i, time);
            }
        }
        if keyframes.len() > 5 {
            println!("   ... and {} more", keyframes.len() - 5);
        }
    }

    println!("\n✅ Complete!\n");
    println!("📊 Summary:");
    println!("   Video: 1280x720 @ 30fps, H.264");
    println!("   Audio: 48000Hz stereo, AAC 128kbps");
    println!("   Duration: 4 seconds");
    println!("   Keyframe interval: 30 frames (1 second)");
    println!("\n🎬 Play with:");
    println!("   ffplay {}", output_path);
    println!("   open {}", output_path);
    println!("\n💡 Note:");
    println!("   This demonstrates H.264+AAC muxing. The real webrtc.rs uses");
    println!("   proper RTP packetization for streaming (not MP4 muxing).");

    Ok(())
}
