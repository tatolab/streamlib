// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration test: validate the vulkan-video encode/decode pipeline when
//! using the `from_device()` constructors with a shared Vulkan device.
//!
//! This test creates its own Vulkan instance, device, and VMA allocator
//! (mirroring what streamlib's VulkanDevice would provide), then exercises
//! the `SimpleEncoder::from_device()` and `SimpleDecoder::from_device()`
//! paths to confirm the shared-device integration works end-to-end.
//!
//! Requires a GPU with H.265 encode + decode support. Marked `#[ignore]`
//! so it does not run in `cargo test` by default.
//!
//! Run with:
//!   cargo test -p vulkan-video --test shared_device_test -- --ignored --nocapture

use std::ffi::CStr;
use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

use vulkan_video::{
    SimpleEncoder, SimpleEncoderConfig, Codec, Preset,
    SimpleDecoder, SimpleDecoderConfig,
    decode::DpbOutputMode,
};

// ---------------------------------------------------------------------------
// NV12 test pattern generation
// ---------------------------------------------------------------------------

fn generate_nv12_gradient(width: u32, height: u32, frame_idx: u32) -> Vec<u8> {
    let y_size = (width * height) as usize;
    let uv_size = (width * height / 2) as usize;
    let mut data = vec![0u8; y_size + uv_size];

    // Y plane: horizontal gradient with frame-varying offset
    for row in 0..height {
        for col in 0..width {
            let y = ((col as f32 / width as f32 * 200.0) + frame_idx as f32 * 10.0) as u8;
            data[(row * width + col) as usize] = y;
        }
    }

    // UV plane: centered chroma
    for i in 0..uv_size {
        data[y_size + i] = 128;
    }

    data
}

// ---------------------------------------------------------------------------
// PSNR computation (Y-plane only)
// ---------------------------------------------------------------------------

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
// Queue family discovery
// ---------------------------------------------------------------------------

fn find_queue_family(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    required: vk::QueueFlags,
) -> Option<u32> {
    let props =
        unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
    for (i, p) in props.iter().enumerate() {
        if p.queue_flags.contains(required) {
            return Some(i as u32);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// ffprobe validation
// ---------------------------------------------------------------------------

/// Run ffprobe on an encoded file, return (codec_name, width, height, nb_read_frames).
fn run_ffprobe(path: &str) -> Option<(String, u32, u32, u32)> {
    let output = std::process::Command::new("ffprobe")
        .args([
            "-v", "error",
            "-count_frames",
            "-select_streams", "v:0",
            "-show_entries", "stream=codec_name,width,height,nb_read_frames",
            "-of", "json",
            path,
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let json = String::from_utf8_lossy(&output.stdout);

    let get_str = |key: &str| -> Option<String> {
        let needle = format!("\"{}\"", key);
        let pos = json.find(&needle)?;
        let rest = &json[pos + needle.len()..];
        let colon = rest.find(':')?;
        let after_colon = rest[colon + 1..].trim_start();
        if after_colon.starts_with('"') {
            let end = after_colon[1..].find('"')? + 1;
            Some(after_colon[1..end].to_string())
        } else {
            let end = after_colon
                .find(|c: char| c == ',' || c == '}' || c == '\n')
                .unwrap_or(after_colon.len());
            Some(after_colon[..end].trim().trim_matches('"').to_string())
        }
    };

    let codec_name = get_str("codec_name")?;
    let width = get_str("width")?.parse::<u32>().ok()?;
    let height = get_str("height")?.parse::<u32>().ok()?;
    let nb_frames = get_str("nb_read_frames")?.parse::<u32>().ok()?;

    Some((codec_name, width, height, nb_frames))
}

// ---------------------------------------------------------------------------
// Codec capability probing
// ---------------------------------------------------------------------------

unsafe fn probe_encode_support(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    codec: vk::VideoCodecOperationFlagsKHR,
) -> bool {
    use vulkanalia::vk::KhrVideoQueueExtensionInstanceCommands;

    let mut h265_profile = vk::VideoEncodeH265ProfileInfoKHR::builder()
        .std_profile_idc(vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN);
    let mut h264_profile = vk::VideoEncodeH264ProfileInfoKHR::builder()
        .std_profile_idc(vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH);

    let mut profile_info = vk::VideoProfileInfoKHR::builder()
        .video_codec_operation(codec)
        .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::_420)
        .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8)
        .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8);

    if codec == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
        profile_info = profile_info.push_next(&mut h265_profile);
    } else if codec == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
        profile_info = profile_info.push_next(&mut h264_profile);
    }

    let mut h265_encode_caps = vk::VideoEncodeH265CapabilitiesKHR::default();
    let mut h264_encode_caps = vk::VideoEncodeH264CapabilitiesKHR::default();
    let mut encode_caps = vk::VideoEncodeCapabilitiesKHR::default();
    let mut caps = vk::VideoCapabilitiesKHR::default();

    if codec == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
        encode_caps.next = &mut h265_encode_caps as *mut _ as *mut std::ffi::c_void;
    } else if codec == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
        encode_caps.next = &mut h264_encode_caps as *mut _ as *mut std::ffi::c_void;
    }
    caps.next = &mut encode_caps as *mut _ as *mut std::ffi::c_void;

    instance
        .get_physical_device_video_capabilities_khr(physical_device, &profile_info, &mut caps)
        .is_ok()
}

