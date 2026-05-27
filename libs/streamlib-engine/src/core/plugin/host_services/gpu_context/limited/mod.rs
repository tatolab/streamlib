// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `GpuContextLimitedAccessVTable` callbacks, split per
//! banner-bounded section of the original `gpu_context.rs` file.
//! Each submodule owns the wrappers for one resource family or one
//! concern; the parent's `HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE`
//! static wires the function pointers up by name.

mod buffer;
mod escalate;
mod texture;
mod texture_registration;
mod video_source_timeline;

pub(in crate::core::plugin::host_services) use buffer::{
    host_gpu_lim_acquire_index_buffer, host_gpu_lim_acquire_storage_buffer,
    host_gpu_lim_acquire_uniform_buffer, host_gpu_lim_acquire_vertex_buffer,
    host_gpu_lim_clone_index_buffer, host_gpu_lim_clone_storage_buffer,
    host_gpu_lim_clone_uniform_buffer, host_gpu_lim_clone_vertex_buffer,
    host_gpu_lim_drop_index_buffer, host_gpu_lim_drop_storage_buffer,
    host_gpu_lim_drop_uniform_buffer, host_gpu_lim_drop_vertex_buffer,
};
pub(in crate::core::plugin::host_services) use escalate::{
    host_gpu_lim_escalate_begin, host_gpu_lim_escalate_end,
};
pub(in crate::core::plugin::host_services) use texture::{
    host_gpu_lim_acquire_texture, host_gpu_lim_clone_texture, host_gpu_lim_drop_pooled_texture_handle,
    host_gpu_lim_drop_texture, host_gpu_lim_register_texture,
    host_gpu_lim_resolve_texture_by_surface_id, host_gpu_lim_texture_native_dma_buf_fd,
    host_gpu_lim_unregister_texture, host_gpu_lim_update_texture_registration_layout,
};
pub(in crate::core::plugin::host_services) use texture_registration::{
    host_gpu_lim_clone_texture_registration, host_gpu_lim_drop_texture_registration,
    host_gpu_lim_resolve_texture_registration_by_surface_id,
    host_gpu_lim_texture_registration_current_layout,
    host_gpu_lim_texture_registration_texture,
    host_gpu_lim_texture_registration_update_layout,
};
pub(in crate::core::plugin::host_services) use video_source_timeline::{
    host_gpu_lim_clear_video_source_timeline_semaphore,
    host_gpu_lim_host_video_source_timeline_arc,
    host_gpu_lim_set_video_source_timeline_semaphore,
    host_gpu_lim_wait_timeline_semaphore,
};
