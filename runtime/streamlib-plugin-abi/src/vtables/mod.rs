// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `#[repr(C)]` vtable structs (extern "C" dispatch tables) carried in the
//! [`crate::HostServices`] payload. Each vtable is paired with a
//! `*_VTABLE_LAYOUT_VERSION` constant pinned at offset 0; the host
//! validates the version before dereferencing any other field.

pub mod audio_clock;
pub mod gpu_context_full;
pub mod gpu_context_limited;
pub mod host_timeline_semaphore;
pub mod input_mailboxes;
pub mod output_writer;
pub mod present_target;
pub mod processor;
pub mod rhi_color_converter;
pub mod rhi_command_recorder;
pub mod runtime_context;
pub mod runtime_ops;
pub mod surface_store;
pub mod texture_ring;
pub mod video_decoder_session;
pub mod video_encoder_session;
pub mod vulkan_acceleration_structure;
pub mod vulkan_compute_kernel;
pub mod vulkan_graphics_kernel;
pub mod vulkan_ray_tracing_kernel;
pub mod vulkan_texture_readback;

pub use audio_clock::*;
pub use gpu_context_full::*;
pub use gpu_context_limited::*;
pub use host_timeline_semaphore::*;
pub use input_mailboxes::*;
pub use output_writer::*;
pub use present_target::*;
pub use processor::*;
pub use rhi_color_converter::*;
pub use rhi_command_recorder::*;
pub use runtime_context::*;
pub use runtime_ops::*;
pub use surface_store::*;
pub use texture_ring::*;
pub use video_decoder_session::*;
pub use video_encoder_session::*;
pub use vulkan_acceleration_structure::*;
pub use vulkan_compute_kernel::*;
pub use vulkan_graphics_kernel::*;
pub use vulkan_ray_tracing_kernel::*;
pub use vulkan_texture_readback::*;
