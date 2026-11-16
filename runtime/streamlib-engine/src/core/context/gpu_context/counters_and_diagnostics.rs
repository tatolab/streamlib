// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl GpuContext {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn gpu_capabilities(&self) -> GpuCapabilitiesSnapshot {
        let dev = &self.device.inner;
        GpuCapabilitiesSnapshot {
            device_name: dev.name(),
            supports_external_memory: dev.supports_external_memory(),
            device_is_software: dev.device_is_software(),
            supports_dma_buf_export: dev.supports_dma_buf_export(),
            supports_semaphore_export: dev.supports_semaphore_export(),
            supports_cross_device_dma_buf_probe: dev.supports_cross_device_dma_buf_probe(),
            supports_ray_tracing_pipeline: dev.supports_ray_tracing_pipeline(),
        }
    }

    /// How many times this context has actually run the Slang compiler.
    ///
    /// What a cache-hit assertion counts; elapsed time cannot stand in for it,
    /// since re-creating a kernel is free of compilation while still
    /// allocating handles.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[must_use]
    pub fn slang_shader_compiler_invocation_count(&self) -> u64 {
        self.slang_shader_source_compiler.invocation_count()
    }

    /// How many queue submissions this context's device has made.
    ///
    /// What a batching assertion counts. Elapsed time cannot stand in for it:
    /// a batch is faster than N dispatches for reasons a loaded machine can
    /// hide, while the submission count says outright whether the work went
    /// out as one command buffer or N.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[must_use]
    pub fn queue_submission_count(&self) -> usize {
        self.device.inner.queue_submission_count()
    }

    /// How many times a command recorder, a compute kernel or a compute
    /// dispatch has blocked on its GPU work — the stalls a batch exists to
    /// collapse.
    ///
    /// Recorder-wide, not compute-only: present, staging and the tone mapper
    /// drain recorders too, so a reading is only about a batch when nothing
    /// else on this device is recording. See [`Self::queue_submission_count`]
    /// on why this is counted, not timed.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[must_use]
    pub fn counted_host_waits_on_gpu_work_count(&self) -> usize {
        self.device.inner.counted_host_waits_on_gpu_work_count()
    }
}
