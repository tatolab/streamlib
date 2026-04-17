// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Video demo: encode NV12 fixture with SimpleEncoder, decode with SimpleDecoder,
//! produce MP4 files for visual verification.

use vulkan_video::{SimpleEncoder, SimpleEncoderConfig, Codec, Preset};
use vulkan_video::{SimpleDecoder, SimpleDecoderConfig};

fn main() {
    let width: u32 = 640;
    let height: u32 = 480;
    let frame_size = (width * height * 3 / 2) as usize;

    // Read fixture
    let fixture_path = std::env::args().nth(1)
        .unwrap_or_else(|| "/tmp/nvpro_video_demo/motion_fixture.yuv".to_string());

    println!("Reading NV12 fixture: {}", fixture_path);
    let fixture_data = std::fs::read(&fixture_path).expect("Cannot read fixture");
    let total_frames = fixture_data.len() / frame_size;
    println!("  {} frames, {}x{}", total_frames, width, height);

    let codecs = [
        ("H.264", Codec::H264, "h264"),
        ("H.265", Codec::H265, "h265"),
    ];

    for (name, codec, ext) in &codecs {
        println!("\n=== {} Encode ===", name);

        // Encode
        let config = SimpleEncoderConfig {
            width, height, fps: 30,
            codec: *codec,
            preset: Preset::Quality,
            streaming: false,
            ..Default::default()
        };

        let mut encoder = match SimpleEncoder::new(config) {
            Ok(e) => e,
            Err(e) => {
                println!("  Encoder creation failed: {}. Skipping {}.", e, name);
                continue;
            }
        };

        let mut bitstream = Vec::new();
        for f in 0..total_frames {
            let offset = f * frame_size;
            let frame_data = &fixture_data[offset..offset + frame_size];
            match encoder.submit_frame(frame_data, None) {
                Ok(packets) => {
                    for packet in &packets {
                        bitstream.extend_from_slice(&packet.data);
                    }
                    if f == 0 || f == total_frames - 1 {
                        let total_pkt_bytes: usize = packets.iter().map(|p| p.data.len()).sum();
                        let any_keyframe = packets.iter().any(|p| p.is_keyframe);
                        println!("  Frame {:>3}: {} bytes, keyframe={}", f, total_pkt_bytes, any_keyframe);
                    }
                }
                Err(e) => {
                    println!("  Encode frame {} failed: {}", f, e);
                    break;
                }
            }
        }

        let h264_path = format!("/tmp/nvpro_video_demo/encoded.{}", ext);
        std::fs::write(&h264_path, &bitstream).expect("write failed");
        println!("  Wrote {} bytes to {}", bitstream.len(), h264_path);

        // Wrap in MP4 with silent audio for Telegram
        let mp4_path = format!("/tmp/nvpro_video_demo/encoded_{}.mp4", ext);
        let _ = std::process::Command::new("ffmpeg")
            .args([
                "-y", "-r", "30", "-i", &h264_path,
                "-f", "lavfi", "-i", "anullsrc=r=44100:cl=mono",
                "-c:v", "copy", "-c:a", "aac", "-b:a", "32k",
                "-shortest", "-movflags", "+faststart",
                &mp4_path,
            ])
            .output();

        if std::fs::metadata(&mp4_path).map(|m| m.len() > 0).unwrap_or(false) {
            println!("  MP4: {}", mp4_path);
        } else {
            // Try without audio
            let _ = std::process::Command::new("ffmpeg")
                .args(["-y", "-r", "30", "-i", &h264_path, "-c:v", "copy",
                       "-movflags", "+faststart", &mp4_path])
                .output();
            println!("  MP4 (no audio): {}", mp4_path);
        }

        // Decode (H.264 only for now)
        if *codec == Codec::H264 {
            println!("\n=== {} Decode ===", name);
            let dec_config = SimpleDecoderConfig {
                codec: *codec,
                ..Default::default()
            };

            let mut decoder = match SimpleDecoder::new(dec_config) {
                Ok(d) => d,
                Err(e) => {
                    println!("  Decoder creation failed: {}. Skipping.", e);
                    continue;
                }
            };

            let mut decoded_yuv = Vec::new();
            let frames = match decoder.feed(&bitstream) {
                Ok(f) => f,
                Err(e) => {
                    println!("  Decode failed: {}. Skipping.", e);
                    continue;
                }
            };

            println!("  Decoded {} frames", frames.len());
            for frame in &frames {
                decoded_yuv.extend_from_slice(&frame.data);
            }

            if !decoded_yuv.is_empty() {
                let decoded_yuv_path = format!("/tmp/nvpro_video_demo/decoded_{}.yuv", ext);
                std::fs::write(&decoded_yuv_path, &decoded_yuv).expect("write failed");

                // Re-encode decoded NV12 as H.264 using our own encoder (Telegram needs H.264)
                let decoded_h264_path = format!("/tmp/nvpro_video_demo/decoded_{}_reenc.h264", ext);
                let decoded_mp4 = format!("/tmp/nvpro_video_demo/decoded_{}.mp4", ext);
                let decoded_frames = decoded_yuv.len() / frame_size;

                let reenc_config = SimpleEncoderConfig {
                    width, height, fps: 30,
                    codec: Codec::H264,
                    preset: Preset::Quality,
                    streaming: false,
                    ..Default::default()
                };

                if let Ok(mut reenc) = SimpleEncoder::new(reenc_config) {
                    let mut reenc_bs = Vec::new();
                    for f in 0..decoded_frames {
                        let off = f * frame_size;
                        if let Ok(packets) = reenc.submit_frame(&decoded_yuv[off..off + frame_size], None) {
                            for pkt in packets {
                                reenc_bs.extend_from_slice(&pkt.data);
                            }
                        }
                    }
                    let _ = std::fs::write(&decoded_h264_path, &reenc_bs);

                    // Wrap in MP4 with audio
                    let _ = std::process::Command::new("ffmpeg")
                        .args([
                            "-y", "-r", "30", "-i", &decoded_h264_path,
                            "-f", "lavfi", "-t", "3", "-i", "anullsrc=r=44100:cl=stereo",
                            "-c:v", "copy", "-c:a", "aac", "-b:a", "64k",
                            "-map", "0:v", "-map", "1:a", "-shortest",
                            "-movflags", "+faststart", &decoded_mp4,
                        ])
                        .output();
                }

                if std::fs::metadata(&decoded_mp4).map(|m| m.len() > 0).unwrap_or(false) {
                    println!("  Decoded MP4: {} ({} frames)", decoded_mp4, decoded_frames);
                }
            }
        }
    }

    // Create reference original MP4 using our own H.264 encoder at near-lossless quality
    println!("\n=== Original fixture MP4 ===");
    let orig_h264 = "/tmp/nvpro_video_demo/original_fixture.h264";
    let orig_mp4 = "/tmp/nvpro_video_demo/original_fixture.mp4";

    let orig_config = SimpleEncoderConfig {
        width, height, fps: 30,
        codec: Codec::H264,
        preset: Preset::Quality,
        streaming: false,
        ..Default::default()
    };

    if let Ok(mut orig_enc) = SimpleEncoder::new(orig_config) {
        let mut orig_bs = Vec::new();
        for f in 0..total_frames {
            let offset = f * frame_size;
            if let Ok(packets) = orig_enc.submit_frame(&fixture_data[offset..offset + frame_size], None) {
                for pkt in packets {
                    orig_bs.extend_from_slice(&pkt.data);
                }
            }
        }
        let _ = std::fs::write(orig_h264, &orig_bs);

        // Wrap in MP4 with silent audio
        let _ = std::process::Command::new("ffmpeg")
            .args([
                "-y", "-r", "30", "-i", orig_h264,
                "-f", "lavfi", "-t", "3", "-i", "anullsrc=r=44100:cl=stereo",
                "-c:v", "copy", "-c:a", "aac", "-b:a", "64k",
                "-map", "0:v", "-map", "1:a", "-shortest",
                "-movflags", "+faststart", orig_mp4,
            ])
            .output();

        if std::fs::metadata(orig_mp4).map(|m| m.len() > 0).unwrap_or(false) {
            println!("  Original MP4: {} (encoded with our H.264 at QP=18)", orig_mp4);
        }
    }

    println!("\n=== Done ===");
    println!("Files in /tmp/nvpro_video_demo/:");
    if let Ok(entries) = std::fs::read_dir("/tmp/nvpro_video_demo") {
        let mut files: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        files.sort_by_key(|e| e.file_name());
        for entry in files {
            if let Ok(meta) = entry.metadata() {
                if meta.len() > 0 {
                    println!("  {} ({} bytes)", entry.file_name().to_string_lossy(), meta.len());
                }
            }
        }
    }
}
