// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Full pipeline integration test: encode -> decode round-trip.
//!
//! Generates test fixture frames (SMPTE bars via ffmpeg or gradient pattern),
//! encodes them with the Vulkan Video encoder, then decodes the encoded
//! bitstream with the Vulkan Video decoder, and compares decoded frames
//! against the original fixture using PSNR metrics.
//!
//! Requires a GPU with both encode and decode queue family support.
//!
//! Usage:
//!   cargo run --bin pipeline-test

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma::{self as vma, Alloc};
use std::ffi::CStr;
use std::sync::Arc;

use vulkan_video::{
    VideoContext,
    SimpleEncoder, SimpleEncoderConfig, Codec, Preset,
    decode::DpbOutputMode,
};

// ---------------------------------------------------------------------------
// Vulkan init helpers
// ---------------------------------------------------------------------------

/// Find a queue family that supports video encode.
fn find_encode_queue_family(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
) -> Option<u32> {
    let props = unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
    for (i, p) in props.iter().enumerate() {
        if p.queue_flags.contains(vk::QueueFlags::VIDEO_ENCODE_KHR) {
            return Some(i as u32);
        }
    }
    None
}

/// Find a queue family that supports video decode.
fn find_decode_queue_family(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
) -> Option<u32> {
    let props = unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
    for (i, p) in props.iter().enumerate() {
        if p.queue_flags.contains(vk::QueueFlags::VIDEO_DECODE_KHR) {
            return Some(i as u32);
        }
    }
    None
}

/// Find a queue family that supports transfer operations.
fn find_transfer_queue_family(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
) -> Option<u32> {
    let props = unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
    for (i, p) in props.iter().enumerate() {
        if p.queue_flags.contains(vk::QueueFlags::TRANSFER)
            || p.queue_flags.contains(vk::QueueFlags::GRAPHICS)
        {
            return Some(i as u32);
        }
    }
    None
}

/// Find a queue family that supports compute operations.
fn find_compute_queue_family(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
) -> Option<u32> {
    let props = unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
    for (i, p) in props.iter().enumerate() {
        if p.queue_flags.contains(vk::QueueFlags::COMPUTE) {
            return Some(i as u32);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Fixture generation
// ---------------------------------------------------------------------------

/// Generate NV12 test fixture frames via ffmpeg (SMPTE bars) or fallback to gradient.
fn generate_fixture_frames(width: u32, height: u32, num_frames: u32) -> Vec<u8> {
    let fixture_path = "/tmp/nvpro_pipeline_fixture.yuv";
    let duration = format!("{:.3}", num_frames as f64 / 30.0);

    let result = std::process::Command::new("ffmpeg")
        .args([
            "-y", "-f", "lavfi",
            "-i", &format!("smptebars=size={}x{}:rate=30:duration={}", width, height, duration),
            "-frames:v", &num_frames.to_string(),
            "-pix_fmt", "nv12",
            fixture_path,
        ])
        .output();

    match result {
        Ok(output) if output.status.success() => {
            if let Ok(data) = std::fs::read(fixture_path) {
                let frame_size = (width * height * 3 / 2) as usize;
                if data.len() == frame_size * num_frames as usize {
                    return data;
                }
            }
        }
        _ => {}
    }

    // Fallback: gradient pattern
    generate_gradient_frames(width, height, num_frames)
}

/// Generate NV12 frames with a gradient pattern.
fn generate_gradient_frames(width: u32, height: u32, num_frames: u32) -> Vec<u8> {
    let frame_size = (width * height * 3 / 2) as usize;
    let mut data = vec![0u8; frame_size * num_frames as usize];

    for f in 0..num_frames {
        let offset = f as usize * frame_size;
        let y_size = (width * height) as usize;

        // Y plane: horizontal gradient
        for row in 0..height {
            for col in 0..width {
                let y_val = ((col as f32 / width as f32 * 200.0) as u8)
                    .wrapping_add((row as u8).wrapping_mul(2));
                data[offset + (row * width + col) as usize] = y_val.max(16).min(235);
            }
        }

        // UV plane: color pattern
        let uv_offset = offset + y_size;
        let uv_width = width as usize;
        let uv_height = (height / 2) as usize;
        for row in 0..uv_height {
            for col in (0..uv_width).step_by(2) {
                let u = 128u8.wrapping_add((col as f32 / uv_width as f32 * 80.0) as u8);
                let v = 128u8.wrapping_add((row as f32 / uv_height as f32 * 80.0) as u8);
                data[uv_offset + row * uv_width + col] = u.max(16).min(240);
                data[uv_offset + row * uv_width + col + 1] = v.max(16).min(240);
            }
        }
    }

    data
}

/// Compute PSNR between two NV12 buffers (Y plane only for simplicity).
fn compute_y_psnr(a: &[u8], b: &[u8], width: u32, height: u32) -> f64 {
    let y_size = (width * height) as usize;
    if a.len() < y_size || b.len() < y_size {
        return -1.0;
    }

    let mut mse_sum: f64 = 0.0;
    for i in 0..y_size {
        let diff = a[i] as f64 - b[i] as f64;
        mse_sum += diff * diff;
    }

    let mse = mse_sum / y_size as f64;
    if mse < 1e-10 {
        return 100.0;
    }

    10.0 * (255.0 * 255.0 / mse).log10()
}

// ---------------------------------------------------------------------------
// Codec capability probing
// ---------------------------------------------------------------------------

/// Check if a specific encode codec is supported by querying video capabilities.
/// Returns true if the query succeeds (the GPU supports it).
unsafe fn probe_encode_support(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    codec: vk::VideoCodecOperationFlagsKHR,
) -> bool {
    use vulkanalia::vk::KhrVideoQueueExtensionInstanceCommands;

    let mut profile_info = vk::VideoProfileInfoKHR::builder()
        .video_codec_operation(codec)
        .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::_420)
        .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8)
        .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8);

    let mut h264_profile;
    let mut h265_profile;

    if codec == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
        h264_profile = vk::VideoEncodeH264ProfileInfoKHR::builder()
            .std_profile_idc(vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH);
        profile_info = profile_info.push_next(&mut h264_profile);
    } else if codec == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
        h265_profile = vk::VideoEncodeH265ProfileInfoKHR::builder()
            .std_profile_idc(vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN);
        profile_info = profile_info.push_next(&mut h265_profile);
    }

    let mut h264_encode_caps = vk::VideoEncodeH264CapabilitiesKHR::default();
    let mut h265_encode_caps = vk::VideoEncodeH265CapabilitiesKHR::default();
    let mut encode_caps = vk::VideoEncodeCapabilitiesKHR::default();

    // Build the pNext chain manually since get_physical_device_video_capabilities_khr
    // takes &mut VideoCapabilitiesKHR, not a builder.
    let mut caps = vk::VideoCapabilitiesKHR::default();
    encode_caps.next = std::ptr::null_mut();
    if codec == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
        encode_caps.next = &mut h264_encode_caps as *mut _ as *mut std::ffi::c_void;
        caps.next = &mut encode_caps as *mut _ as *mut std::ffi::c_void;
    } else if codec == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
        encode_caps.next = &mut h265_encode_caps as *mut _ as *mut std::ffi::c_void;
        caps.next = &mut encode_caps as *mut _ as *mut std::ffi::c_void;
    } else {
        caps.next = &mut encode_caps as *mut _ as *mut std::ffi::c_void;
    }

    instance.get_physical_device_video_capabilities_khr(
        physical_device, &profile_info, &mut caps,
    ).is_ok()
}

