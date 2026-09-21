// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The AVFoundation arm captures from a real camera through the video device
//! seam: frames arrive at the negotiated extent in pooled `Rgba32` pixel
//! buffers with real content, each stamped with a non-zero, increasing capture
//! instant in the `MediaClock` epoch — zero-copy wherever the driver can import
//! the camera's IOSurfaces, and through a CPU copy only where it cannot.
//!
//! Rig tier — it needs a camera, the GPU, and camera access already allowed for
//! the terminal running it, so Cargo builds it only under `hardware-tests`.
//! `STREAMLIB_CAMERA_CAPTURE_PNG` names where the frame it scores is written;
//! a temporary file otherwise.

#![cfg(target_os = "macos")]

use std::ptr::NonNull;
use std::sync::Arc;
use std::time::{Duration, Instant};

use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CFType};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetIOSurface,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    kCVReturnSuccess,
};
use parking_lot::Mutex;
use streamlib::sdk::context::{GpuContext, VideoDeviceStreamRequest, probe_video_device_backend};
use streamlib::sdk::media_clock::MediaClock;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

const CAPTURE_CAP_WIDTH: u32 = 1280;
const CAPTURE_CAP_HEIGHT: u32 = 720;

/// Enough frames to see a steady stream, and the one whose pixels are scored —
/// past the first few, which an auto-exposing camera delivers dark.
const FRAMES_TO_COLLECT: usize = 60;
const FRAME_WHOSE_PIXELS_ARE_SCORED: usize = 30;

/// Opening a camera and its first frames take a moment; a camera access
/// prompt still waiting on the user takes forever, which this bound turns into
/// a failure naming it.
const COLLECTION_DEADLINE: Duration = Duration::from_secs(20);

/// Below this, a frame's luma spread is a flat field — black, or a constant
/// colour — rather than anything a camera sees.
const LEAST_LUMA_STANDARD_DEVIATION_OF_REAL_CONTENT: f64 = 4.0;

struct HandedOffFrame {
    width: u32,
    height: u32,
    capture_timestamp_ns: i64,
    handed_off_at_ns: i64,
}

/// The transport the arm logged on its first frame — how a camera frame
/// reached the GPU.
#[derive(Default)]
struct FirstFrameTransport(Mutex<Option<String>>);

struct FirstFrameTransportRecorder(Arc<FirstFrameTransport>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for FirstFrameTransportRecorder {
    fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
        struct TransportField(Option<String>);
        impl tracing::field::Visit for TransportField {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if field.name() == "transport" {
                    self.0 = Some(value.to_owned());
                }
            }

            fn record_debug(
                &mut self,
                _field: &tracing::field::Field,
                _value: &dyn std::fmt::Debug,
            ) {
            }
        }
        let mut transport = TransportField(None);
        event.record(&mut transport);
        if let Some(transport) = transport.0 {
            *self.0.0.lock() = Some(transport);
        }
    }
}

/// Whether this driver imports a CoreVideo 4:2:0 surface as a storage buffer —
/// what decides whether the arm may capture zero-copy.
fn the_driver_imports_a_corevideo_420v_surface(gpu: &GpuContext) -> bool {
    let no_surface_properties = CFDictionary::<CFString, CFType>::empty();
    let no_surface_properties: &CFType = &no_surface_properties;
    // SAFETY: a CoreVideo-exported key.
    let attributes = CFDictionary::<CFString, CFType>::from_slices(
        &[unsafe { kCVPixelBufferIOSurfacePropertiesKey }],
        &[no_surface_properties],
    );
    let mut pixel_buffer: *mut CVPixelBuffer = std::ptr::null_mut();
    // SAFETY: the out-pointer is a valid stack slot and the attributes a live
    // dictionary.
    let created = unsafe {
        CVPixelBufferCreate(
            None,
            1280,
            720,
            kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            Some(attributes.as_opaque()),
            NonNull::from(&mut pixel_buffer),
        )
    };
    assert_eq!(created, kCVReturnSuccess, "CVPixelBufferCreate");
    // SAFETY: a successful create hands back a +1 buffer.
    let pixel_buffer =
        unsafe { CFRetained::from_raw(NonNull::new(pixel_buffer).expect("a created buffer")) };
    let iosurface = CVPixelBufferGetIOSurface(Some(&pixel_buffer)).expect("IOSurface-backed");
    gpu.import_iosurface_as_storage_buffer(&iosurface).is_ok()
}

