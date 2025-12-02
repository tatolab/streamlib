//! Point-in-time snapshot types for graph observation.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::core::graph::ProcessorId;
use crate::core::link_channel::LinkId;
use crate::core::processors::ProcessorState;

/// Point-in-time snapshot of a processor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorSnapshot {
    /// Processor identifier.
    pub id: ProcessorId,
    /// Processor type name.
    pub processor_type: String,
    /// Current state.
    pub state: ProcessorState,
    /// Throughput in frames per second.
    pub throughput_fps: f64,
    /// Latency statistics.
    pub latency: LatencyStats,
    /// Current configuration as JSON.
    pub config: serde_json::Value,
}

/// Point-in-time snapshot of a link.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkSnapshot {
    /// Link identifier.
    pub id: LinkId,
    /// Source processor.
    pub source_processor: ProcessorId,
    /// Source port name.
    pub source_port: String,
    /// Target processor.
    pub target_processor: ProcessorId,
    /// Target port name.
    pub target_port: String,
    /// Current queue depth.
    pub queue_depth: usize,
    /// Queue capacity.
    pub capacity: usize,
    /// Throughput in frames per second.
    pub throughput_fps: f64,
}

/// Latency statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LatencyStats {
    /// 50th percentile latency.
    pub p50: Duration,
    /// 90th percentile latency.
    pub p90: Duration,
    /// 99th percentile latency.
    pub p99: Duration,
    /// Maximum observed latency.
    pub max: Duration,
}

/// Overall graph health summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphHealth {
    /// Current graph state.
    pub state: GraphStateSnapshot,
    /// Number of processors.
    pub processor_count: usize,
    /// Number of links.
    pub link_count: usize,
    /// Total dropped frames across all processors.
    pub dropped_frames: u64,
    /// Total error count.
    pub error_count: u64,
    /// Processors identified as bottlenecks.
    pub bottlenecks: Vec<ProcessorId>,
}

/// Snapshot of graph state (serializable version).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GraphStateSnapshot {
    #[default]
    Idle,
    Running,
    Paused,
}

impl GraphHealth {
    /// Check if the graph is healthy (no errors, no bottlenecks).
    pub fn is_healthy(&self) -> bool {
        self.error_count == 0 && self.bottlenecks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_graph_health_is_healthy() {
        let healthy = GraphHealth {
            state: GraphStateSnapshot::Running,
            processor_count: 3,
            link_count: 2,
            dropped_frames: 0,
            error_count: 0,
            bottlenecks: vec![],
        };
        assert!(healthy.is_healthy());

        let unhealthy = GraphHealth {
            state: GraphStateSnapshot::Running,
            processor_count: 3,
            link_count: 2,
            dropped_frames: 10,
            error_count: 1,
            bottlenecks: vec![],
        };
        assert!(!unhealthy.is_healthy());
    }

    #[test]
    fn test_latency_stats_default() {
        let stats = LatencyStats::default();
        assert_eq!(stats.p50, Duration::ZERO);
        assert_eq!(stats.p99, Duration::ZERO);
    }
}
