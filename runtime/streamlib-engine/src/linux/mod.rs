// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Linux-specific implementations.

pub mod alsa_audio_device_backend;
pub mod audio_clock;
pub mod host_identity;
pub mod machine_clock_identity;
pub mod pipewire_audio_device_backend;
pub mod pipewire_runtime_library;
pub mod pipewire_video_source;
pub mod rtkit;
pub mod surface_share;
pub mod thread_priority;
pub mod v4l2_color;
pub mod v4l2_video_device_backend;

pub use audio_clock::LinuxTimerFdAudioClock;

// Domain processors (camera, display, codecs, debug utilities, etc.)
// live in their own `packages/<name>/` carve-outs.
