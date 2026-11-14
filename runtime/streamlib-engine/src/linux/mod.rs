// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Linux-specific implementations.

pub mod alsa_audio_device_backend;
pub mod audio_clock;
pub(crate) mod dma_buf_mapped_for_cpu_reads;
pub(crate) mod folder_moved_to_the_freedesktop_trash;
pub mod pipewire_audio_device_backend;
pub mod pipewire_runtime_library;
pub mod pipewire_video_source;
pub mod rtkit;
pub mod surface_share;
pub mod system_memory_available_from_proc_meminfo;
pub mod thread_priority;
pub mod v4l2_color;
pub mod v4l2_video_device_backend;

pub use audio_clock::LinuxTimerFdAudioClock;

// Domain processors (camera, display, codecs, debug utilities, etc.)
// live in their own `packages/<name>/` carve-outs.
