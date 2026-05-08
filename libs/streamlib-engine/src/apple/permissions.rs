// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::Result;
use objc2::MainThreadMarker;

pub fn request_camera_permission() -> Result<bool> {
    use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeVideo};

    let _mtm = MainThreadMarker::new().ok_or_else(|| {
        crate::core::StreamError::Configuration(
            "request_camera_permission must be called on main thread".into(),
        )
    })?;

    tracing::info!("Checking camera permission status...");

    let media_type = unsafe {
        AVMediaTypeVideo.ok_or_else(|| {
            crate::core::StreamError::Configuration("AVMediaTypeVideo not available".into())
        })?
    };

    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) };

    match status.0 {
        3 => {
            tracing::info!("Camera permission already granted");
            Ok(true)
        }
        0 => {
            tracing::info!("Camera permission not determined, will be requested on first use");
            Ok(true)
        }
        1 | 2 => {
            tracing::error!(
                "Camera permission denied or restricted (status={})",
                status.0
            );
            Ok(false)
        }
        _ => {
            tracing::warn!("Unknown camera authorization status: {}", status.0);
            Ok(false)
        }
    }
}

pub fn request_display_permission() -> Result<bool> {
    tracing::info!("Display permission granted (no system prompt required on macOS)");
    Ok(true)
}

pub fn request_audio_permission() -> Result<bool> {
    use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeAudio};

    let _mtm = MainThreadMarker::new().ok_or_else(|| {
        crate::core::StreamError::Configuration(
            "request_audio_permission must be called on main thread".into(),
        )
    })?;

    tracing::info!("Checking audio permission status...");

    let media_type = unsafe {
        AVMediaTypeAudio.ok_or_else(|| {
            crate::core::StreamError::Configuration("AVMediaTypeAudio not available".into())
        })?
    };

    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) };

    match status.0 {
        3 => {
            tracing::info!("Audio permission already granted");
            Ok(true)
        }
        0 => {
            tracing::info!("Audio permission not determined, will be requested on first use");
            Ok(true)
        }
        1 | 2 => {
            tracing::error!(
                "Audio permission denied or restricted (status={})",
                status.0
            );
            Ok(false)
        }
        _ => {
            tracing::warn!("Unknown audio authorization status: {}", status.0);
            Ok(false)
        }
    }
}
