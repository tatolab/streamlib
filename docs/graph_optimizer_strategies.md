# Graph Optimizer Strategies (Phases 1-5)

## Overview

This document describes the actual **optimization strategies** that the GraphOptimizer can employ once the infrastructure from Phase 0 is complete. Each phase introduces a new execution plan type that improves performance in specific ways.

**Prerequisites**: Complete Phase 0 (see `graph_optimizer_infrastructure.md`) - the GraphOptimizer infrastructure with petgraph representation and query APIs.

**Key Principle**: Each optimization phase is **optional and independent**. We can ship Phase 1, skip Phase 2, implement Phase 3, etc. based on performance measurements and user needs.

## Execution Plan Evolution

```rust
pub enum ExecutionPlan {
    /// Phase 0: Current behavior (infrastructure complete)
    Legacy {
        processors: Vec<ProcessorId>,
        connections: Vec<ConnectionId>,
    },

    /// Phase 1: Smart thread priorities
    Prioritized {
        threads: HashMap<ProcessorId, ThreadPriority>,
        buffer_sizes: HashMap<ConnectionId, usize>,
    },

    /// Phase 2: Processor fusion
    Fused {
        threads: HashMap<ProcessorId, ThreadConfig>,
        fused_groups: Vec<FusionGroup>,
        buffer_sizes: HashMap<ConnectionId, usize>,
    },

    /// Phase 3: Thread pooling
    Pooled {
        dedicated_threads: Vec<ProcessorId>,
        pooled_processors: Vec<ProcessorId>,
        pool_size: usize,
        buffer_sizes: HashMap<ConnectionId, usize>,
    },

    // Future phases...
}

#[derive(Debug, Clone)]
pub enum ThreadPriority {
    Realtime,  // Audio I/O, time-critical
    High,      // Video I/O, display
    Normal,    // Processing, transforms
    Low,       // Background tasks
}

#[derive(Debug, Clone)]
pub struct ThreadConfig {
    priority: ThreadPriority,
    fused_children: Vec<ProcessorId>,
}

#[derive(Debug, Clone)]
pub struct FusionGroup {
    parent: ProcessorId,
    children: Vec<ProcessorId>,
}
```

---

## Phase 1: Smart Thread Priorities

**Goal**: Apply appropriate thread priorities based on processor type and position in graph.

**Benefit**: Reduces latency and jitter for time-critical processors (audio I/O, video I/O).

**Risk**: Low - just changes thread priority, doesn't change topology.

### Strategy

Assign thread priorities based on processor characteristics:

1. **Realtime Priority**: Audio I/O processors
   - `AudioCaptureProcessor`
   - `AudioOutputProcessor`
   - Critical for low-latency audio (<10ms)

2. **High Priority**: Video I/O processors
   - `CameraProcessor`
   - `DisplayProcessor`
   - Reduces dropped frames

3. **Normal Priority**: Everything else
   - Encoders, decoders, filters, transforms

### Implementation

