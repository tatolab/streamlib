//! Observability layer for runtime inspection and monitoring.

mod inspector;
mod snapshots;

pub use inspector::GraphInspector;
pub use snapshots::{GraphHealth, LatencyStats, LinkSnapshot, ProcessorSnapshot};
