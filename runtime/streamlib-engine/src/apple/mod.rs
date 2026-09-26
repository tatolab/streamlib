// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

pub mod appkit_content_view_of_winit_window;
pub mod audio_clock;
pub mod avfoundation_video_device_backend;
pub(crate) mod biplanar_420_iosurface_to_pooled_rgba_conversion;
pub mod core_video_pixel_buffer_color;
pub(crate) mod core_video_pixel_format_dictionary;
pub mod coreaudio_audio_device_backend;
pub mod corevideo_ffi;
pub mod host_identity;
pub mod iosurface;
pub mod machine_clock_identity;
pub mod media_clock;
pub mod metal_layer_added_as_sublayer_of_window_content_view;
pub mod vimage_ffi;

pub mod permissions;
pub mod responsible_gui_application;
pub mod surface_share;

pub mod main_thread;

pub mod application_menu;

pub mod thread_priority;
pub(crate) mod videotoolbox_video_codec_backend;

pub use audio_clock::CoreAudioClock;