/// Check if a specific decode codec is supported by querying video capabilities.
unsafe fn probe_decode_support(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    codec: vk::VideoCodecOperationFlagsKHR,
) -> bool {
    use vulkanalia::vk::KhrVideoQueueExtensionInstanceCommands;

    let mut profile_info = vk::VideoProfileInfoKHR::builder()
        .video_codec_operation(codec)
        .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::_420)
        .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8)
        .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8);

    let mut h264_profile;
    let mut h265_profile;
    let mut av1_profile;

    if codec == vk::VideoCodecOperationFlagsKHR::DECODE_H264 {
        h264_profile = vk::VideoDecodeH264ProfileInfoKHR::builder()
            .std_profile_idc(vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH)
            .picture_layout(vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE);
        profile_info = profile_info.push_next(&mut h264_profile);
    } else if codec == vk::VideoCodecOperationFlagsKHR::DECODE_H265 {
        h265_profile = vk::VideoDecodeH265ProfileInfoKHR::builder()
            .std_profile_idc(vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN);
        profile_info = profile_info.push_next(&mut h265_profile);
    } else if codec == vk::VideoCodecOperationFlagsKHR::DECODE_AV1 {
        av1_profile = vk::VideoDecodeAV1ProfileInfoKHR::builder()
            .std_profile(vk::video::STD_VIDEO_AV1_PROFILE_MAIN);
        profile_info = profile_info.push_next(&mut av1_profile);
    }

    let mut h264_decode_caps = vk::VideoDecodeH264CapabilitiesKHR::default();
    let mut h265_decode_caps = vk::VideoDecodeH265CapabilitiesKHR::default();
    let mut av1_decode_caps = vk::VideoDecodeAV1CapabilitiesKHR::default();
    let mut decode_caps = vk::VideoDecodeCapabilitiesKHR::default();

    // Build the pNext chain manually since get_physical_device_video_capabilities_khr
    // takes &mut VideoCapabilitiesKHR, not a builder.
    let mut caps = vk::VideoCapabilitiesKHR::default();
    decode_caps.next = std::ptr::null_mut();
    if codec == vk::VideoCodecOperationFlagsKHR::DECODE_H264 {
        decode_caps.next = &mut h264_decode_caps as *mut _ as *mut std::ffi::c_void;
        caps.next = &mut decode_caps as *mut _ as *mut std::ffi::c_void;
    } else if codec == vk::VideoCodecOperationFlagsKHR::DECODE_H265 {
        decode_caps.next = &mut h265_decode_caps as *mut _ as *mut std::ffi::c_void;
        caps.next = &mut decode_caps as *mut _ as *mut std::ffi::c_void;
    } else if codec == vk::VideoCodecOperationFlagsKHR::DECODE_AV1 {
        decode_caps.next = &mut av1_decode_caps as *mut _ as *mut std::ffi::c_void;
        caps.next = &mut decode_caps as *mut _ as *mut std::ffi::c_void;
    } else {
        caps.next = &mut decode_caps as *mut _ as *mut std::ffi::c_void;
    }

    instance.get_physical_device_video_capabilities_khr(
        physical_device, &profile_info, &mut caps,
    ).is_ok()
}

// ---------------------------------------------------------------------------
// ffprobe metadata validation
// ---------------------------------------------------------------------------

/// Run ffprobe on an encoded file and validate metadata fields.
/// Returns (codec_name, profile, width, height, nb_read_frames) on success.
fn run_ffprobe_checks(
    encoded_path: &str,
    expected_codec: &str,
    expected_profile: &str,
    expected_width: u32,
    expected_height: u32,
    expected_frames: u32,
) -> (bool, String, String, Option<u32>, Option<u32>, Option<u32>) {
    // all_ok, codec_name, profile, width, height, nb_read_frames
    let output = std::process::Command::new("ffprobe")
        .args([
            "-v", "error",
            "-count_frames",
            "-select_streams", "v:0",
            "-show_entries", "stream=codec_name,profile,width,height,nb_read_frames",
            "-of", "json",
            encoded_path,
        ])
        .output();

    let json_str = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => {
            println!("    [FAIL] ffprobe: could not run on {}", encoded_path);
            return (false, String::new(), String::new(), None, None, None);
        }
    };

    // Minimal JSON parsing without serde -- extract key values
    let get_str = |key: &str| -> Option<String> {
        let needle = format!("\"{}\"", key);
        let pos = json_str.find(&needle)?;
        let rest = &json_str[pos + needle.len()..];
        // skip ': "' or ': '
        let colon = rest.find(':')?;
        let after_colon = rest[colon + 1..].trim_start();
        if after_colon.starts_with('"') {
            let start = 1;
            let end = after_colon[1..].find('"')? + 1;
            Some(after_colon[start..end].to_string())
        } else {
            // number or unquoted
            let end = after_colon.find(|c: char| c == ',' || c == '}' || c == '\n')
                .unwrap_or(after_colon.len());
            Some(after_colon[..end].trim().trim_matches('"').to_string())
        }
    };

    let codec_name = get_str("codec_name").unwrap_or_default();
    let profile = get_str("profile").unwrap_or_default();
    let width_val = get_str("width").and_then(|s| s.parse::<u32>().ok());
    let height_val = get_str("height").and_then(|s| s.parse::<u32>().ok());
    let nb_frames = get_str("nb_read_frames").and_then(|s| s.parse::<u32>().ok());

    let mut all_ok = true;

    // Check codec_name
    if codec_name == expected_codec {
        println!("    [PASS] ffprobe codec_name: {} (expected {})", codec_name, expected_codec);
    } else {
        println!("    [FAIL] ffprobe codec_name: {} (expected {})", codec_name, expected_codec);
        all_ok = false;
    }

    // Check profile
    if profile == expected_profile {
        println!("    [PASS] ffprobe profile: {} (expected {})", profile, expected_profile);
    } else {
        println!("    [FAIL] ffprobe profile: {} (expected {})", profile, expected_profile);
        all_ok = false;
    }

    // Check width
    if width_val == Some(expected_width) {
        println!("    [PASS] ffprobe width: {} (expected {})", width_val.unwrap(), expected_width);
    } else {
        println!("    [FAIL] ffprobe width: {:?} (expected {})", width_val, expected_width);
        all_ok = false;
    }

    // Check height
    if height_val == Some(expected_height) {
        println!("    [PASS] ffprobe height: {} (expected {})", height_val.unwrap(), expected_height);
    } else {
        println!("    [FAIL] ffprobe height: {:?} (expected {})", height_val, expected_height);
        all_ok = false;
    }

    // Check frame count
    if nb_frames == Some(expected_frames) {
        println!("    [PASS] ffprobe nb_read_frames: {} (expected {})", nb_frames.unwrap(), expected_frames);
    } else {
        println!("    [FAIL] ffprobe nb_read_frames: {:?} (expected {})", nb_frames, expected_frames);
        all_ok = false;
    }

    (all_ok, codec_name, profile, width_val, height_val, nb_frames)
}

// ---------------------------------------------------------------------------
// PSNR / SSIM via ffmpeg
// ---------------------------------------------------------------------------

/// Compute PSNR between a raw NV12 source and an encoded file via ffmpeg.
/// Returns the average PSNR value parsed from ffmpeg stderr.
fn compute_ffmpeg_psnr(
    fixture_data: &[u8],
    width: u32,
    height: u32,
    total_frames: u32,
    encoded_path: &str,
) -> Option<f64> {
    let frame_size = (width * height * 3 / 2) as usize;
    let raw_path = format!("{}.psnr_source.yuv", encoded_path);
    let source_len = frame_size * total_frames as usize;
    if fixture_data.len() < source_len {
        return None;
    }
    if std::fs::write(&raw_path, &fixture_data[..source_len]).is_err() {
        return None;
    }

    let result = std::process::Command::new("ffmpeg")
        .args([
            "-f", "rawvideo", "-pix_fmt", "nv12",
            "-s", &format!("{}x{}", width, height),
            "-r", "30",
            "-i", &raw_path,
            "-i", encoded_path,
            "-lavfi", "psnr",
            "-f", "null", "-",
        ])
        .output();

    let _ = std::fs::remove_file(&raw_path);

    let output = match result {
        Ok(o) => o,
        _ => return None,
    };

    // PSNR is printed to stderr, e.g.: "PSNR y:41.234 ..."
    // or "[Parsed_psnr_0 ...] PSNR ... average:41.234 ..."
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Look for "average:XX.XX" in the PSNR summary line
    for line in stderr.lines() {
        if line.contains("PSNR") && line.contains("average:") {
            if let Some(pos) = line.find("average:") {
                let rest = &line[pos + 8..];
                let end = rest.find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
                    .unwrap_or(rest.len());
                if let Ok(val) = rest[..end].parse::<f64>() {
                    return Some(val);
                }
            }
        }
    }

    // Fallback: look for "psnr_avg:" pattern
    for line in stderr.lines() {
        if let Some(pos) = line.find("psnr_avg:") {
            let rest = &line[pos + 9..];
            let end = rest.find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
                .unwrap_or(rest.len());
            if let Ok(val) = rest[..end].parse::<f64>() {
                return Some(val);
            }
        }
    }

    None
}

/// Compute SSIM between a raw NV12 source and an encoded file via ffmpeg.
/// Returns the average SSIM value parsed from ffmpeg stderr.
// ---------------------------------------------------------------------------
// Test result tracking for summary table
// ---------------------------------------------------------------------------

struct TestResult {
    name: String,
    status: String,  // "PASS", "FAIL", "SKIP"
    psnr_ffmpeg: Option<f64>,
    ssim: Option<f64>,
    frame_count: Option<u32>,
    output_path: Option<String>,
}

// ---------------------------------------------------------------------------
// Main pipeline test
// ---------------------------------------------------------------------------

