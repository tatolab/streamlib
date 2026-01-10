// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Metal GPU backend for macOS/iOS.

pub mod rhi;

// Re-exports for public API (intentionally exposed for external use)
#[allow(unused_imports)]
pub use rhi::{MetalCommandBuffer, MetalCommandQueue, MetalDevice, MetalTexture};