```rust
impl GraphOptimizer {
    fn compute_prioritized_plan(&self) -> ExecutionPlan {
        let mut threads = HashMap::new();
        let mut buffer_sizes = HashMap::new();

        for node_idx in self.graph.node_indices() {
            let node = &self.graph[node_idx];
            let priority = self.infer_priority(&node.processor_type);
            threads.insert(node.id.clone(), priority);
        }

        // Compute buffer sizes based on priorities
        for edge_idx in self.graph.edge_indices() {
            let edge = &self.graph[edge_idx];
            let (from_idx, to_idx) = self.graph.edge_endpoints(edge_idx).unwrap();

            let from_priority = threads.get(&self.graph[from_idx].id).unwrap();
            let to_priority = threads.get(&self.graph[to_idx].id).unwrap();

            let size = self.compute_buffer_size(from_priority, to_priority);
            buffer_sizes.insert(edge.id.clone(), size);
        }

        ExecutionPlan::Prioritized {
            threads,
            buffer_sizes,
        }
    }

    fn infer_priority(&self, processor_type: &str) -> ThreadPriority {
        // Heuristics based on processor type name
        if processor_type.contains("AudioCapture") || processor_type.contains("AudioOutput") {
            ThreadPriority::Realtime
        } else if processor_type.contains("Camera") || processor_type.contains("Display") {
            ThreadPriority::High
        } else {
            ThreadPriority::Normal
        }
    }

    fn compute_buffer_size(&self, from: &ThreadPriority, to: &ThreadPriority) -> usize {
        use ThreadPriority::*;

        match (from, to) {
            (Realtime, Realtime) => 2,   // Minimal latency
            (Realtime, High) => 3,        // Audio → Video
            (Realtime, Normal) => 4,      // Audio → Processing
            (High, Realtime) => 3,        // Video → Audio
            (High, High) => 3,            // Video → Video (camera → display)
            (High, Normal) => 8,          // Video → Processing
            (Normal, Realtime) => 4,      // Processing → Audio
            (Normal, High) => 4,          // Processing → Video
            (Normal, Normal) => 16,       // Processing → Processing
            _ => 10,                      // Default
        }
    }
}

// Platform-specific thread priority implementation
#[cfg(target_os = "macos")]
fn set_thread_priority(priority: ThreadPriority) -> Result<()> {
    use mach::thread_policy::*;
    use mach::traps::mach_thread_self;

    let thread = mach_thread_self();

    match priority {
        ThreadPriority::Realtime => {
            // Time constraint policy for realtime audio
            let period = 2_902_000; // ~2.9ms @ 1GHz (audio buffer size)
            let computation = 1_000_000; // ~1ms computation time
            let constraint = period;
            let preemptible = 1;

            let policy = thread_time_constraint_policy {
                period,
                computation,
                constraint,
                preemptible,
            };

            unsafe {
                thread_policy_set(
                    thread,
                    THREAD_TIME_CONSTRAINT_POLICY,
                    &policy as *const _ as *const i32,
                    THREAD_TIME_CONSTRAINT_POLICY_COUNT,
                )
            }
        }
        ThreadPriority::High => {
            // Precedence policy for video I/O
            let policy = thread_precedence_policy {
                importance: 63, // High importance (max is 63)
            };

            unsafe {
                thread_policy_set(
                    thread,
                    THREAD_PRECEDENCE_POLICY,
                    &policy as *const _ as *const i32,
                    THREAD_PRECEDENCE_POLICY_COUNT,
                )
            }
        }
        ThreadPriority::Normal | ThreadPriority::Low => {
            // Default scheduling
            Ok(())
        }
    }
}
```

### Application in Runtime

```rust
impl StreamRuntime {
    fn apply_prioritized_plan(&mut self, plan: &ExecutionPlan) -> Result<()> {
        match plan {
            ExecutionPlan::Prioritized { threads, buffer_sizes } => {
                // Spawn threads with priorities
                for (proc_id, priority) in threads {
                    self.spawn_processor_thread_with_priority(proc_id, *priority)?;
                }

                // Update buffer sizes (may require reconnecting)
                for (conn_id, new_size) in buffer_sizes {
                    self.resize_connection_buffer(conn_id, *new_size)?;
                }

                tracing::info!("▶️  Started with thread priorities: {} realtime, {} high, {} normal",
                    threads.values().filter(|p| matches!(p, ThreadPriority::Realtime)).count(),
                    threads.values().filter(|p| matches!(p, ThreadPriority::High)).count(),
                    threads.values().filter(|p| matches!(p, ThreadPriority::Normal)).count(),
                );
            }
            _ => {}
        }
        Ok(())
    }

    fn spawn_processor_thread_with_priority(
        &mut self,
        proc_id: &ProcessorId,
        priority: ThreadPriority,
    ) -> Result<()> {
        let processor = self.processors.get(proc_id).unwrap();

        let handle = std::thread::Builder::new()
            .name(format!("processor-{}", proc_id))
            .spawn(move || {
                // Set thread priority (platform-specific)
                set_thread_priority(priority).ok();

                // Run processor loop
                loop {
                    processor.process()?;
                    if should_stop() { break; }
                }

                Ok(())
            })?;

        self.threads.insert(proc_id.clone(), handle);
        Ok(())
    }
}
```

### Testing

