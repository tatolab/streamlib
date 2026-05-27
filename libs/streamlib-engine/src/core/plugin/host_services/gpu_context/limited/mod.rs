// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `GpuContextLimitedAccessVTable` callbacks, split per
//! banner-bounded section of the original `gpu_context.rs` file.
//! Each submodule owns the wrappers for one resource family or one
//! concern; the parent's `HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE`
//! static wires the function pointers up by name.

mod escalate;
mod video_source_timeline;

pub(in crate::core::plugin::host_services) use escalate::{
    host_gpu_lim_escalate_begin, host_gpu_lim_escalate_end,
};
pub(in crate::core::plugin::host_services) use video_source_timeline::{
    host_gpu_lim_clear_video_source_timeline_semaphore,
    host_gpu_lim_host_video_source_timeline_arc,
    host_gpu_lim_set_video_source_timeline_semaphore,
    host_gpu_lim_wait_timeline_semaphore,
};
