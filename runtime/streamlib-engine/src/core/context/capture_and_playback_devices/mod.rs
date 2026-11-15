// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Audio and video device backends: their probes, null arms, liveness reports, access prompts and
//! the capture conversion stage.

pub(super) mod audio_device_backend;
pub(super) mod capture_device_access_prompting;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod captured_video_frame_to_pooled_rgba_conversion_stage;
pub(super) mod device_backend_probe_chain;
pub(super) mod device_stream_liveness_report;
pub(super) mod refusing_null_video_device_backend;
pub(crate) mod silent_null_audio_device_backend;
pub(super) mod video_device_backend;