```rust
#[test]
fn test_audio_gets_realtime_priority() {
    let mut optimizer = GraphOptimizer::new();

    optimizer.add_processor(
        "audio_in".into(),
        "AudioCaptureProcessor".into(),
        None,
    );

    let plan = optimizer.compute_prioritized_plan();

    match plan {
        ExecutionPlan::Prioritized { threads, .. } => {
            assert_eq!(
                threads.get("audio_in"),
                Some(&ThreadPriority::Realtime)
            );
        }
        _ => panic!("Expected Prioritized plan"),
    }
}

#[test]
fn test_buffer_sizing_realtime_to_normal() {
    let mut optimizer = GraphOptimizer::new();

    let size = optimizer.compute_buffer_size(
        &ThreadPriority::Realtime,
        &ThreadPriority::Normal,
    );

    assert_eq!(size, 4); // Small buffer for low latency
}
```

**Performance Target**: Reduce audio latency jitter by 50%+ (measured with latency histograms).

---

## Phase 2: Processor Fusion

**Goal**: Eliminate context switching by running lightweight processors inline in their parent's thread.

**Benefit**: 10-30% CPU reduction for pipelines with simple transforms, better cache locality.

**Risk**: Medium - changes execution topology, needs careful testing.

### Strategy

Fuse a processor if:
1. ✅ Exactly one input connection (no fan-in)
2. ✅ Exactly one output connection (no fan-out)
3. ✅ Processor is "lightweight" (whitelisted types or measured <500μs)
4. ✅ No blocking operations

**Whitelist** (initial candidates):
- `ResizeProcessor` - Simple image resize
- `ColorConvertProcessor` - RGB ↔ YUV
- `AudioChannelConverterProcessor` - Mono ↔ Stereo
- `SimpleFilterProcessor` - Lightweight pixel operations

**NOT fusable**:
- Hardware I/O (Camera, Display, Audio)
- Encoders/Decoders (heavy compute)
- Anything with fan-in/fan-out

### Implementation

```rust
impl GraphOptimizer {
    fn compute_fused_plan(&self) -> ExecutionPlan {
        let mut threads = HashMap::new();
        let mut fused_groups = Vec::new();

        // First pass: identify fusion candidates
        let mut fused_processors = HashSet::new();

        for node_idx in self.graph.node_indices() {
            let node = &self.graph[node_idx];

            if self.can_be_fused(node_idx) {
                // Find parent (single upstream processor)
                let mut parents = self.graph.neighbors_directed(node_idx, Direction::Incoming);
                if let Some(parent_idx) = parents.next() {
                    let parent_id = self.graph[parent_idx].id.clone();

                    // Add to fusion group
                    let mut group = fused_groups.iter_mut()
                        .find(|g: &&mut FusionGroup| g.parent == parent_id);

                    if let Some(group) = group {
                        group.children.push(node.id.clone());
                    } else {
                        fused_groups.push(FusionGroup {
                            parent: parent_id,
                            children: vec![node.id.clone()],
                        });
                    }

                    fused_processors.insert(node.id.clone());
                }
            }
        }

        // Second pass: create thread configs
        for node_idx in self.graph.node_indices() {
            let node = &self.graph[node_idx];

            if !fused_processors.contains(&node.id) {
                // Not fused - gets its own thread
                let priority = self.infer_priority(&node.processor_type);

                let fused_children = fused_groups.iter()
                    .find(|g| g.parent == node.id)
                    .map(|g| g.children.clone())
                    .unwrap_or_default();

                threads.insert(node.id.clone(), ThreadConfig {
                    priority,
                    fused_children,
                });
            }
        }

        // Compute buffer sizes (fused connections eliminated)
        let buffer_sizes = self.compute_buffer_sizes_with_fusion(&threads, &fused_groups);

        ExecutionPlan::Fused {
            threads,
            fused_groups,
            buffer_sizes,
        }
    }

    fn can_be_fused(&self, node_idx: NodeIndex) -> bool {
        // Check topology: exactly one input and one output
        let input_count = self.graph.neighbors_directed(node_idx, Direction::Incoming).count();
        let output_count = self.graph.neighbors_directed(node_idx, Direction::Outgoing).count();

        if input_count != 1 || output_count != 1 {
            return false;
        }

        // Check processor type (whitelist)
        let node = &self.graph[node_idx];
        let fusable_types = [
            "ResizeProcessor",
            "ColorConvertProcessor",
            "AudioChannelConverterProcessor",
            "SimpleFilterProcessor",
        ];

        fusable_types.iter().any(|&t| node.processor_type.contains(t))
    }

    fn compute_buffer_sizes_with_fusion(
        &self,
        threads: &HashMap<ProcessorId, ThreadConfig>,
        fused_groups: &[FusionGroup],
    ) -> HashMap<ConnectionId, usize> {
        let mut buffer_sizes = HashMap::new();

        // Build set of fused processor IDs for quick lookup
        let fused_ids: HashSet<_> = fused_groups.iter()
            .flat_map(|g| g.children.iter())
            .collect();

        for edge_idx in self.graph.edge_indices() {
            let edge = &self.graph[edge_idx];
            let (from_idx, to_idx) = self.graph.edge_endpoints(edge_idx).unwrap();

            let from_id = &self.graph[from_idx].id;
            let to_id = &self.graph[to_idx].id;

            // Skip fused connections (eliminated)
            if fused_ids.contains(&from_id) || fused_ids.contains(&to_id) {
                continue;
            }

            // Compute buffer size based on thread priorities
            let from_priority = threads.get(from_id).map(|c| &c.priority).unwrap_or(&ThreadPriority::Normal);
            let to_priority = threads.get(to_id).map(|c| &c.priority).unwrap_or(&ThreadPriority::Normal);

            let size = self.compute_buffer_size(from_priority, to_priority);
            buffer_sizes.insert(edge.id.clone(), size);
        }

        buffer_sizes
    }
}
```

