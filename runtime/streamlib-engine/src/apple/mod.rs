// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

pub(crate) mod audio_clock;
pub(crate) mod avfoundation_video_device_backend;
pub(crate) mod biplanar_420_iosurface_to_pooled_rgba_conversion;
pub(crate) mod core_video_pixel_buffer_color;
pub(crate) mod core_video_pixel_format_dictionary;
pub(crate) mod coreaudio_audio_device_backend;
pub(crate) mod corevideo_ffi;
pub(crate) mod folder_moved_to_the_trash;
pub(crate) mod imported_iosurface_storage_buffers_kept_for_recycling;
pub(crate) mod iosurface;
pub(crate) mod media_clock;
pub(crate) mod vimage_ffi;

pub(crate) mod permissions;
pub(crate) mod responsible_gui_application;
pub(crate) mod surface_share;

pub(crate) mod thread_priority;

pub(crate) use audio_clock::CoreAudioClock;
