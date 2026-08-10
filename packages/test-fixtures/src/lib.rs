// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Crate root for the attribute-macro test fixtures.
//!
//! One `mod` arm per file under `processors/`. Adding a fixture there means
//! adding its arm here — the generator that used to write this file went with
//! the packaging tools.

pub mod test_fixture_processor_configs;

#[path = "../processors/compute_kernel_test_processor.rs"]
pub mod compute_kernel_test_processor;
#[path = "../processors/concurrent_escalate_test_processor.rs"]
pub mod concurrent_escalate_test_processor;
#[path = "../processors/escalate_smoke_test_processor.rs"]
pub mod escalate_smoke_test_processor;
#[path = "../processors/gpu_acquire_test_processor.rs"]
pub mod gpu_acquire_test_processor;
#[path = "../processors/graphics_kernel_smoke_test_processor.rs"]
pub mod graphics_kernel_smoke_test_processor;
#[path = "../processors/lifecycle_probe_processor.rs"]
pub mod lifecycle_probe_processor;
#[path = "../processors/panicking_lifecycle_processor.rs"]
pub mod panicking_lifecycle_processor;
#[path = "../processors/ray_tracing_kernel_smoke_test_processor.rs"]
pub mod ray_tracing_kernel_smoke_test_processor;
#[path = "../processors/test_configured_processor.rs"]
pub mod test_configured_processor;