### Application in Runtime

```rust
impl StreamRuntime {
    fn apply_fused_plan(&mut self, plan: &ExecutionPlan) -> Result<()> {
        match plan {
            ExecutionPlan::Fused { threads, fused_groups, buffer_sizes } => {
                // Spawn threads with fusion support
                for (proc_id, thread_config) in threads {
                    self.spawn_fused_thread(proc_id, thread_config)?;
                }

                let fused_count: usize = fused_groups.iter().map(|g| g.children.len()).sum();

                tracing::info!("▶️  Started {} threads (optimized from {}), {} fused processors",
                    threads.len(),
                    threads.len() + fused_count,
                    fused_count,
                );
            }
            _ => {}
        }
        Ok(())
    }

    fn spawn_fused_thread(
        &mut self,
        proc_id: &ProcessorId,
        config: &ThreadConfig,
    ) -> Result<()> {
        let processor = self.processors.get(proc_id).unwrap();
        let fused_children: Vec<_> = config.fused_children.iter()
            .map(|id| self.processors.get(id).unwrap())
            .collect();

        let handle = std::thread::Builder::new()
            .name(format!("processor-{}", proc_id))
            .spawn(move || {
                set_thread_priority(config.priority).ok();

                loop {
                    // Process main processor
                    processor.process()?;

                    // Process fused children inline (same thread, same stack)
                    for child in &fused_children {
                        child.process()?;
                    }

                    if should_stop() { break; }
                }

                Ok(())
            })?;

        self.threads.insert(proc_id.clone(), handle);
        Ok(())
    }
}
```

### Testing

