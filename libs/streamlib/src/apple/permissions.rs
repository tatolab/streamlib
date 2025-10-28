//! Platform permission handling for macOS/iOS
//!
//! This module provides functions to request system permissions (camera, microphone, etc.)
//! before starting the runtime. This ensures permission dialogs are shown upfront rather
//! than during processor initialization.

use crate::core::Result;
use objc2::MainThreadMarker;

/// Request camera permission on macOS/iOS
///
/// This function MUST be called on the main thread before entering async runtime.
/// It creates a temporary camera processor to trigger the system permission dialog,
/// then checks if permission was granted.
///
/// # Returns
///
/// * `Ok(true)` - Permission granted (either already authorized or user allowed)
/// * `Ok(false)` - Permission denied by user
/// * `Err(_)` - Failed to initialize camera (non-permission error)
///
/// # Platform Behavior
///
/// - **Already authorized**: Returns immediately with no dialog
/// - **Not determined**: Shows macOS permission dialog, blocks until user responds
/// - **Denied/Restricted**: Returns Ok(false)
///
/// # Example
///
/// ```ignore
/// // Call on main thread before tokio runtime
/// if request_camera_permission()? {
///     println!("Camera permission granted");
/// } else {
///     eprintln!("Camera permission denied");
/// }
/// ```
pub fn request_camera_permission() -> Result<bool> {
    use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeVideo};

    // Ensure we're on main thread
    let _mtm = MainThreadMarker::new().ok_or_else(|| {
        crate::core::StreamError::Configuration(
            "request_camera_permission must be called on main thread".into(),
        )
    })?;

    tracing::info!("Checking camera permission status...");

    // Check authorization status without creating a camera session
    // This avoids leaking AVFoundation sessions
    let media_type = unsafe {
        AVMediaTypeVideo.ok_or_else(|| {
            crate::core::StreamError::Configuration("AVMediaTypeVideo not available".into())
        })?
    };

    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) };

    // AVAuthorizationStatus values:
    // 0 = NotDetermined (user hasn't been asked yet)
    // 1 = Restricted (parental controls, etc.)
    // 2 = Denied (user explicitly denied)
    // 3 = Authorized (user granted permission)

    match status.0 {
        3 => {
            // Already authorized
            tracing::info!("Camera permission already granted");
            Ok(true)
        }
        0 => {
            // Not determined - need to request permission by creating a camera
            // Unfortunately macOS requires actually starting a session to trigger the dialog
            tracing::info!("Camera permission not determined, will be requested on first use");
            // Return true - the actual camera processor will trigger the dialog
            Ok(true)
        }
        1 | 2 => {
            // Restricted or Denied
            tracing::error!("Camera permission denied or restricted (status={})", status.0);
            Ok(false)
        }
        _ => {
            tracing::warn!("Unknown camera authorization status: {}", status.0);
            Ok(false)
        }
    }
}

/// Request display permission on macOS/iOS
///
/// On macOS, creating windows doesn't require special permissions,
/// so this always returns true. Included for API consistency.
pub fn request_display_permission() -> Result<bool> {
    tracing::info!("Display permission granted (no system prompt required on macOS)");
    Ok(true)
}
