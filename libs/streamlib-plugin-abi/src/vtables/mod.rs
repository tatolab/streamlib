// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `#[repr(C)]` vtable structs (extern "C" dispatch tables) carried in the
//! [`crate::HostServices`] payload. Each vtable is paired with a
//! `*_VTABLE_LAYOUT_VERSION` constant pinned at offset 0; the host
//! validates the version before dereferencing any other field.

pub mod audio_clock;
pub mod gpu_context_full;
pub mod gpu_context_limited;
pub mod input_mailboxes;
pub mod output_writer;
pub mod processor;
pub mod rhi_color_converter;
pub mod rhi_command_recorder;
pub mod runtime_context;
pub mod runtime_ops;
pub mod surface_store;
pub mod texture_ring;
pub mod vulkan_acceleration_structure;
pub mod vulkan_compute_kernel;
pub mod vulkan_graphics_kernel;
pub mod vulkan_ray_tracing_kernel;

pub use audio_clock::*;
pub use gpu_context_full::*;
pub use gpu_context_limited::*;
pub use input_mailboxes::*;
pub use output_writer::*;
pub use processor::*;
pub use rhi_color_converter::*;
pub use rhi_command_recorder::*;
pub use runtime_context::*;
pub use runtime_ops::*;
pub use surface_store::*;
pub use texture_ring::*;
pub use vulkan_acceleration_structure::*;
pub use vulkan_compute_kernel::*;
pub use vulkan_graphics_kernel::*;
pub use vulkan_ray_tracing_kernel::*;