```rust
#[test]
fn test_fusion_detection() {
    let mut optimizer = GraphOptimizer::new();

    // Graph: Camera → Resize → Display
    optimizer.add_processor("camera".into(), "CameraProcessor".into(), None);
    optimizer.add_processor("resize".into(), "ResizeProcessor".into(), None);
    optimizer.add_processor("display".into(), "DisplayProcessor".into(), None);

    let conn1 = Connection::new(/* camera → resize */);
    let conn2 = Connection::new(/* resize → display */);

    optimizer.add_connection(&conn1);
    optimizer.add_connection(&conn2);

    let plan = optimizer.compute_fused_plan();

    match plan {
        ExecutionPlan::Fused { threads, fused_groups, .. } => {
            // Should have 2 threads (camera + display)
            assert_eq!(threads.len(), 2);

            // Resize should be fused into camera
            assert_eq!(fused_groups.len(), 1);
            assert_eq!(fused_groups[0].parent, "camera");
            assert_eq!(fused_groups[0].children, vec!["resize"]);
        }
        _ => panic!("Expected Fused plan"),
    }
}

#[test]
fn test_no_fusion_fan_out() {
    let mut optimizer = GraphOptimizer::new();

    // Graph: Camera → Resize → Display
    //                       └→ Encoder (fan-out!)

    // Resize has 2 outputs, should NOT be fused

    let plan = optimizer.compute_fused_plan();

    match plan {
        ExecutionPlan::Fused { threads, fused_groups, .. } => {
            // Should have 4 threads (no fusion)
            assert_eq!(threads.len(), 4);
            assert_eq!(fused_groups.len(), 0);
        }
        _ => panic!("Expected Fused plan"),
    }
}
```

**Performance Target**: 10-30% CPU reduction for camera→resize→display pipelines (measured with Activity Monitor).

---

## Phase 3: Thread Pooling

**Goal**: Share threads across multiple processors to reduce overhead for bursty/intermittent work.

**Benefit**: Better CPU utilization for workloads with many idle processors.

**Risk**: Medium - requires work-stealing scheduler, careful load balancing.

### Strategy

Use thread pool for processors that are:
- I/O-bound (waiting on network, disk)
- Bursty (intermittent work)
- Low-priority background tasks

Keep dedicated threads for:
- Hardware I/O (Camera, Audio, Display)
- Real-time processors
- Heavy continuous compute

### Implementation

```rust
impl GraphOptimizer {
    fn compute_pooled_plan(&self) -> ExecutionPlan {
        let mut dedicated_threads = Vec::new();
        let mut pooled_processors = Vec::new();

        for node_idx in self.graph.node_indices() {
            let node = &self.graph[node_idx];

            if self.requires_dedicated_thread(&node.processor_type) {
                dedicated_threads.push(node.id.clone());
            } else {
                pooled_processors.push(node.id.clone());
            }
        }

        let pool_size = (pooled_processors.len() / 4).max(2).min(num_cpus::get());

        let buffer_sizes = self.compute_default_buffer_sizes();

        ExecutionPlan::Pooled {
            dedicated_threads,
            pooled_processors,
            pool_size,
            buffer_sizes,
        }
    }

    fn requires_dedicated_thread(&self, processor_type: &str) -> bool {
        // Hardware I/O always gets dedicated threads
        processor_type.contains("Camera") ||
        processor_type.contains("Display") ||
        processor_type.contains("AudioCapture") ||
        processor_type.contains("AudioOutput") ||
        processor_type.contains("Encoder") ||
        processor_type.contains("Decoder")
    }
}
```

### Application with Rayon

```rust
use rayon::prelude::*;

impl StreamRuntime {
    fn apply_pooled_plan(&mut self, plan: &ExecutionPlan) -> Result<()> {
        match plan {
            ExecutionPlan::Pooled { dedicated_threads, pooled_processors, pool_size, .. } => {
                // Spawn dedicated threads
                for proc_id in dedicated_threads {
                    self.spawn_processor_thread(proc_id)?;
                }

                // Create thread pool
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(*pool_size)
                    .build()
                    .unwrap();

                // Run pooled processors on pool
                pool.scope(|s| {
                    for proc_id in pooled_processors {
                        let processor = self.processors.get(proc_id).unwrap();
                        s.spawn(move |_| {
                            loop {
                                processor.process().ok();
                                if should_stop() { break; }
                            }
                        });
                    }
                });

                tracing::info!("▶️  Started {} dedicated threads + {} pooled processors on {} threads",
                    dedicated_threads.len(),
                    pooled_processors.len(),
                    pool_size,
                );
            }
            _ => {}
        }
        Ok(())
    }
}
```

**Performance Target**: Support 1000+ processors with <100 OS threads (10x improvement).

---

## Phase 4: Profiling-Based Learning

**Goal**: Use runtime measurements to improve optimization decisions.

**Benefit**: Adaptive optimization based on actual workload.

**Risk**: High - requires careful measurement, can be unstable.

### Strategy

