// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Error taxonomy for consumer-side RHI operations.

use thiserror::Error;

/// Failures the consumer-side carve-out can return — driver bring-up,
/// missing device extensions, DMA-BUF import failures, configuration
/// mismatches.
///
/// Kept deliberately narrow: the carve-out is small (DMA-BUF FD import
/// + bind + map, sync wait/signal, layout transitions on imported
/// handles), so two variants cover everything that can go wrong.
/// `streamlib::core::StreamError` provides a `From<ConsumerRhiError>`
/// impl, so host-side code can wrap consumer errors with `?`.
#[derive(Debug, Error)]
pub enum ConsumerRhiError {
    /// A Vulkan API call failed (driver returned a non-success result,
    /// loader couldn't bring the library up, required device
    /// extension missing, …). The string carries the API call name
    /// and the driver error.
    #[error("GPU operation failed: {0}")]
    Gpu(String),

    /// Caller-supplied input failed validation before any driver call
    /// — empty plane vec, mismatched plane-array lengths, plane count
    /// over `MAX_DMA_BUF_PLANES`, …
    #[error("invalid configuration: {0}")]
    Configuration(String),
}

/// Result alias paired with [`ConsumerRhiError`].
pub type Result<T> = std::result::Result<T, ConsumerRhiError>;