unsafe fn probe_decode_support(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    codec: vk::VideoCodecOperationFlagsKHR,
) -> bool {
    use vulkanalia::vk::KhrVideoQueueExtensionInstanceCommands;

    let mut h265_profile = vk::VideoDecodeH265ProfileInfoKHR::builder()
        .std_profile_idc(vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN);
    let mut h264_profile = vk::VideoDecodeH264ProfileInfoKHR::builder()
        .std_profile_idc(vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH)
        .picture_layout(vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE);

    let mut profile_info = vk::VideoProfileInfoKHR::builder()
        .video_codec_operation(codec)
        .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::_420)
        .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8)
        .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::_8);

    if codec == vk::VideoCodecOperationFlagsKHR::DECODE_H265 {
        profile_info = profile_info.push_next(&mut h265_profile);
    } else if codec == vk::VideoCodecOperationFlagsKHR::DECODE_H264 {
        profile_info = profile_info.push_next(&mut h264_profile);
    }

    let mut h265_decode_caps = vk::VideoDecodeH265CapabilitiesKHR::default();
    let mut h264_decode_caps = vk::VideoDecodeH264CapabilitiesKHR::default();
    let mut decode_caps = vk::VideoDecodeCapabilitiesKHR::default();
    let mut caps = vk::VideoCapabilitiesKHR::default();

    if codec == vk::VideoCodecOperationFlagsKHR::DECODE_H265 {
        decode_caps.next = &mut h265_decode_caps as *mut _ as *mut std::ffi::c_void;
    } else if codec == vk::VideoCodecOperationFlagsKHR::DECODE_H264 {
        decode_caps.next = &mut h264_decode_caps as *mut _ as *mut std::ffi::c_void;
    }
    caps.next = &mut decode_caps as *mut _ as *mut std::ffi::c_void;

    instance
        .get_physical_device_video_capabilities_khr(physical_device, &profile_info, &mut caps)
        .is_ok()
}

// ---------------------------------------------------------------------------
// Check if a device extension is available
// ---------------------------------------------------------------------------

unsafe fn has_device_extension(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    name: &CStr,
) -> bool {
    let available = instance
        .enumerate_device_extension_properties(physical_device, None)
        .unwrap_or_default();
    available.iter().any(|ext| {
        let ext_name = CStr::from_ptr(ext.extension_name.as_ptr());
        ext_name == name
    })
}

// ===========================================================================
// Test: H.265 encode -> ffprobe validate -> decode -> PSNR (shared device)
// ===========================================================================