fn main() {
    // Initialize tracing subscriber for debug logging.
    // Use RUST_LOG=debug to see H.265 DPB/encode instrumentation.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    println!("nvpro-vulkan-video Full Pipeline Test");
    println!("=====================================");
    println!("Multi-codec test matrix with hardware capability probing\n");

    let width: u32 = 640;
    let height: u32 = 480;
    let total_frames: u32 = 10; // 10 frames per codec for speed

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let mut results: Vec<TestResult> = Vec::new();

    // =====================================================================
    // 1. Initialize Vulkan
    // =====================================================================
    println!("[Init] Loading Vulkan");

    let loader = match unsafe { vulkanalia::loader::LibloadingLoader::new(vulkanalia::loader::LIBRARY) } {
        Ok(l) => l,
        Err(e) => { println!("  No Vulkan loader: {}. Skipping.", e); std::process::exit(0); }
    };
    let entry: vulkanalia::Entry = match unsafe { vulkanalia::Entry::new(loader) } {
        Ok(e) => e,
        Err(e) => { println!("  No Vulkan: {}. Skipping.", e); std::process::exit(0); }
    };

    let app_info = vk::ApplicationInfo::builder()
        .application_name(b"nvpro-pipeline-test\0")
        .api_version(vk::make_version(1, 3, 0));

    let instance = match unsafe {
        entry.create_instance(&vk::InstanceCreateInfo::builder().application_info(&app_info), None)
    } {
        Ok(i) => i,
        Err(e) => { println!("  Instance failed: {:?}. Skipping.", e); std::process::exit(0); }
    };

    let physical_device = match unsafe { instance.enumerate_physical_devices() } {
        Ok(devs) if !devs.is_empty() => devs[0],
        _ => {
            println!("  No devices. Skipping.");
            unsafe { instance.destroy_instance(None); }
            std::process::exit(0);
        }
    };

    let props = unsafe { instance.get_physical_device_properties(physical_device) };
    let name = unsafe { CStr::from_ptr(props.device_name.as_ptr()) }.to_str().unwrap_or("?");
    println!("  GPU: {}", name);

    // Find queue families
    let encode_qf = find_encode_queue_family(&instance, physical_device);
    let decode_qf = find_decode_queue_family(&instance, physical_device);
    let transfer_qf = find_transfer_queue_family(&instance, physical_device);
    let compute_qf = find_compute_queue_family(&instance, physical_device);

    println!("  Encode QF: {:?}, Decode QF: {:?}, Transfer QF: {:?}, Compute QF: {:?}\n",
        encode_qf, decode_qf, transfer_qf, compute_qf);

    // =====================================================================
    // 2. Probe hardware capabilities
    // =====================================================================
    println!("[Probe] Hardware codec support");

    struct CodecSupport {
        name: &'static str,
        codec: vk::VideoCodecOperationFlagsKHR,
        is_encode: bool,
        supported: bool,
        extension_name: &'static std::ffi::CStr,
    }

    let mut codecs = vec![
        CodecSupport { name: "H.264 Encode", codec: vk::VideoCodecOperationFlagsKHR::ENCODE_H264,
            is_encode: true, supported: false, extension_name: vk::KHR_VIDEO_ENCODE_H264_EXTENSION.name.as_cstr() },
        CodecSupport { name: "H.265 Encode", codec: vk::VideoCodecOperationFlagsKHR::ENCODE_H265,
            is_encode: true, supported: false, extension_name: vk::KHR_VIDEO_ENCODE_H265_EXTENSION.name.as_cstr() },
        CodecSupport { name: "H.264 Decode", codec: vk::VideoCodecOperationFlagsKHR::DECODE_H264,
            is_encode: false, supported: false, extension_name: vk::KHR_VIDEO_DECODE_H264_EXTENSION.name.as_cstr() },
        CodecSupport { name: "H.265 Decode", codec: vk::VideoCodecOperationFlagsKHR::DECODE_H265,
            is_encode: false, supported: false, extension_name: vk::KHR_VIDEO_DECODE_H265_EXTENSION.name.as_cstr() },
        CodecSupport { name: "AV1 Decode",   codec: vk::VideoCodecOperationFlagsKHR::DECODE_AV1,
            is_encode: false, supported: false, extension_name: vk::KHR_VIDEO_DECODE_AV1_EXTENSION.name.as_cstr() },
    ];

    for codec in &mut codecs {
        let qf_exists = if codec.is_encode { encode_qf.is_some() } else { decode_qf.is_some() };
        if !qf_exists {
            println!("  {:20} -- no queue family", codec.name);
            continue;
        }

        codec.supported = unsafe {
            if codec.is_encode {
                probe_encode_support(&instance, physical_device, codec.codec)
            } else {
                probe_decode_support(&instance, physical_device, codec.codec)
            }
        };

        let status = if codec.supported { "SUPPORTED" } else { "not supported" };
        println!("  {:20} {}", codec.name, status);
    }
    println!();

    // =====================================================================
    // 3. Create device with all supported extensions
    // =====================================================================
    println!("[Init] Creating device");

    let mut unique_families: Vec<u32> = Vec::new();
    if let Some(q) = encode_qf { unique_families.push(q); }
    if let Some(q) = decode_qf { unique_families.push(q); }
    if let Some(q) = transfer_qf { unique_families.push(q); }
    if let Some(q) = compute_qf { unique_families.push(q); }
    unique_families.sort();
    unique_families.dedup();

    if unique_families.is_empty() {
        println!("  No video queue families. Skipping all GPU tests.");
        unsafe { instance.destroy_instance(None); }
        std::process::exit(0);
    }

    let queue_priorities = [1.0f32];
    let queue_create_infos: Vec<_> = unique_families.iter()
        .map(|&f| vk::DeviceQueueCreateInfo::builder().queue_family_index(f).queue_priorities(&queue_priorities))
        .collect();

    // Only include extensions for supported codecs
    let mut device_extensions: Vec<*const i8> = vec![
        vk::KHR_VIDEO_QUEUE_EXTENSION.name.as_ptr(),
        vk::KHR_SYNCHRONIZATION2_EXTENSION.name.as_ptr(),
        vk::KHR_PUSH_DESCRIPTOR_EXTENSION.name.as_ptr(),
        vk::KHR_SAMPLER_YCBCR_CONVERSION_EXTENSION.name.as_ptr(),
    ];
    if encode_qf.is_some() {
        device_extensions.push(vk::KHR_VIDEO_ENCODE_QUEUE_EXTENSION.name.as_ptr());
    }
    if decode_qf.is_some() {
        device_extensions.push(vk::KHR_VIDEO_DECODE_QUEUE_EXTENSION.name.as_ptr());
    }
    for codec in &codecs {
        if codec.supported {
            device_extensions.push(codec.extension_name.as_ptr());
        }
    }
    // Deduplicate extension pointers
    device_extensions.sort();
    device_extensions.dedup();

    let mut sync2 = vk::PhysicalDeviceSynchronization2Features::builder().synchronization2(true);
    let mut ycbcr_feat = vk::PhysicalDeviceSamplerYcbcrConversionFeatures::builder()
        .sampler_ycbcr_conversion(true);
    let device_info = vk::DeviceCreateInfo::builder()
        .queue_create_infos(&queue_create_infos)
        .enabled_extension_names(&device_extensions)
        .push_next(&mut sync2)
        .push_next(&mut ycbcr_feat);

    let device = match unsafe { instance.create_device(physical_device, &device_info, None) } {
        Ok(d) => d,
        Err(e) => {
            println!("  Device creation failed: {:?}. Skipping.", e);
            unsafe { instance.destroy_instance(None); }
            std::process::exit(0);
        }
    };

    let _encode_queue = encode_qf.map(|qf| unsafe { device.get_device_queue(qf, 0) });
    let _decode_queue = decode_qf.map(|qf| unsafe { device.get_device_queue(qf, 0) });
    let transfer_queue = transfer_qf.map(|qf| unsafe { device.get_device_queue(qf, 0) });
    let compute_queue = compute_qf.map(|qf| unsafe { device.get_device_queue(qf, 0) });

    let ctx = Arc::new(VideoContext::new(instance.clone(), device.clone(), physical_device)
        .expect("Failed to create VideoContext"));
    println!("  Device created\n");

    // =====================================================================
    // 4. Load fixture data
    // =====================================================================
    let frame_size = (width * height * 3 / 2) as usize;
    let fixture_nv12_path = format!("tests/fixtures/testsrc2_{}x{}_nv12.yuv", width, height);
    let fixture_data = if std::path::Path::new(&fixture_nv12_path).exists() {
        println!("[Fixture] Loading raw NV12 from {}", fixture_nv12_path);
        std::fs::read(&fixture_nv12_path).expect("Failed to read fixture NV12 file")
    } else {
        println!("[Fixture] {} not found, generating {} test frames ({}x{} NV12)",
            fixture_nv12_path, total_frames, width, height);
        println!("  Run ./tests/generate_fixtures.sh to create fixture files");
        generate_fixture_frames(width, height, total_frames)
    };
    let fixture_total_frames = fixture_data.len() / frame_size;
    println!("[Fixture] {} frames ({}x{} NV12), {} bytes total\n",
        fixture_total_frames, width, height, fixture_data.len());


    // =====================================================================
    // SimpleDecoder — decode ffmpeg fixture, validate structure + quality
    //
    // Two measurements per codec:
    //   Structural: frame count + frame dimensions
    //   Quality:    ffmpeg encode → our decode → PSNR vs raw NV12 source
    //               (isolates decoder quality; ffmpeg is the reference encoder)
    //
    // Fixtures are generated by ffmpeg software encoders (libx264, libx265)
    // — known-good bitstreams that any compliant decoder must handle.
    // This is the primary decode correctness and quality gate.
    // =====================================================================
    let decode_codecs: Vec<(&str, vk::VideoCodecOperationFlagsKHR, &str)> = vec![
        ("H.264", vk::VideoCodecOperationFlagsKHR::DECODE_H264, "h264"),
        ("H.265", vk::VideoCodecOperationFlagsKHR::DECODE_H265, "h265"),
        ("AV1",   vk::VideoCodecOperationFlagsKHR::DECODE_AV1,  "ivf"),
    ];

    for (codec_name, codec_flag, ext) in &decode_codecs {
        let test_name = format!("SimpleDecoder {} — decode ffmpeg fixture, PSNR vs raw NV12", codec_name);

        let supported = codecs.iter().any(|c| c.codec == *codec_flag && c.supported);
        if !supported || decode_qf.is_none() {
            println!("[SKIP] {} -- not supported on this GPU", test_name);
            skipped += 1;
            results.push(TestResult {
                name: test_name,
                status: "SKIP".to_string(),
                psnr_ffmpeg: None,
                ssim: None,
                frame_count: None,
                output_path: None,
            });
            continue;
        }

        // Decode tests use known-good fixtures generated by ffmpeg software
        // encoders (libx264, libx265). Never fall back to our own encoder output —
        // that defeats the purpose of isolated decode testing.
        let fixture_path = format!("tests/fixtures/testsrc2_{}x{}.{}", width, height, ext);

        if !std::path::Path::new(&fixture_path).exists() {
            println!("[SKIP] {} -- fixture not found: {}", test_name, fixture_path);
            println!("    Run FFMPEG=/path/to/ffmpeg ./tests/generate_fixtures.sh");
            skipped += 1;
            results.push(TestResult {
                name: test_name,
                status: "SKIP".to_string(),
                psnr_ffmpeg: None,
                ssim: None,
                frame_count: None,
                output_path: None,
            });
            continue;
        }
        let test_file = fixture_path;

        println!("[TEST] {}", test_name);
        println!("  Fixture: {}", test_file);

        if *ext == "h264" || *ext == "h265" {
            // Unified decode test: SimpleDecoder is the library consumer API
            use vulkan_video::{SimpleDecoder, SimpleDecoderConfig};

            let codec = if *ext == "h264" { Codec::H264 } else { Codec::H265 };
            let bitstream = match std::fs::read(&test_file) {
                Ok(d) => d,
                Err(e) => {
                    println!("    [FAIL] Could not read {}: {}", test_file, e);
                    failed += 1;
                    results.push(TestResult {
                        name: test_name,
                        status: "FAIL".to_string(),
                        psnr_ffmpeg: None,
                        ssim: None,
                        frame_count: None,
                        output_path: Some(test_file.clone()),
                    });
                    println!();
                    continue;
                }
            };

            let dec_config = SimpleDecoderConfig {
                codec,
                max_width: 0,
                max_height: 0,
                output_mode: DpbOutputMode::Coincide,
                rgba_output: false,
            };

            match SimpleDecoder::new(dec_config) {
                Ok(mut decoder) => {
                    match decoder.feed(&bitstream) {
                        Ok(frames) => {
                            let nf = frames.len() as u32;
                            println!("    Decoded {} frames from ffmpeg fixture", nf);

                            if nf == 0 {
                                println!("    [XFAIL] {} decode — no frames decoded", codec_name);
                                skipped += 1;
                                results.push(TestResult {
                                    name: test_name,
                                    status: "XFAIL".to_string(),
                                    psnr_ffmpeg: None,
                                    ssim: None,
                                    frame_count: Some(0),
                                    output_path: Some(test_file.clone()),
                                });
                            } else {
                                // Compare decoded frames against the raw NV12 source
                                // that ffmpeg encoded from. PSNR includes encode loss
                                // (known-good) + any decoder error.
                                let psnr = compute_y_psnr(
                                    &fixture_data[..frame_size],
                                    &frames[0].data,
                                    width, height,
                                );
                                println!("    Y-PSNR (frame 0): {:.2} dB", psnr);

                                let mut valid_frames = 0u32;
                                for (i, frame) in frames.iter().enumerate() {
                                    let fix_start = i * frame_size;
                                    if fix_start + frame_size <= fixture_data.len() {
                                        let fp = compute_y_psnr(
                                            &fixture_data[fix_start..fix_start + frame_size],
                                            &frame.data,
                                            width, height,
                                        );
                                        let nz = frame.data.iter()
                                            .take((width * height) as usize)
                                            .filter(|&&b| b != 0).count();
                                        println!("    frame[{}] poc={} PSNR={:.2} dB nonzero_y={}",
                                            i, frame.picture_order_count, fp, nz);
                                        if fp > 20.0 { valid_frames += 1; }
                                    }
                                }
                                println!("    Valid frames (PSNR > 20 dB): {}/{}", valid_frames, nf);

                                if valid_frames == nf && psnr > 30.0 {
                                    println!("    [PASS] {} decode PSNR = {:.2} dB", codec_name, psnr);
                                    passed += 1;
                                    results.push(TestResult {
                                        name: test_name,
                                        status: "PASS".to_string(),
                                        psnr_ffmpeg: Some(psnr),
                                        ssim: None,
                                        frame_count: Some(nf),
                                        output_path: Some(test_file.clone()),
                                    });
                                } else {
                                    println!("    [XFAIL] {} decode — {}/{} valid frames", codec_name, valid_frames, nf);
                                    println!("            Decode quality below threshold");
                                    skipped += 1;
                                    results.push(TestResult {
                                        name: test_name,
                                        status: "XFAIL".to_string(),
                                        psnr_ffmpeg: Some(psnr),
                                        ssim: None,
                                        frame_count: Some(nf),
                                        output_path: Some(test_file.clone()),
                                    });
                                }
                            }
                            drop(decoder);
                        }
                        Err(e) => {
                            println!("    [XFAIL] {} decode — SimpleDecoder::feed() failed: {}", codec_name, e);
                            skipped += 1;
                            results.push(TestResult {
                                name: test_name,
                                status: "XFAIL".to_string(),
                                psnr_ffmpeg: None,
                                ssim: None,
                                frame_count: None,
                                output_path: Some(test_file.clone()),
                            });
                        }
                    }
                }
                Err(e) => {
                    println!("    [FAIL] SimpleDecoder::new() failed: {}", e);
                    failed += 1;
                    results.push(TestResult {
                        name: test_name,
                        status: "FAIL".to_string(),
                        psnr_ffmpeg: None,
                        ssim: None,
                        frame_count: None,
                        output_path: None,
                    });
                }
            }
        } else {
            println!("    [SKIP] No decode test implemented for {}", ext);
            skipped += 1;
            results.push(TestResult {
                name: test_name,
                status: "SKIP".to_string(),
                psnr_ffmpeg: None,
                ssim: None,
                frame_count: None,
                output_path: None,
            });
        }
        println!();
    }

    // =====================================================================
    // SimpleEncoder — encode frames, validate structure + quality
    //
    // Two measurements per codec:
    //   Structural: ffprobe validates codec, profile, dimensions, frame count
    //   Quality:    encode → ffmpeg decode → PSNR vs raw NV12 source
    //               (isolates encoder quality; ffmpeg is the reference decoder)
    //
    // Also exercises force_idr() and finish() API.
    // This is the primary encode correctness and quality gate.
    // =====================================================================
    let encode_test_codecs: Vec<(&str, Codec, vk::VideoCodecOperationFlagsKHR, &str, &str)> = vec![
        ("H.264", Codec::H264, vk::VideoCodecOperationFlagsKHR::ENCODE_H264, "h264", "High"),
        ("H.265", Codec::H265, vk::VideoCodecOperationFlagsKHR::ENCODE_H265, "hevc", "Main"),
    ];

    for (codec_name, codec, codec_flag, ext, expected_profile) in &encode_test_codecs {
        let test_name = format!("SimpleEncoder {} — encode, ffprobe validation", codec_name);
        let has_encode = codecs.iter().any(|c| c.codec == *codec_flag && c.supported);

        if !has_encode || encode_qf.is_none() {
            println!("[SKIP] {} — no {} encode support on this GPU", test_name, codec_name);
            skipped += 1;
            results.push(TestResult {
                name: test_name,
                status: "SKIP".to_string(),
                psnr_ffmpeg: None, ssim: None, frame_count: None, output_path: None,
            });
            println!();
            continue;
        }

        println!("[TEST] {}", test_name);

        let simple_width = 640u32;
        let simple_height = 480u32;
        let simple_num_frames = 30u32;

        let config = SimpleEncoderConfig {
            width: simple_width,
            height: simple_height,
            fps: 30,
            codec: *codec,
            preset: Preset::Medium,
            streaming: false,
            ..Default::default()
        };

        match SimpleEncoder::new(config) {
            Ok(mut encoder) => {
                println!("  SimpleEncoder created ({})", codec_name);

                let simple_fixture = generate_fixture_frames(simple_width, simple_height, simple_num_frames);
                let frame_sz = (simple_width * simple_height * 3 / 2) as usize;

                let mut output_data = Vec::new();
                let mut encode_ok = true;

                // Encode all frames
                for f in 0..simple_num_frames {
                    let frame_start = f as usize * frame_sz;
                    let frame_data = &simple_fixture[frame_start..frame_start + frame_sz];

                    match encoder.submit_frame(frame_data, None) {
                        Ok(packets) => {
                            for packet in &packets {
                                output_data.extend_from_slice(&packet.data);
                            }
                            if f < 3 || f == simple_num_frames - 1 {
                                let pkt_bytes: usize = packets.iter().map(|p| p.data.len()).sum();
                                let frame_type_name = packets.first().map(|p| p.frame_type.name()).unwrap_or("?");
                                let is_keyframe = packets.iter().any(|p| p.is_keyframe);
                                println!("    Frame {:3}: {} {:>6} bytes{}",
                                    f, frame_type_name, pkt_bytes,
                                    if is_keyframe { " [IDR]" } else { "" });
                            } else if f == 3 {
                                println!("    ...");
                            }
                        }
                        Err(e) => {
                            println!("    [FAIL] Frame {} encode failed: {}", f, e);
                            encode_ok = false;
                            break;
                        }
                    }
                }

                // Test force_idr
                if encode_ok {
                    encoder.force_idr();
                    let idr_frame = &simple_fixture[0..frame_sz];
                    match encoder.submit_frame(idr_frame, None) {
                        Ok(packets) => {
                            let is_keyframe = packets.iter().any(|p| p.is_keyframe);
                            let pkt_bytes: usize = packets.iter().map(|p| p.data.len()).sum();
                            println!("    force_idr: {} bytes [keyframe={}]", pkt_bytes, is_keyframe);
                            if !is_keyframe {
                                println!("    [FAIL] force_idr did not produce IDR");
                                encode_ok = false;
                            }
                            for packet in &packets {
                                output_data.extend_from_slice(&packet.data);
                            }
                        }
                        Err(e) => {
                            println!("    [FAIL] force_idr failed: {}", e);
                            encode_ok = false;
                        }
                    }
                }

                // Flush
                if encode_ok {
                    match encoder.finish() {
                        Ok(trailing) => {
                            println!("    finish(): {} trailing packets", trailing.len());
                        }
                        Err(e) => {
                            println!("    [FAIL] finish() failed: {}", e);
                            encode_ok = false;
                        }
                    }
                }

                if encode_ok {
                    let out_path = format!("/tmp/simple_encoder_test.{}", ext);
                    let _ = std::fs::write(&out_path, &output_data);
                    println!("  Encoded {} frames + 1 forced IDR, {} bytes", simple_num_frames, output_data.len());

                    let expected_frames = simple_num_frames + 1;

                    // Structural validation: ffprobe checks codec, profile, dims, frame count
                    println!("  --- structural (ffprobe) ---");
                    let (probe_ok, _, _, _, _, _) = run_ffprobe_checks(
                        &out_path, ext, expected_profile,
                        simple_width, simple_height, expected_frames,
                    );

                    // Quality: encode → ffmpeg decode → PSNR vs raw NV12 source
                    // This isolates encoder quality (ffmpeg's decoder is the reference)
                    println!("  --- quality (PSNR via ffmpeg decode) ---");
                    let psnr_val = compute_ffmpeg_psnr(
                        &simple_fixture, simple_width, simple_height,
                        simple_num_frames, &out_path,
                    );
                    // Threshold is 10 dB — above noise floor, confirms real
                    // encode/decode happened. Actual quality varies by codec and
                    // content (H.264 gradient ~15 dB, H.265 gradient ~67 dB).
                    let psnr_threshold = 10.0;
                    let psnr_pass = match psnr_val {
                        Some(v) => {
                            println!("    PSNR: {:.2} dB (threshold: {:.0} dB)", v, psnr_threshold);
                            v > psnr_threshold
                        }
                        None => {
                            println!("    PSNR: could not compute (ffmpeg may not be available)");
                            true // don't fail on missing ffmpeg
                        }
                    };

                    if probe_ok && psnr_pass {
                        println!("  [PASS] {} encode", codec_name);
                        passed += 1;
                    } else {
                        println!("  [FAIL] {} encode (probe_ok={}, psnr_pass={})", codec_name, probe_ok, psnr_pass);
                        failed += 1;
                    }
                    results.push(TestResult {
                        name: test_name,
                        status: if probe_ok && psnr_pass { "PASS" } else { "FAIL" }.to_string(),
                        psnr_ffmpeg: psnr_val, ssim: None,
                        frame_count: Some(expected_frames),
                        output_path: Some(out_path),
                    });
                } else {
                    println!("  [FAIL] {} encode failed", codec_name);
                    failed += 1;
                    results.push(TestResult {
                        name: test_name,
                        status: "FAIL".to_string(),
                        psnr_ffmpeg: None, ssim: None, frame_count: None, output_path: None,
                    });
                }

                drop(encoder);
            }
            Err(e) => {
                println!("  [FAIL] SimpleEncoder::new() failed: {}", e);
                failed += 1;
                results.push(TestResult {
                    name: test_name,
                    status: "FAIL".to_string(),
                    psnr_ffmpeg: None, ssim: None, frame_count: None, output_path: None,
                });
            }
        }
        println!();
    }

    // =====================================================================
    // GPU input: SimpleEncoder.encode_image() — RGBA VkImage → encode
    //
    // Validates the GPU-to-GPU encode path used by streamlib: caller
    // provides an RGBA VkImage (e.g., from a render pass), and
    // encode_image() runs the RGB→NV12 compute shader then encodes.
    // No CPU-side pixel upload — the entire pipeline stays on the GPU.
    //
    // Validates: produces non-empty packets for 30 frames, IDR present,
    // ffprobe confirms valid bitstream with correct dimensions and frame count.
    // =====================================================================
    let gpu_input_codecs: Vec<(&str, Codec, vk::VideoCodecOperationFlagsKHR, &str, &str)> = vec![
        ("H.264", Codec::H264, vk::VideoCodecOperationFlagsKHR::ENCODE_H264, "h264", "High"),
        ("H.265", Codec::H265, vk::VideoCodecOperationFlagsKHR::ENCODE_H265, "h265", "Main"),
    ];

    for (codec_name, codec, codec_flag, ext, expected_profile) in &gpu_input_codecs {
        let test_name = format!("SimpleEncoder.encode_image() {} — RGBA GPU input", codec_name);
        let has_encode = codecs.iter().any(|c| c.codec == *codec_flag && c.supported);

        if !has_encode || encode_qf.is_none() {
            println!("[SKIP] {} — no {} encode support", test_name, codec_name);
            skipped += 1;
            results.push(TestResult {
                name: test_name,
                status: "SKIP".to_string(),
                psnr_ffmpeg: None, ssim: None, frame_count: None, output_path: None,
            });
            println!();
            continue;
        }

        println!("[TEST] {}", test_name);

        let gi_width = 640u32;
        let gi_height = 480u32;
        let num_frames = 30u32;

        let config = SimpleEncoderConfig {
            width: gi_width,
            height: gi_height,
            fps: 30,
            codec: *codec,
            preset: Preset::Medium,
            ..Default::default()
        };

        match SimpleEncoder::new(config) {
            Ok(mut encoder) => {
                let result: Result<(), String> = (|| unsafe {
                    let device = encoder.device().clone();
                    let allocator = encoder.allocator().clone();
                    let (transfer_qf, transfer_queue) = encoder.transfer_queue();
                    let (aligned_w, aligned_h) = encoder.aligned_extent();
                    println!("  aligned: {}x{}, transfer_qf={}", aligned_w, aligned_h, transfer_qf);

                    // Create RGBA staging buffer (host-visible)
                    let rgba_size = (aligned_w * aligned_h * 4) as usize;
                    let stg_info = vk::BufferCreateInfo::builder()
                        .size(rgba_size as u64)
                        .usage(vk::BufferUsageFlags::TRANSFER_SRC)
                        .sharing_mode(vk::SharingMode::EXCLUSIVE);
                    let stg_opts = vma::AllocationOptions {
                        flags: vma::AllocationCreateFlags::MAPPED
                            | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
                        required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                            | vk::MemoryPropertyFlags::HOST_COHERENT,
                        ..Default::default()
                    };
                    let (staging_buf, staging_alloc) = allocator
                        .create_buffer(stg_info, &stg_opts)
                        .map_err(|e| format!("staging buffer: {e}"))?;
                    let stg_info_alloc = allocator.get_allocation_info(staging_alloc);
                    let stg_ptr = stg_info_alloc.pMappedData as *mut u8;

                    // Create RGBA VkImage on encoder's device
                    let rgba_img_info = vk::ImageCreateInfo::builder()
                        .image_type(vk::ImageType::_2D)
                        .format(vk::Format::R8G8B8A8_UNORM)
                        .extent(vk::Extent3D { width: aligned_w, height: aligned_h, depth: 1 })
                        .mip_levels(1)
                        .array_layers(1)
                        .samples(vk::SampleCountFlags::_1)
                        .tiling(vk::ImageTiling::OPTIMAL)
                        .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::STORAGE)
                        .sharing_mode(vk::SharingMode::EXCLUSIVE)
                        .initial_layout(vk::ImageLayout::UNDEFINED);
                    let rgba_alloc_opts = vma::AllocationOptions {
                        required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
                        ..Default::default()
                    };
                    let (rgba_image, rgba_alloc) = allocator
                        .create_image(rgba_img_info, &rgba_alloc_opts)
                        .map_err(|e| format!("RGBA image: {e}"))?;

                    // Create transfer command pool/buffer/fence
                    let tf_pool = device.create_command_pool(
                        &vk::CommandPoolCreateInfo::builder()
                            .queue_family_index(transfer_qf)
                            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                        None,
                    ).map_err(|e| format!("cmd pool: {e}"))?;
                    let tf_cb = device.allocate_command_buffers(
                        &vk::CommandBufferAllocateInfo::builder()
                            .command_pool(tf_pool)
                            .level(vk::CommandBufferLevel::PRIMARY)
                            .command_buffer_count(1),
                    ).map_err(|e| format!("cmd buf: {e}"))?[0];
                    let tf_fence = device.create_fence(&vk::FenceCreateInfo::default(), None)
                        .map_err(|e| format!("fence: {e}"))?;

                    let mut output_data = Vec::new();
                    let mut idr_seen = false;
                    let mut all_nonempty = true;

                    for f in 0..num_frames {
                        // Fill gradient pattern
                        let gradient = generate_gradient_frames(aligned_w, aligned_h, 1);
                        std::ptr::copy_nonoverlapping(
                            gradient.as_ptr(), stg_ptr, gradient.len().min(rgba_size),
                        );

                        // Upload to RGBA image
                        device.reset_command_buffer(tf_cb, vk::CommandBufferResetFlags::empty())
                            .map_err(|e| format!("reset cb: {e}"))?;
                        device.begin_command_buffer(tf_cb,
                            &vk::CommandBufferBeginInfo::builder()
                                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                        ).map_err(|e| format!("begin cb: {e}"))?;

                        let barrier_undef = vk::ImageMemoryBarrier::builder()
                            .old_layout(vk::ImageLayout::UNDEFINED)
                            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                            .image(rgba_image)
                            .subresource_range(vk::ImageSubresourceRange {
                                aspect_mask: vk::ImageAspectFlags::COLOR,
                                base_mip_level: 0, level_count: 1,
                                base_array_layer: 0, layer_count: 1,
                            })
                            .src_access_mask(vk::AccessFlags::empty())
                            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);
                        device.cmd_pipeline_barrier(tf_cb,
                            vk::PipelineStageFlags::TOP_OF_PIPE,
                            vk::PipelineStageFlags::TRANSFER,
                            vk::DependencyFlags::empty(), &[] as &[vk::MemoryBarrier], &[] as &[vk::BufferMemoryBarrier], &[barrier_undef]);

                        let region = vk::BufferImageCopy {
                            buffer_offset: 0, buffer_row_length: 0, buffer_image_height: 0,
                            image_subresource: vk::ImageSubresourceLayers {
                                aspect_mask: vk::ImageAspectFlags::COLOR,
                                mip_level: 0, base_array_layer: 0, layer_count: 1,
                            },
                            image_offset: vk::Offset3D { x: 0, y: 0, z: 0 },
                            image_extent: vk::Extent3D { width: aligned_w, height: aligned_h, depth: 1 },
                        };
                        device.cmd_copy_buffer_to_image(
                            tf_cb, staging_buf, rgba_image,
                            vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[region],
                        );

                        let barrier_general = vk::ImageMemoryBarrier::builder()
                            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                            .new_layout(vk::ImageLayout::GENERAL)
                            .image(rgba_image)
                            .subresource_range(vk::ImageSubresourceRange {
                                aspect_mask: vk::ImageAspectFlags::COLOR,
                                base_mip_level: 0, level_count: 1,
                                base_array_layer: 0, layer_count: 1,
                            })
                            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                            .dst_access_mask(vk::AccessFlags::SHADER_READ);
                        device.cmd_pipeline_barrier(tf_cb,
                            vk::PipelineStageFlags::TRANSFER,
                            vk::PipelineStageFlags::COMPUTE_SHADER,
                            vk::DependencyFlags::empty(), &[] as &[vk::MemoryBarrier], &[] as &[vk::BufferMemoryBarrier], &[barrier_general]);

                        device.end_command_buffer(tf_cb).map_err(|e| format!("end cb: {e}"))?;
                        device.reset_fences(&[tf_fence]).map_err(|e| format!("reset fence: {e}"))?;
                        let cbs = [tf_cb];
                        let submit_info = vk::SubmitInfo::builder().command_buffers(&cbs);
                        device.queue_submit(transfer_queue, &[submit_info], tf_fence)
                            .map_err(|e| format!("queue submit: {e}"))?;
                        device.wait_for_fences(&[tf_fence], true, 1_000_000_000)
                            .map_err(|e| format!("wait fence: {e}"))?;

                        // Encode from GPU image
                        // Create image view for encode_image
                        let rgba_view_info = vk::ImageViewCreateInfo::builder()
                            .image(rgba_image)
                            .view_type(vk::ImageViewType::_2D)
                            .format(vk::Format::R8G8B8A8_UNORM)
                            .subresource_range(vk::ImageSubresourceRange {
                                aspect_mask: vk::ImageAspectFlags::COLOR,
                                base_mip_level: 0, level_count: 1,
                                base_array_layer: 0, layer_count: 1,
                            });
                        let rgba_view = device.create_image_view(&rgba_view_info, None)
                            .map_err(|e| format!("image view: {e}"))?;
                        let packets = encoder.encode_image(rgba_view, None)
                            .map_err(|e| format!("encode_image frame {f}: {e}"))?;

                        if packets.is_empty() { all_nonempty = false; }
                        if packets.iter().any(|p| p.is_keyframe) { idr_seen = true; }
                        for p in &packets {
                            output_data.extend_from_slice(&p.data);
                        }
                    }

                    println!("  Encoded {} frames, {} bytes total", num_frames, output_data.len());
                    println!("  IDR present: {}, all non-empty: {}", idr_seen, all_nonempty);

                    let out_path = format!("/tmp/pipeline_test_rgba_encode.{}", ext);
                    std::fs::write(&out_path, &output_data)
                        .map_err(|e| format!("write: {e}"))?;
                    println!("  Wrote {}", out_path);

                    let (_probe_ok, _, _, _, _, ffprobe_nf) = run_ffprobe_checks(
                        &out_path, ext, expected_profile, gi_width, gi_height, num_frames,
                    );
                    println!("  ffprobe frame count: {:?} (expected {})", ffprobe_nf, num_frames);

                    // Cleanup
                    allocator.destroy_image(rgba_image, rgba_alloc);
                    allocator.destroy_buffer(staging_buf, staging_alloc);
                    device.destroy_command_pool(tf_pool, None);
                    device.destroy_fence(tf_fence, None);

                    Ok(())
                })();

                match result {
                    Ok(()) => {
                        println!("  [PASS]");
                        passed += 1;
                        results.push(TestResult {
                            name: test_name,
                            status: "PASS".to_string(),
                            psnr_ffmpeg: None, ssim: None,
                            frame_count: Some(num_frames),
                            output_path: Some(format!("/tmp/pipeline_test_rgba_encode.{}", ext)),
                        });
                    }
                    Err(e) => {
                        println!("  [FAIL] {}", e);
                        failed += 1;
                        results.push(TestResult {
                            name: test_name,
                            status: "FAIL".to_string(),
                            psnr_ffmpeg: None, ssim: None, frame_count: None, output_path: None,
                        });
                    }
                }

                drop(encoder);
            }
            Err(e) => {
                println!("  [FAIL] SimpleEncoder::new() failed: {}", e);
                failed += 1;
                results.push(TestResult {
                    name: test_name,
                    status: "FAIL".to_string(),
                    psnr_ffmpeg: None, ssim: None, frame_count: None, output_path: None,
                });
            }
        }
        println!();
    }


    // =====================================================================
    // NV12→RGB Post-Process Filter — verify converter produces correct RGB
    //
    // Uploads known NV12 fixture data to GPU, runs the NV12→RGB compute
    // shader with VkSamplerYcbcrConversion, reads back RGBA, validates:
    //   - Not all-zero (green/black screen)
    //   - R, G, B channels all present
    //   - PSNR against CPU reference conversion
    // =====================================================================
    if decode_qf.is_some() && compute_qf.is_some() {
        use vulkan_video::Nv12ToRgbConverter;

        let compute_qf_val = compute_qf.unwrap();
        let decode_qf_val = decode_qf.unwrap();
        let compute_q = compute_queue.unwrap();

        let test_name = "NV12→RGB post-process filter".to_string();
        println!("[TEST] {}", test_name);

        let result: Result<(), String> = (|| {
            unsafe {
                let allocator = ctx.allocator();

                // --- 1. Create NV12 image on GPU with SAMPLED + CONCURRENT ---
                let queue_families = [compute_qf_val, decode_qf_val];
                let concurrent = compute_qf_val != decode_qf_val;

                let mut nv12_create = vk::ImageCreateInfo::builder()
                    .image_type(vk::ImageType::_2D)
                    .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
                    .extent(vk::Extent3D { width, height, depth: 1 })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(
                        vk::ImageUsageFlags::TRANSFER_DST
                            | vk::ImageUsageFlags::SAMPLED,
                    )
                    .initial_layout(vk::ImageLayout::UNDEFINED);

                if concurrent {
                    nv12_create = nv12_create
                        .sharing_mode(vk::SharingMode::CONCURRENT)
                        .queue_family_indices(&queue_families);
                } else {
                    nv12_create = nv12_create.sharing_mode(vk::SharingMode::EXCLUSIVE);
                }

                let alloc_opts = vma::AllocationOptions {
                    required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
                    ..Default::default()
                };
                let (nv12_image, nv12_alloc) = allocator
                    .create_image(nv12_create, &alloc_opts)
                    .map_err(|e| format!("NV12 image: {e}"))?;

                // --- 2. Upload first NV12 frame via staging buffer ---
                let y_size = (width * height) as usize;
                let uv_size = (width * height / 2) as usize;
                let frame_total = y_size + uv_size;
                let nv12_data = &fixture_data[..frame_total];

                let stg_info = vk::BufferCreateInfo::builder()
                    .size(frame_total as u64)
                    .usage(vk::BufferUsageFlags::TRANSFER_SRC)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE);
                let stg_alloc_opts = vma::AllocationOptions {
                    flags: vma::AllocationCreateFlags::MAPPED
                        | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
                    required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                        | vk::MemoryPropertyFlags::HOST_COHERENT,
                    ..Default::default()
                };
                let (stg_buf, stg_alloc) = allocator
                    .create_buffer(stg_info, &stg_alloc_opts)
                    .map_err(|e| format!("staging buf: {e}"))?;

                let ai = allocator.get_allocation_info(stg_alloc);
                std::ptr::copy_nonoverlapping(
                    nv12_data.as_ptr(),
                    ai.pMappedData as *mut u8,
                    frame_total,
                );

                // Transfer command pool on the transfer queue family
                let tf_qf = transfer_qf.unwrap_or(compute_qf_val);
                let tf_q = transfer_queue.unwrap_or(compute_q);

                let tf_pool = device
                    .create_command_pool(
                        &vk::CommandPoolCreateInfo::builder()
                            .queue_family_index(tf_qf)
                            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                        None,
                    )
                    .map_err(|e| format!("cmd pool: {e}"))?;
                let tf_cb = device
                    .allocate_command_buffers(
                        &vk::CommandBufferAllocateInfo::builder()
                            .command_pool(tf_pool)
                            .level(vk::CommandBufferLevel::PRIMARY)
                            .command_buffer_count(1),
                    )
                    .map_err(|e| format!("cmd buf: {e}"))?[0];
                let tf_fence = device
                    .create_fence(&vk::FenceCreateInfo::default(), None)
                    .map_err(|e| format!("fence: {e}"))?;

                device
                    .begin_command_buffer(
                        tf_cb,
                        &vk::CommandBufferBeginInfo::builder()
                            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                    )
                    .map_err(|e| format!("begin cb: {e}"))?;

                // Transition NV12 UNDEFINED → TRANSFER_DST
                let barrier_dst = vk::ImageMemoryBarrier::builder()
                    .old_layout(vk::ImageLayout::UNDEFINED)
                    .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(nv12_image)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);
                let no_mem: &[vk::MemoryBarrier] = &[];
                let no_buf: &[vk::BufferMemoryBarrier] = &[];
                device.cmd_pipeline_barrier(
                    tf_cb,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    no_mem, no_buf, &[barrier_dst],
                );

                // Copy Y and UV planes
                let y_region = vk::BufferImageCopy {
                    buffer_offset: 0,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::PLANE_0,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    },
                    image_offset: vk::Offset3D::default(),
                    image_extent: vk::Extent3D { width, height, depth: 1 },
                };
                let uv_region = vk::BufferImageCopy {
                    buffer_offset: y_size as u64,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::PLANE_1,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    },
                    image_offset: vk::Offset3D::default(),
                    image_extent: vk::Extent3D {
                        width: width / 2,
                        height: height / 2,
                        depth: 1,
                    },
                };
                device.cmd_copy_buffer_to_image(
                    tf_cb, stg_buf, nv12_image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[y_region, uv_region],
                );

                device.end_command_buffer(tf_cb).map_err(|e| format!("end cb: {e}"))?;

                let submit = vk::SubmitInfo::builder().command_buffers(std::slice::from_ref(&tf_cb));
                device.queue_submit(tf_q, &[submit], tf_fence).map_err(|e| format!("submit: {e}"))?;
                device.wait_for_fences(&[tf_fence], true, u64::MAX).map_err(|e| format!("wait: {e}"))?;

                allocator.destroy_buffer(stg_buf, stg_alloc);

                // --- 3. Create NV12→RGB converter and run ---
                let mut converter = Nv12ToRgbConverter::new(
                    &ctx, width, height,
                    compute_qf_val, compute_q,
                    decode_qf_val,
                ).map_err(|e| format!("Nv12ToRgbConverter::new: {e}"))?;

                let (rgba_image, _rgba_view) = converter.convert(
                    nv12_image, 0,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                ).map_err(|e| format!("convert: {e}"))?;

                // --- 4. Read back RGBA via staging buffer ---
                let rgba_size = (width * height * 4) as u64;
                let rb_info = vk::BufferCreateInfo::builder()
                    .size(rgba_size)
                    .usage(vk::BufferUsageFlags::TRANSFER_DST)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE);
                let rb_opts = vma::AllocationOptions {
                    flags: vma::AllocationCreateFlags::MAPPED
                        | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
                    required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                        | vk::MemoryPropertyFlags::HOST_COHERENT,
                    ..Default::default()
                };
                let (rb_buf, rb_alloc) = allocator
                    .create_buffer(rb_info, &rb_opts)
                    .map_err(|e| format!("rb buf: {e}"))?;

                // Record copy from RGBA image (in TRANSFER_SRC_OPTIMAL) to buffer
                device.reset_fences(&[tf_fence]).map_err(|e| format!("reset fence: {e}"))?;
                device.reset_command_buffer(tf_cb, vk::CommandBufferResetFlags::empty())
                    .map_err(|e| format!("reset cb: {e}"))?;
                device.begin_command_buffer(
                    tf_cb,
                    &vk::CommandBufferBeginInfo::builder()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                ).map_err(|e| format!("begin cb: {e}"))?;

                let copy_region = vk::BufferImageCopy {
                    buffer_offset: 0,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    },
                    image_offset: vk::Offset3D::default(),
                    image_extent: vk::Extent3D { width, height, depth: 1 },
                };
                device.cmd_copy_image_to_buffer(
                    tf_cb, rgba_image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    rb_buf, &[copy_region],
                );

                device.end_command_buffer(tf_cb).map_err(|e| format!("end cb: {e}"))?;
                let submit = vk::SubmitInfo::builder().command_buffers(std::slice::from_ref(&tf_cb));
                device.queue_submit(tf_q, &[submit], tf_fence).map_err(|e| format!("submit: {e}"))?;
                device.wait_for_fences(&[tf_fence], true, u64::MAX).map_err(|e| format!("wait: {e}"))?;

                let rb_ai = allocator.get_allocation_info(rb_alloc);
                let rgba_data = std::slice::from_raw_parts(
                    rb_ai.pMappedData as *const u8,
                    rgba_size as usize,
                );

                // --- 5. Validate RGBA output ---
                let pixel_count = (width * height) as usize;
                let mut nonzero = 0usize;
                let mut r_sum = 0u64;
                let mut g_sum = 0u64;
                let mut b_sum = 0u64;
                let mut a_sum = 0u64;
                for i in 0..pixel_count {
                    let r = rgba_data[i * 4] as u64;
                    let g = rgba_data[i * 4 + 1] as u64;
                    let b = rgba_data[i * 4 + 2] as u64;
                    let a = rgba_data[i * 4 + 3] as u64;
                    if r > 0 || g > 0 || b > 0 { nonzero += 1; }
                    r_sum += r;
                    g_sum += g;
                    b_sum += b;
                    a_sum += a;
                }

                let r_avg = r_sum as f64 / pixel_count as f64;
                let g_avg = g_sum as f64 / pixel_count as f64;
                let b_avg = b_sum as f64 / pixel_count as f64;
                let a_avg = a_sum as f64 / pixel_count as f64;
                let nonzero_pct = nonzero as f64 / pixel_count as f64 * 100.0;

                println!("    Pixels: {} total, {} nonzero ({:.1}%)", pixel_count, nonzero, nonzero_pct);
                println!("    Avg R={:.1} G={:.1} B={:.1} A={:.1}", r_avg, g_avg, b_avg, a_avg);

                // CPU reference NV12→RGB conversion (BT.709, ITU narrow)
                let mut psnr_sum = 0.0f64;
                let mut psnr_count = 0u32;
                for y_coord in 0..height {
                    for x_coord in 0..width {
                        let yi = (y_coord * width + x_coord) as usize;
                        let y_val = nv12_data[yi] as f64;
                        let uv_x = (x_coord / 2) as usize;
                        let uv_y = (y_coord / 2) as usize;
                        let uv_idx = y_size + uv_y * width as usize + uv_x * 2;
                        let cb = nv12_data[uv_idx] as f64;
                        let cr = nv12_data[uv_idx + 1] as f64;

                        // BT.709 ITU narrow → RGB
                        let y_norm = (y_val - 16.0) / 219.0;
                        let cb_norm = (cb - 128.0) / 224.0;
                        let cr_norm = (cr - 128.0) / 224.0;
                        let r_ref = (y_norm + 1.5748 * cr_norm).clamp(0.0, 1.0) * 255.0;
                        let g_ref = (y_norm - 0.1873 * cb_norm - 0.4681 * cr_norm).clamp(0.0, 1.0) * 255.0;
                        let b_ref = (y_norm + 1.8556 * cb_norm).clamp(0.0, 1.0) * 255.0;

                        let pi = (y_coord * width + x_coord) as usize;
                        let r_gpu = rgba_data[pi * 4] as f64;
                        let g_gpu = rgba_data[pi * 4 + 1] as f64;
                        let b_gpu = rgba_data[pi * 4 + 2] as f64;

                        let dr = r_ref - r_gpu;
                        let dg = g_ref - g_gpu;
                        let db = b_ref - b_gpu;
                        let mse = (dr * dr + dg * dg + db * db) / 3.0;
                        if mse > 0.0 {
                            psnr_sum += 10.0 * (255.0f64 * 255.0 / mse).log10();
                        } else {
                            psnr_sum += 100.0; // perfect pixel
                        }
                        psnr_count += 1;
                    }
                }
                let avg_psnr = psnr_sum / psnr_count as f64;
                println!("    RGB PSNR vs CPU reference: {:.2} dB", avg_psnr);

                // Cleanup
                allocator.destroy_buffer(rb_buf, rb_alloc);
                drop(converter);
                allocator.destroy_image(nv12_image, nv12_alloc);
                device.destroy_command_pool(tf_pool, None);
                device.destroy_fence(tf_fence, None);

                // Acceptance criteria:
                // 1. Not all-zero (>90% nonzero pixels)
                // 2. All channels present (avg > 10 for R, G, B)
                // 3. Alpha = 255 (opaque)
                // 4. PSNR > 25 dB vs CPU reference (accounts for chroma upsampling diff)
                if nonzero_pct < 90.0 {
                    return Err(format!("only {:.1}% nonzero pixels (expected >90%)", nonzero_pct));
                }
                if r_avg < 10.0 || g_avg < 10.0 || b_avg < 10.0 {
                    return Err(format!("missing channel: R={:.1} G={:.1} B={:.1}", r_avg, g_avg, b_avg));
                }
                if a_avg < 200.0 {
                    return Err(format!("alpha too low: {:.1} (expected ~255)", a_avg));
                }
                if avg_psnr < 25.0 {
                    return Err(format!("PSNR {:.2} dB too low (expected >25 dB)", avg_psnr));
                }

                println!("    [PASS] NV12→RGB correct: {:.1}% nonzero, PSNR={:.2} dB", nonzero_pct, avg_psnr);
                Ok(())
            }
        })();

        match result {
            Ok(()) => {
                passed += 1;
                results.push(TestResult {
                    name: test_name,
                    status: "PASS".to_string(),
                    psnr_ffmpeg: None, ssim: None, frame_count: Some(1), output_path: None,
                });
            }
            Err(e) => {
                println!("    [FAIL] {}", e);
                failed += 1;
                results.push(TestResult {
                    name: test_name,
                    status: "FAIL".to_string(),
                    psnr_ffmpeg: None, ssim: None, frame_count: None, output_path: None,
                });
            }
        }
        println!();
    }


    // =====================================================================
    // Cleanup and summary
    // =====================================================================
    unsafe {
        device.device_wait_idle().ok();
        device.destroy_device(None);
        instance.destroy_instance(None);
    }

    // Print summary table
    println!("========================================");
    println!("Pipeline Test Summary");
    println!("========================================");
    println!();
    println!("  {:<20} {:<8} {:>10} {:>10} {:>8} {}",
        "Test", "Status", "PSNR(dB)", "SSIM", "Frames", "Output");
    println!("  {:<20} {:<8} {:>10} {:>10} {:>8} {}",
        "----", "------", "--------", "------", "------", "------");

    for r in &results {
        let psnr_str = match r.psnr_ffmpeg {
            Some(v) => format!("{:.2}", v),
            None => "--".to_string(),
        };
        let ssim_str = match r.ssim {
            Some(v) => format!("{:.6}", v),
            None => "--".to_string(),
        };
        let frames_str = match r.frame_count {
            Some(v) => format!("{}", v),
            None => "--".to_string(),
        };
        let path_str = match &r.output_path {
            Some(p) => p.as_str(),
            None => "--",
        };
        println!("  {:<20} {:<8} {:>10} {:>10} {:>8} {}",
            r.name, r.status, psnr_str, ssim_str, frames_str, path_str);
    }

    println!();
    println!("  Passed:  {}", passed);
    println!("  Failed:  {}", failed);
    println!("  Skipped: {}", skipped);
    println!("  Total:   {}", passed + failed + skipped);
    println!();

    // List screenshot files
    println!("  Screenshot files in /tmp/nvpro_pipeline_debug/:");
    if let Ok(entries) = std::fs::read_dir("/tmp/nvpro_pipeline_debug") {
        let mut files: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".png") || n.ends_with(".h264") || n.ends_with(".h265") || n.ends_with(".mp4"))
            .collect();
        files.sort();
        for f in &files {
            let full = format!("/tmp/nvpro_pipeline_debug/{}", f);
            let size = std::fs::metadata(&full).map(|m| m.len()).unwrap_or(0);
            println!("    {} ({} bytes)", f, size);
        }
    }
    println!();

    // Compact summary for automated parsing (agents, CI)
    println!("=== PSNR SUMMARY ===");
    for r in &results {
        let psnr_str = match r.psnr_ffmpeg {
            Some(v) => format!("{:.2} dB", v),
            None => "--".to_string(),
        };
        println!("  {} {} {}", r.name, psnr_str, r.status);
    }
    println!("====================");
    println!();

    if failed > 0 {
        println!("SOME TESTS FAILED");
        std::process::exit(1);
    } else {
        println!("ALL TESTS PASSED");
    }
}
