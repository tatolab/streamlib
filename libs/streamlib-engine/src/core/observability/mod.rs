// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Observability layer for runtime inspection and monitoring.

mod inspector;
mod snapshots;

pub use inspector::GraphInspector;
pub use snapshots::{GraphHealth, LatencyStats, LinkSnapshot, ProcessorSnapshot};
