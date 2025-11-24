use crate::core::bus::ConnectionId;
use crate::core::handles::ProcessorId;

/// Execution plan - how to run the graph
///
/// Phase 0: Only Legacy variant is implemented
#[derive(Debug, Clone)]
pub enum ExecutionPlan {
    /// Current behavior: one thread per processor, default buffer sizes
    Legacy {
        processors: Vec<ProcessorId>,
        connections: Vec<ConnectionId>,
    },
    // Future variants (NOT IMPLEMENTED YET):
    //
    // /// Smart thread priorities and buffer sizing
    // Prioritized {
    //     threads: HashMap<ProcessorId, ThreadPriority>,
    //     buffer_sizes: HashMap<ConnectionId, usize>,
    // },
    //
    // /// Processor fusion (inline calls, no threads for fused processors)
    // Fused {
    //     threads: HashMap<ProcessorId, ThreadConfig>,
    //     fused_groups: Vec<FusionGroup>,
    //     buffer_sizes: HashMap<ConnectionId, usize>,
    // },
    //
    // /// Thread pooling (share threads across processors)
    // Pooled {
    //     dedicated_threads: Vec<ProcessorId>,
    //     pooled_processors: Vec<ProcessorId>,
    //     pool_size: usize,
    //     buffer_sizes: HashMap<ConnectionId, usize>,
    // },
}

impl ExecutionPlan {
    /// Export execution plan as JSON for testing and debugging
    ///
    /// This allows you to:
    /// 1. **Verify optimizer decisions in tests** - Assert exact plan structure
    /// 2. **Debug optimization** - Inspect what the optimizer chose
    /// 3. **Compare plans** - Snapshot test before/after optimization changes
    /// 4. **Visualize execution** - Show how graph maps to threads/buffers
    ///
    /// # Example Test Usage
    /// ```rust
    /// let plan = runtime.execution_plan().unwrap();
    /// let json = plan.to_json();
    ///
    /// // Verify it's a Legacy plan
    /// assert_eq!(json["variant"], "Legacy");
    ///
    /// // Check processor execution order
    /// let processors = json["processors"].as_array().unwrap();
    /// assert_eq!(processors.len(), 3);
    /// assert_eq!(processors[0], "camera_1");  // Source first
    /// assert_eq!(processors[2], "display_1"); // Sink last
    ///
    /// // Verify all connections are included
    /// let connections = json["connections"].as_array().unwrap();
    /// assert_eq!(connections.len(), 2);
    /// ```
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            ExecutionPlan::Legacy {
                processors,
                connections,
            } => {
                serde_json::json!({
                    "variant": "Legacy",
                    "processors": processors,
                    "connections": connections,
                    "description": "One thread per processor, default buffer sizes"
                })
            } // Future variants will serialize their specific fields
        }
    }
}