#[test]
#[ignore] // Requires a GPU with H.265 encode + decode support
fn h265_shared_device_encode_decode_roundtrip() {
    // -----------------------------------------------------------------------
    // 1. Load Vulkan
    // -----------------------------------------------------------------------
    let loader = match unsafe {
        vulkanalia::loader::LibloadingLoader::new(vulkanalia::loader::LIBRARY)
    } {
        Ok(l) => l,
        Err(e) => {
            eprintln!("No Vulkan loader available: {}. Skipping.", e);
            return;
        }
    };

    let entry = match unsafe { vulkanalia::Entry::new(loader) } {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Failed to create Vulkan entry: {}. Skipping.", e);
            return;
        }
    };

    // -----------------------------------------------------------------------
    // 2. Create instance
    // -----------------------------------------------------------------------
    let app_info = vk::ApplicationInfo::builder()
        .application_name(b"shared-device-test\0")
        .api_version(vk::make_version(1, 4, 0));

    let instance = match unsafe {
        entry.create_instance(
            &vk::InstanceCreateInfo::builder().application_info(&app_info),
            None,
        )
    } {
        Ok(i) => i,
        Err(e) => {
            eprintln!("Failed to create Vulkan instance: {:?}. Skipping.", e);
            return;
        }
    };

    // -----------------------------------------------------------------------
    // 3. Find a physical device with encode + decode + transfer + compute
    // -----------------------------------------------------------------------
    let physical_devices = match unsafe { instance.enumerate_physical_devices() } {
        Ok(devs) if !devs.is_empty() => devs,
        _ => {
            eprintln!("No Vulkan physical devices. Skipping.");
            unsafe { instance.destroy_instance(None) };
            return;
        }
    };

    let mut selected_physical_device = None;
    let mut encode_qf = 0u32;
    let mut decode_qf = 0u32;
    let mut transfer_qf = 0u32;
    let mut compute_qf = 0u32;

    for &pd in &physical_devices {
        // Reject software renderers
        if vulkan_video::reject_software_renderer(&instance, pd).is_err() {
            continue;
        }

        let enc = find_queue_family(&instance, pd, vk::QueueFlags::VIDEO_ENCODE_KHR);
        let dec = find_queue_family(&instance, pd, vk::QueueFlags::VIDEO_DECODE_KHR);
        let tf = find_queue_family(&instance, pd, vk::QueueFlags::TRANSFER);
        let comp = find_queue_family(&instance, pd, vk::QueueFlags::COMPUTE);

        if let (Some(e), Some(d), Some(t), Some(c)) = (enc, dec, tf, comp) {
            // Probe actual H.265 encode + decode capability
            let h265_enc = unsafe {
                probe_encode_support(&instance, pd, vk::VideoCodecOperationFlagsKHR::ENCODE_H265)
            };
            let h265_dec = unsafe {
                probe_decode_support(&instance, pd, vk::VideoCodecOperationFlagsKHR::DECODE_H265)
            };

            if h265_enc && h265_dec {
                selected_physical_device = Some(pd);
                encode_qf = e;
                decode_qf = d;
                transfer_qf = t;
                compute_qf = c;
                break;
            }
        }
    }

    let physical_device = match selected_physical_device {
        Some(pd) => pd,
        None => {
            eprintln!("No GPU with H.265 encode + decode support. Skipping.");
            unsafe { instance.destroy_instance(None) };
            return;
        }
    };

    let props = unsafe { instance.get_physical_device_properties(physical_device) };
    let gpu_name = unsafe { CStr::from_ptr(props.device_name.as_ptr()) }
        .to_str()
        .unwrap_or("unknown");
    println!("GPU: {}", gpu_name);
    println!(
        "Queue families -- encode: {}, decode: {}, transfer: {}, compute: {}",
        encode_qf, decode_qf, transfer_qf, compute_qf
    );

    // -----------------------------------------------------------------------
    // 4. Create device with all required extensions
    // -----------------------------------------------------------------------
    let video_maint1_name =
        unsafe { CStr::from_bytes_with_nul_unchecked(b"VK_KHR_video_maintenance1\0") };

    let mut device_extensions: Vec<*const i8> = vec![
        vk::KHR_VIDEO_QUEUE_EXTENSION.name.as_ptr(),
        vk::KHR_VIDEO_ENCODE_QUEUE_EXTENSION.name.as_ptr(),
        vk::KHR_VIDEO_DECODE_QUEUE_EXTENSION.name.as_ptr(),
        vk::KHR_VIDEO_ENCODE_H265_EXTENSION.name.as_ptr(),
        vk::KHR_VIDEO_DECODE_H265_EXTENSION.name.as_ptr(),
        vk::KHR_SYNCHRONIZATION2_EXTENSION.name.as_ptr(),
        vk::KHR_PUSH_DESCRIPTOR_EXTENSION.name.as_ptr(),
    ];

    // Add video_maintenance1 only if available (encoder requires it)
    let has_maint1 =
        unsafe { has_device_extension(&instance, physical_device, video_maint1_name) };
    if has_maint1 {
        device_extensions.push(video_maint1_name.as_ptr());
    }

    device_extensions.sort();
    device_extensions.dedup();

    // Collect unique queue families
    let mut unique_families = vec![encode_qf, decode_qf, transfer_qf, compute_qf];
    unique_families.sort();
    unique_families.dedup();

    let queue_priorities = [1.0f32];
    let queue_create_infos: Vec<_> = unique_families
        .iter()
        .map(|&qf| {
            vk::DeviceQueueCreateInfo::builder()
                .queue_family_index(qf)
                .queue_priorities(&queue_priorities)
        })
        .collect();

    let mut sync2 =
        vk::PhysicalDeviceSynchronization2Features::builder().synchronization2(true);
    let mut video_maint1_feat =
        vk::PhysicalDeviceVideoMaintenance1FeaturesKHR::builder().video_maintenance1(true);

    let mut device_info = vk::DeviceCreateInfo::builder()
        .queue_create_infos(&queue_create_infos)
        .enabled_extension_names(&device_extensions)
        .push_next(&mut sync2);

    if has_maint1 {
        device_info = device_info.push_next(&mut video_maint1_feat);
    }

    let device = match unsafe { instance.create_device(physical_device, &device_info, None) } {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Device creation failed: {:?}. Skipping.", e);
            unsafe { instance.destroy_instance(None) };
            return;
        }
    };

    let encode_queue = unsafe { device.get_device_queue(encode_qf, 0) };
    let decode_queue = unsafe { device.get_device_queue(decode_qf, 0) };
    let transfer_queue = unsafe { device.get_device_queue(transfer_qf, 0) };
    let compute_queue = unsafe { device.get_device_queue(compute_qf, 0) };

    // -----------------------------------------------------------------------
    // 5. Create VMA allocator
    // -----------------------------------------------------------------------
    let mut alloc_options = vma::AllocatorOptions::new(&instance, &device, physical_device);
    alloc_options.version = vulkanalia::Version::new(1, 4, 0);
    let allocator = Arc::new(unsafe {
        vma::Allocator::new(&alloc_options).expect("Failed to create VMA allocator")
    });

    // -----------------------------------------------------------------------
    // 6. Encode 10 frames of H.265 via from_device()
    // -----------------------------------------------------------------------
    let width = 640u32;
    let height = 480u32;
    let num_frames = 10u32;

    let encoder_config = SimpleEncoderConfig {
        width,
        height,
        fps: 30,
        codec: Codec::H265,
        preset: Preset::Medium,
        streaming: true,
        idr_interval_secs: 2,
        ..Default::default()
    };

    let mut encoder = SimpleEncoder::from_device(
        encoder_config,
        instance.clone(),
        device.clone(),
        physical_device,
        allocator.clone(),
        encode_queue,
        encode_qf,
        transfer_queue,
        transfer_qf,
        compute_queue,
        compute_qf,
    )
    .expect("SimpleEncoder::from_device() failed");

    println!("SimpleEncoder created via from_device()");

    // Generate and encode frames
    let mut original_frames: Vec<Vec<u8>> = Vec::new();
    let mut encoded_bitstream: Vec<u8> = Vec::new();
    let mut total_packets = 0u32;
    let mut idr_count = 0u32;

    for frame_idx in 0..num_frames {
        let nv12_data = generate_nv12_gradient(width, height, frame_idx);
        original_frames.push(nv12_data.clone());

        let packets = encoder
            .submit_frame(&nv12_data, Some(frame_idx as i64 * 33_333_333))
            .unwrap_or_else(|e| panic!("Encode frame {} failed: {}", frame_idx, e));

        for packet in &packets {
            encoded_bitstream.extend_from_slice(&packet.data);
            total_packets += 1;
            if packet.is_keyframe {
                idr_count += 1;
            }
        }

        if frame_idx < 3 || frame_idx == num_frames - 1 {
            let pkt_bytes: usize = packets.iter().map(|p| p.data.len()).sum();
            let ft = packets.first().map(|p| p.frame_type.name()).unwrap_or("?");
            let kf = packets.iter().any(|p| p.is_keyframe);
            println!(
                "  Frame {:2}: {} {:>6} bytes{}",
                frame_idx,
                ft,
                pkt_bytes,
                if kf { " [IDR]" } else { "" }
            );
        } else if frame_idx == 3 {
            println!("  ...");
        }
    }

    // Flush trailing packets
    let trailing = encoder.finish().expect("finish() failed");
    for packet in &trailing {
        encoded_bitstream.extend_from_slice(&packet.data);
        total_packets += 1;
    }
    println!(
        "  finish(): {} trailing packets",
        trailing.len()
    );

    drop(encoder);

    println!(
        "Encoded {} packets ({} IDR), {} bytes total",
        total_packets,
        idr_count,
        encoded_bitstream.len()
    );

    assert!(
        !encoded_bitstream.is_empty(),
        "Encoded bitstream must not be empty"
    );
    assert!(idr_count >= 1, "At least one IDR frame expected");

    // -----------------------------------------------------------------------
    // 7. Write bitstream to temp file and validate with ffprobe
    // -----------------------------------------------------------------------
    let encoded_path = "/tmp/shared_device_test_h265.h265";
    std::fs::write(encoded_path, &encoded_bitstream)
        .expect("Failed to write encoded bitstream to temp file");

    if let Some((codec_name, probe_width, probe_height, nb_frames)) = run_ffprobe(encoded_path) {
        println!("ffprobe: codec={}, {}x{}, {} frames", codec_name, probe_width, probe_height, nb_frames);
        assert_eq!(codec_name, "hevc", "Expected hevc codec from ffprobe");
        assert_eq!(probe_width, width, "ffprobe width mismatch");
        assert_eq!(probe_height, height, "ffprobe height mismatch");
        assert_eq!(nb_frames, num_frames, "ffprobe frame count mismatch");
    } else {
        println!("ffprobe not available or failed -- skipping ffprobe validation");
    }

    // -----------------------------------------------------------------------
    // 8. Decode via SimpleDecoder::from_device()
    // -----------------------------------------------------------------------
    let decoder_config = SimpleDecoderConfig {
        codec: Codec::H265,
        max_width: 0,
        max_height: 0,
        output_mode: DpbOutputMode::Coincide,
    };

    let mut decoder = SimpleDecoder::from_device(
        decoder_config,
        instance.clone(),
        device.clone(),
        physical_device,
        allocator.clone(),
        decode_queue,
        decode_qf,
        transfer_queue,
        transfer_qf,
    )
    .expect("SimpleDecoder::from_device() failed");

    println!("SimpleDecoder created via from_device()");

    let decoded_frames = decoder
        .feed(&encoded_bitstream)
        .expect("SimpleDecoder::feed() failed");

    println!("Decoded {} frames", decoded_frames.len());

    assert!(
        !decoded_frames.is_empty(),
        "Decoder must produce at least one frame"
    );

    // -----------------------------------------------------------------------
    // 9. Compute PSNR between original and decoded frames
    // -----------------------------------------------------------------------
    let mut min_psnr = f64::MAX;
    let mut max_psnr = f64::MIN;
    let mut psnr_sum = 0.0f64;
    let mut psnr_count = 0u32;

    for (i, decoded) in decoded_frames.iter().enumerate() {
        assert_eq!(decoded.width, width, "Decoded frame {} width mismatch", i);
        assert_eq!(decoded.height, height, "Decoded frame {} height mismatch", i);

        // Match decoded frame to original by index (decode order may differ
        // from display order for B-frames, but this GOP is IP-only so they
        // should align).
        if i < original_frames.len() {
            let psnr = compute_y_psnr(&original_frames[i], &decoded.data, width, height);
            println!(
                "  decoded[{}] poc={} PSNR={:.2} dB",
                i, decoded.picture_order_count, psnr
            );

            if psnr < min_psnr {
                min_psnr = psnr;
            }
            if psnr > max_psnr {
                max_psnr = psnr;
            }
            psnr_sum += psnr;
            psnr_count += 1;
        }
    }

    drop(decoder);

    let avg_psnr = if psnr_count > 0 {
        psnr_sum / psnr_count as f64
    } else {
        0.0
    };

    println!(
        "PSNR summary: min={:.2} dB, max={:.2} dB, avg={:.2} dB ({} frames)",
        min_psnr, max_psnr, avg_psnr, psnr_count
    );

    // H.265 at CQP 18 on gradient content should easily exceed 30 dB
    assert!(
        min_psnr > 30.0,
        "Minimum PSNR {:.2} dB is below threshold of 30 dB. \
         The encode/decode roundtrip is introducing too much quality loss.",
        min_psnr
    );

    // -----------------------------------------------------------------------
    // 10. Cleanup
    // -----------------------------------------------------------------------
    let _ = std::fs::remove_file(encoded_path);

    unsafe {
        device.device_wait_idle().ok();
        // device and instance Drop impls handle destruction via vulkanalia's
        // Clone semantics (reference counted). We do not call destroy_device
        // or destroy_instance manually because the encoder/decoder may still
        // hold clones. The last clone dropped triggers destruction.
    }

    println!("PASSED: H.265 shared-device encode/decode roundtrip");
}