#[derive(Default)]
struct WhatTheHandOffSaw {
    frames: Vec<HandedOffFrame>,
    scored_frame_rgba: Option<Vec<u8>>,
    unresolvable_frames: Vec<String>,
}

#[test]
fn the_default_camera_delivers_stamped_frames_with_real_content() {
    let first_frame_transport = Arc::new(FirstFrameTransport::default());
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with(tracing_subscriber::fmt::layer().with_test_writer())
        .with(FirstFrameTransportRecorder(Arc::clone(
            &first_frame_transport,
        )))
        .try_init();
    let gpu = GpuContext::init_for_platform().expect("a Vulkan device on MoltenVK");
    let backend = probe_video_device_backend();
    assert_eq!(backend.backend_name(), "avfoundation");

    let mut stream = backend
        .open_capture_stream(&VideoDeviceStreamRequest {
            device_id: None,
            max_width: CAPTURE_CAP_WIDTH,
            max_height: CAPTURE_CAP_HEIGHT,
            gpu_context: gpu.limited_access(),
        })
        .expect("the default camera opens");
    let stream_format = stream.stream_format();
    assert!(
        stream_format.width > 0
            && stream_format.width <= CAPTURE_CAP_WIDTH
            && stream_format.height > 0
            && stream_format.height <= CAPTURE_CAP_HEIGHT,
        "the negotiated extent fits the cap: {stream_format:?}"
    );

    let seen = Arc::new(Mutex::new(WhatTheHandOffSaw::default()));
    let seen_by_the_hand_off = Arc::clone(&seen);
    let resolving_gpu = gpu.limited_access();
    let started_at_ns = MediaClock::now().as_nanos() as i64;
    stream
        .start_delivering_to(Box::new(move |captured| {
            let handed_off_at_ns = MediaClock::now().as_nanos() as i64;
            let mut seen = seen_by_the_hand_off.lock();
            if seen.frames.len() == FRAME_WHOSE_PIXELS_ARE_SCORED {
                match resolving_gpu.resolve_pixel_buffer_by_surface_id(
                    &captured.published_pixel_buffer_frame_id.to_string(),
                ) {
                    Ok(pixel_buffer) => {
                        let byte_len = captured.width as usize * captured.height as usize * 4;
                        // SAFETY: the arm holds the pooled slot for the length of
                        // the hand-off, and an `Rgba32` buffer of this extent is
                        // `width × height × 4` tightly packed bytes.
                        let rgba = unsafe {
                            std::slice::from_raw_parts(pixel_buffer.plane_base_address(0), byte_len)
                        };
                        seen.scored_frame_rgba = Some(rgba.to_vec());
                    }
                    Err(e) => seen.unresolvable_frames.push(e.to_string()),
                }
            }
            seen.frames.push(HandedOffFrame {
                width: captured.width,
                height: captured.height,
                capture_timestamp_ns: captured.capture_timestamp_ns,
                handed_off_at_ns,
            });
        }))
        .expect("delivery starts");

    let give_up_at = Instant::now() + COLLECTION_DEADLINE;
    while seen.lock().frames.len() < FRAMES_TO_COLLECT && Instant::now() < give_up_at {
        std::thread::sleep(Duration::from_millis(20));
    }
    stream.stop_delivering().expect("delivery stops");
    let frames_when_stopped = seen.lock().frames.len();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        seen.lock().frames.len(),
        frames_when_stopped,
        "a stop that returned Ok is never followed by a hand-off"
    );

    let seen = seen.lock();
    assert!(
        seen.frames.len() >= FRAMES_TO_COLLECT,
        "only {} frames in {COLLECTION_DEADLINE:?} — is camera access allowed for the terminal \
         running this (System Settings › Privacy & Security › Camera)? Liveness: {:?}",
        seen.frames.len(),
        stream.liveness_report().failure_that_ended_the_stream()
    );
    assert!(
        seen.unresolvable_frames.is_empty(),
        "{:?}",
        seen.unresolvable_frames
    );

    for frame in &seen.frames {
        assert_eq!(
            (frame.width, frame.height),
            (stream_format.width, stream_format.height)
        );
        assert!(frame.capture_timestamp_ns > 0);
        assert!(
            frame.capture_timestamp_ns <= frame.handed_off_at_ns,
            "a frame is captured before it is handed off"
        );
        assert!(
            frame.capture_timestamp_ns >= started_at_ns - 1_000_000_000,
            "the capture instant is in the MediaClock epoch, within a second of the start"
        );
    }
    for (earlier, later) in seen.frames.iter().zip(seen.frames.iter().skip(1)) {
        assert!(
            later.capture_timestamp_ns > earlier.capture_timestamp_ns,
            "capture instants increase frame to frame"
        );
    }

    let expected_transport = if the_driver_imports_a_corevideo_420v_surface(&gpu) {
        "IOSurface zero-copy"
    } else {
        "CPU upload"
    };
    assert_eq!(
        first_frame_transport.0.lock().as_deref(),
        Some(expected_transport),
        "a driver that imports the camera's IOSurfaces must be given them without a CPU copy"
    );

    let scored_frame_rgba = seen
        .scored_frame_rgba
        .as_ref()
        .expect("the scored frame was read");
    let png_path = std::env::var_os("STREAMLIB_CAMERA_CAPTURE_PNG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("streamlib-avfoundation-capture.png"));
    write_rgba_png(
        &png_path,
        stream_format.width,
        stream_format.height,
        scored_frame_rgba,
    );
    let luma_standard_deviation = luma_standard_deviation_of(scored_frame_rgba);
    tracing::info!(
        png = %png_path.display(),
        luma_standard_deviation,
        "scored frame written"
    );
    assert!(
        luma_standard_deviation > LEAST_LUMA_STANDARD_DEVIATION_OF_REAL_CONTENT,
        "frame {FRAME_WHOSE_PIXELS_ARE_SCORED} is a flat field (luma standard deviation \
         {luma_standard_deviation:.2}); see {}",
        png_path.display()
    );
}

