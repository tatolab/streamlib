// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Attribute-macro test fixtures (TestConfiguredProcessor, TcpBindTestProcessor)
//! for streamlib SDK macro contract tests and the #885 dlopen-owns-tokio
//! integration test.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

pub mod compute_kernel_test_processor;
pub mod concurrent_escalate_test_processor;
pub mod escalate_smoke_test_processor;
pub mod gpu_acquire_test_processor;
pub mod graphics_kernel_smoke_test_processor;
pub mod lifecycle_probe_processor;
pub mod panicking_lifecycle_processor;
pub mod ray_tracing_kernel_smoke_test_processor;
pub mod tcp_bind_test_processor;
pub mod test_configured_processor;

pub use compute_kernel_test_processor::ComputeKernelTest;
pub use concurrent_escalate_test_processor::ConcurrentEscalateTest;
pub use escalate_smoke_test_processor::EscalateSmokeTest;
pub use gpu_acquire_test_processor::GpuAcquireTest;
pub use graphics_kernel_smoke_test_processor::GraphicsKernelSmokeTest;
pub use lifecycle_probe_processor::LifecycleProbe;
pub use panicking_lifecycle_processor::{
    PanickingContinuousLifecycle, PanickingManualLifecycle,
};
pub use ray_tracing_kernel_smoke_test_processor::RayTracingKernelSmokeTest;
pub use tcp_bind_test_processor::TcpBindTest;
pub use test_configured_processor::ConfiguredProcessor;

#[cfg(feature = "plugin")]
streamlib_plugin_abi::export_plugin!(
    crate::ConfiguredProcessor::Processor,
    crate::TcpBindTest::Processor,
    crate::GpuAcquireTest::Processor,
    crate::EscalateSmokeTest::Processor,
    crate::ComputeKernelTest::Processor,
    crate::GraphicsKernelSmokeTest::Processor,
    crate::RayTracingKernelSmokeTest::Processor,
    crate::LifecycleProbe::Processor,
    crate::PanickingManualLifecycle::Processor,
    crate::PanickingContinuousLifecycle::Processor,
    crate::ConcurrentEscalateTest::Processor,
);
