// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The video device seam's floor on a platform no capture arm serves.

use super::video_device_backend::{
    VideoCaptureDevice, VideoCaptureStream, VideoDeviceBackend, VideoDeviceStreamRequest,
};
use crate::core::{Error, Result};

/// A backend that lists no devices and refuses every open by name.
///
/// There is no null camera the way there is a silent microphone: a graph that
/// asked for a camera and got blank frames would look healthy while showing
/// nothing, so the refusal is the answer.
pub struct RefusingNullVideoDeviceBackend;

impl VideoDeviceBackend for RefusingNullVideoDeviceBackend {
    fn backend_name(&self) -> &'static str {
        "refusing-null"
    }

    fn list_capture_devices(&self) -> Result<Vec<VideoCaptureDevice>> {
        Ok(Vec::new())
    }

    fn open_capture_stream(
        &self,
        request: &VideoDeviceStreamRequest,
    ) -> Result<Box<dyn VideoCaptureStream>> {
        Err(refusal_for_a_platform_no_capture_arm_serves(
            request.device_id.as_deref(),
        ))
    }
}

fn refusal_for_a_platform_no_capture_arm_serves(device_id: Option<&str>) -> Error {
    let named_device = device_id
        .map(|device_id| format!(" — '{device_id}' cannot be opened"))
        .unwrap_or_default();
    Error::Configuration(format!(
        "No camera capture backend serves {}{named_device}: camera capture runs on Linux \
         (V4L2) only for now. Use TestPatternSource to run without a camera.",
        crate::platform::name()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_refusing_backend_lists_no_devices() {
        assert!(
            RefusingNullVideoDeviceBackend
                .list_capture_devices()
                .expect("listing nothing is not a failure")
                .is_empty()
        );
    }

    #[test]
    fn a_named_device_is_refused_naming_it_and_the_way_to_run_without_one() {
        let refusal =
            refusal_for_a_platform_no_capture_arm_serves(Some("FaceTime HD Camera")).to_string();
        assert!(refusal.contains("'FaceTime HD Camera'"), "{refusal}");
        assert!(refusal.contains("TestPatternSource"), "{refusal}");
    }

    #[test]
    fn the_default_device_is_refused_naming_the_platform() {
        let refusal = refusal_for_a_platform_no_capture_arm_serves(None).to_string();
        assert!(refusal.contains(crate::platform::name()), "{refusal}");
    }
}