1. Measure actual processor execution times
2. Detect slow processors (bottlenecks)
3. Adjust thread priorities and buffer sizes dynamically
4. Learn which processors are actually lightweight (fusion candidates)

### Implementation

```rust
pub struct RuntimeProfiler {
    execution_times: HashMap<ProcessorId, RollingAverage>,
    bottlenecks: Vec<ProcessorId>,
}

impl RuntimeProfiler {
    fn record_execution(&mut self, proc_id: &ProcessorId, duration: Duration) {
        self.execution_times
            .entry(proc_id.clone())
            .or_insert_with(RollingAverage::new)
            .add(duration.as_micros() as f64);
    }

    fn detect_bottlenecks(&self) -> Vec<ProcessorId> {
        let avg_time: f64 = self.execution_times.values()
            .map(|ra| ra.average())
            .sum::<f64>() / self.execution_times.len() as f64;

        self.execution_times.iter()
            .filter(|(_, ra)| ra.average() > avg_time * 2.0)
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn is_fusion_candidate(&self, proc_id: &ProcessorId) -> bool {
        self.execution_times
            .get(proc_id)
            .map(|ra| ra.average() < 500.0) // < 500μs
            .unwrap_or(false)
    }
}
```

---

## Phase 5: Advanced Features (Future)

**Optional enhancements** to consider:

### GPU-Aware Scheduling
- Detect GPU-bound processors
- Co-schedule GPU operations
- Minimize CPU-GPU transfers

### NUMA-Aware Threading
- Pin threads to CPU cores
- Respect NUMA boundaries
- Optimize cache locality

### Energy-Aware Optimization
- Use efficiency cores for background work
- Use performance cores for latency-critical paths
- Dynamic frequency scaling hints

---

## Configuration API

Allow users to override optimization decisions:

```rust
pub struct OptimizerConfig {
    pub enable_priorities: bool,     // Phase 1 (default: true)
    pub enable_fusion: bool,          // Phase 2 (default: true)
    pub enable_pooling: bool,         // Phase 3 (default: false)
    pub enable_profiling: bool,       // Phase 4 (default: false)

    pub fusion_whitelist: Vec<String>,  // Override fusion candidates
    pub force_dedicated: Vec<String>,   // Force processors to get dedicated threads
    pub log_decisions: bool,            // Log optimization decisions (default: true)
}

impl StreamRuntime {
    pub fn new_with_optimizer_config(config: OptimizerConfig) -> Self {
        // ...
    }
}

// Usage
let runtime = StreamRuntime::new_with_optimizer_config(OptimizerConfig {
    enable_fusion: false,  // Disable fusion for debugging
    log_decisions: true,
    ..Default::default()
});
```

---

## Testing Strategy

### Benchmarks for Each Phase

```rust
#[bench]
fn bench_phase_0_legacy(b: &mut Bencher) {
    let runtime = build_test_pipeline();
    b.iter(|| runtime.process_frame());
}

#[bench]
fn bench_phase_1_priorities(b: &mut Bencher) {
    let runtime = build_test_pipeline_with_priorities();
    b.iter(|| runtime.process_frame());
}

#[bench]
fn bench_phase_2_fusion(b: &mut Bencher) {
    let runtime = build_test_pipeline_with_fusion();
    b.iter(|| runtime.process_frame());
}
```

### Performance Validation

For each phase, measure:
- **Latency**: End-to-end frame processing time
- **Jitter**: Latency variance (stddev)
- **CPU Usage**: Total CPU % (Activity Monitor)
- **Context Switches**: Count (measured with `dtrace` or similar)
- **Cache Misses**: L1/L2 cache miss rate

---

## Deployment Strategy

1. **Ship Phase 0** - Get graph query APIs working, zero risk
2. **Measure Baseline** - Collect performance metrics with legacy execution
3. **Ship Phase 1** - Enable priorities, measure impact
4. **Ship Phase 2** - Enable fusion for whitelisted processors only
5. **Learn & Expand** - Add more processors to fusion whitelist based on measurements
6. **Phase 3+** - Only if needed based on user workloads

**Key**: Each phase is optional. We can stop at Phase 1 if it provides sufficient benefit.
