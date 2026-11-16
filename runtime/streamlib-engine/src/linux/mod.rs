// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Linux-specific implementations.

pub(crate) mod alsa_audio_device_backend;
pub(crate) mod audio_clock;
pub(crate) mod dma_buf_mapped_for_cpu_reads;
pub(crate) mod folder_moved_to_the_freedesktop_trash;
pub(crate) mod pipewire_audio_device_backend;
pub(crate) mod pipewire_runtime_library;
pub(crate) mod pipewire_video_source;
pub(crate) mod rtkit;
pub(crate) mod surface_share;
pub(crate) mod system_memory_available_from_proc_meminfo;
pub(crate) mod thread_priority;
pub(crate) mod v4l2_color;
pub(crate) mod v4l2_video_device_backend;

pub(crate) use audio_clock::LinuxTimerFdAudioClock;