/// A camera takes seconds to power up; a stop arriving meanwhile must not wait
/// for it, or it can run past the engine's five-second budget for a processor
/// to stop and the processor's thread is abandoned. A stop that waited for the
/// session to stop takes a second or more on a cold camera; one that does not
/// wait takes microseconds, and the bound sits between them.
#[test]
fn a_stop_while_the_camera_powers_up_does_not_wait_for_the_camera() {
    const A_STOP_THAT_DOES_NOT_WAIT_FOR_THE_CAMERA: Duration = Duration::from_millis(500);
    let gpu = GpuContext::init_for_platform().expect("a Vulkan device on MoltenVK");
    let mut stream = probe_video_device_backend()
        .open_capture_stream(&VideoDeviceStreamRequest {
            device_id: None,
            max_width: CAPTURE_CAP_WIDTH,
            max_height: CAPTURE_CAP_HEIGHT,
            gpu_context: gpu.limited_access(),
        })
        .expect("the default camera opens");
    for _ in 0..3 {
        stream
            .start_delivering_to(Box::new(|_captured| {}))
            .expect("delivery starts");
        std::thread::sleep(Duration::from_millis(50));
        let stop_began = Instant::now();
        stream.stop_delivering().expect("delivery stops");
        assert!(
            stop_began.elapsed() < A_STOP_THAT_DOES_NOT_WAIT_FOR_THE_CAMERA,
            "a stop during power-up took {:?}",
            stop_began.elapsed()
        );
    }
}

#[test]
fn a_named_camera_that_is_not_attached_is_refused_naming_it() {
    let gpu = GpuContext::init_for_platform().expect("a Vulkan device on MoltenVK");
    let refusal =
        match probe_video_device_backend().open_capture_stream(&VideoDeviceStreamRequest {
            device_id: Some("streamlib-absent-camera".into()),
            max_width: CAPTURE_CAP_WIDTH,
            max_height: CAPTURE_CAP_HEIGHT,
            gpu_context: gpu.limited_access(),
        }) {
            Ok(_) => panic!("a camera that is not attached cannot open"),
            Err(refusal) => refusal.to_string(),
        };
    assert!(
        refusal.contains("'streamlib-absent-camera' does not exist"),
        "{refusal}"
    );
}

fn luma_standard_deviation_of(rgba: &[u8]) -> f64 {
    let lumas: Vec<f64> = rgba
        .chunks_exact(4)
        .map(|pixel| 0.2126 * pixel[0] as f64 + 0.7152 * pixel[1] as f64 + 0.0722 * pixel[2] as f64)
        .collect();
    let mean = lumas.iter().sum::<f64>() / lumas.len() as f64;
    (lumas.iter().map(|luma| (luma - mean).powi(2)).sum::<f64>() / lumas.len() as f64).sqrt()
}

fn write_rgba_png(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) {
    let file = std::fs::File::create(path).expect("the PNG path is writable");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(rgba))
        .expect("the PNG encodes");
}
