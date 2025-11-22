# Graph Optimization in StreamLib

## Overview

StreamLib's graph optimizer automatically analyzes processor graphs and makes intelligent decisions about threading, buffer sizing, and processor fusion. The goal is to provide a **"Vite-like" experience** - zero configuration for users, optimal performance by default.

Unlike GStreamer (which requires manual queue placement and thread management), StreamLib's optimizer works **transparently** - users just build their graph with `add_processor()` and `connect()`, and the runtime handles all optimization automatically.

## Core Philosophy

**Design Principles**:
1. **Zero Configuration**: Users never think about threads, queues, or performance tuning
2. **Dynamic Optimization**: Graph can change at runtime (add/remove processors), optimizer adapts
3. **Transparent to Users**: All optimization happens behind the scenes
4. **Cache-Friendly**: Repeated patterns are recognized and reused
5. **Service-Mode First**: Designed for multi-tenant streaming services where processors come and go

**What Gets Optimized**:
- Thread creation/priority (which processors get dedicated threads vs. fused)
- Buffer sizes between processors (low-latency vs. throughput)
- Processor fusion (lightweight processors run inline to avoid context switching)

**What Users See**:
- Same API as today (`add_processor`, `connect`, `start`)
- Debug logs explaining optimization decisions
- Better performance with no code changes

## Architecture

### Key Components

```
┌─────────────────────────────────────────────────────────────┐
│ StreamRuntime                                               │
│ ┌─────────────────────────────────────────────────────────┐ │
│ │ GraphOptimizer                                          │ │
│ │ ┌──────────────┐  ┌──────────────┐  ┌────────────────┐ │ │
│ │ │ Strategy     │  │ Processor    │  │ Cache          │ │ │
│ │ │ Computation  │  │ Checksums    │  │ Management     │ │ │
│ │ └──────────────┘  └──────────────┘  └────────────────┘ │ │
│ └─────────────────────────────────────────────────────────┘ │
│                                                             │
│ Processors: HashMap<ProcessorId, ProcessorHandle>          │
│ Connections: Vec<Connection>                                │
│ Running Threads: HashMap<ProcessorId, JoinHandle>          │
└─────────────────────────────────────────────────────────────┘
```

### Threading Decisions

The optimizer chooses one of these strategies for each processor:

1. **Dedicated Thread**: Processor gets its own OS thread
   - Use for: Hardware I/O (camera, audio, display), heavy compute (encoding)
   - Priority levels: Realtime (audio I/O), High (video I/O), Normal (processing)

2. **Fused**: Processor runs inline in upstream processor's thread
   - Use for: Lightweight transformations (resize, color convert, simple filters)
   - Eliminates queue overhead and context switching
   - Requirements: Single input, single output, fast processing (<500μs)

3. **Pooled** *(Future)*: Multiple processors share a thread pool
   - Use for: Bursty/intermittent work, I/O-bound operations
   - Not implemented yet, but architecture supports it

## Implementation Approach

### 1. Graph Analysis

The optimizer builds a DAG (directed acyclic graph) from processors and connections:

```rust
pub struct GraphOptimizer {
    // Cache: graph checksum → optimization strategy
    strategy_cache: HashMap<GraphChecksum, OptimizationStrategy>,

    // Cache: processor config checksum → processor characteristics
    processor_cache: HashMap<ProcessorChecksum, ProcessorCharacteristics>,

    current_graph_checksum: Option<GraphChecksum>,
}

pub struct OptimizationStrategy {
    threading: HashMap<ProcessorId, ThreadingDecision>,
    buffer_sizes: HashMap<ConnectionId, usize>,
}

pub enum ThreadingDecision {
    Dedicated { priority: ThreadPriority },
    Fused { into: ProcessorId },
    // Future: Pooled { pool_id: usize },
}
```

**Analysis Steps**:
1. Compute graph checksum (for cache lookup)
2. If cached, return immediately
3. Otherwise, analyze topology:
   - Find sources (no inputs) → Always dedicated threads
   - Find sinks (no outputs) → Always dedicated threads
   - Analyze middle nodes → Fuse if lightweight, otherwise dedicated
4. Compute buffer sizes based on latency requirements
5. Cache the strategy

### 2. Processor Fusion

**Fusion Heuristics** (suggestions, not requirements):

A processor can be fused if:
- ✅ Exactly one input connection (no fan-in)
- ✅ Exactly one output connection (no fan-out)
- ✅ Processor type is "lightweight" (see whitelist below)
- ✅ No blocking operations (future: could detect via analysis)

**Whitelist of Fusable Processor Types** (initial suggestions):
- `ResizeProcessor` - Simple image resize
- `ColorConvertProcessor` - RGB ↔ YUV conversion
- `SimpleFilterProcessor` - Lightweight pixel operations
- `AudioChannelConverterProcessor` - Channel up/down mixing
- **Not fusable**: Camera, Display, Encoder (hardware I/O or heavy compute)

**Implementation Example**:
```rust
impl GraphOptimizer {
    fn can_be_fused(
        &self,
        proc_id: &ProcessorId,
        processors: &HashMap<ProcessorId, ProcessorHandle>,
        connections: &[Connection],
    ) -> bool {
        // Check topology constraints
        let input_count = connections.iter()
            .filter(|c| c.dest == *proc_id)
            .count();
        let output_count = connections.iter()
            .filter(|c| c.source == *proc_id)
            .count();

        if input_count != 1 || output_count != 1 {
            return false;
        }

        // Check processor type (whitelist approach)
        let proc_name = processors[proc_id].name();
        let fusable_types = [
            "ResizeProcessor",
            "ColorConvertProcessor",
            "SimpleFilterProcessor",
            "AudioChannelConverterProcessor",
        ];

        fusable_types.iter().any(|t| proc_name.contains(t))
    }
}
```

**Execution of Fused Processors**:
```rust
// Thread spawning with fusion support
fn spawn_dedicated_thread(&mut self, proc_id: ProcessorId, priority: ThreadPriority) -> Result<()> {
    let processor = self.processors.get(&proc_id).unwrap();
    let fused_children = self.find_fused_children(proc_id);

    std::thread::Builder::new()
        .name(processor.name())
        .spawn(move || {
            set_thread_priority(priority);

            loop {
                // Process main processor
                processor.process()?;

                // Process fused children inline (same thread)
                for child in &fused_children {
                    child.process()?;
                }

                if should_stop() { break; }
            }
        })?;

    Ok(())
}
```

### 3. Buffer Sizing

**Buffer Size Heuristics** (suggestions):

- **RT-to-RT** (source to sink, both hardware I/O): 2-3 frames
  - Minimize latency, assume both run at similar rates
  - Example: Camera → Display (direct preview)

- **RT-to-Normal** (source to processing): 4-8 frames
  - Allow headroom for variable processing time
  - Example: Camera → Encoder

- **Normal-to-RT** (processing to sink): 3-4 frames
  - Ensure sink never starves
  - Example: Decoder → Display

- **Normal-to-Normal** (processing to processing): 8-16 frames
  - Maximize throughput, latency less critical
  - Example: Filter → Encoder

**Implementation Example**:
```rust
impl GraphOptimizer {
    fn compute_buffer_sizes(
        &self,
        threading: &HashMap<ProcessorId, ThreadingDecision>,
        connections: &[Connection],
    ) -> HashMap<ConnectionId, usize> {
        let mut buffer_sizes = HashMap::new();

        for conn in connections {
            // Determine if source/dest are RT (high priority)
            let source_is_rt = matches!(
                threading.get(&conn.source),
                Some(ThreadingDecision::Dedicated { priority: ThreadPriority::High | ThreadPriority::Realtime })
            );

            let dest_is_rt = matches!(
                threading.get(&conn.dest),
                Some(ThreadingDecision::Dedicated { priority: ThreadPriority::High | ThreadPriority::Realtime })
            );

            let size = match (source_is_rt, dest_is_rt) {
                (true, true) => 3,   // RT-to-RT: low latency
                (true, false) => 8,  // RT-to-Normal: processing headroom
                (false, true) => 4,  // Normal-to-RT: don't starve sink
                (false, false) => 16, // Normal-to-Normal: throughput
            };

            buffer_sizes.insert(conn.id, size);
        }

        buffer_sizes
    }
}
```

### 4. Checksum-Based Caching

**Problem**: Graph analysis is fast (~100-500μs) but can be avoided entirely for repeated patterns.

**Solution**: Compute checksums of graph topology + processor configs, cache optimization results.

**Checksum Components**:
```rust
fn compute_graph_checksum(
    &self,
    processors: &HashMap<ProcessorId, ProcessorHandle>,
    connections: &[Connection],
) -> GraphChecksum {
    let mut hasher = DefaultHasher::new();

    // Hash all processors (sorted by ID for determinism)
    for (proc_id, handle) in processors.iter().sorted_by_key(|(id, _)| *id) {
        proc_id.hash(&mut hasher);
        handle.processor_type().hash(&mut hasher);
        handle.config_checksum().hash(&mut hasher);
    }

    // Hash all connections (sorted for determinism)
    for conn in connections.iter().sorted() {
        conn.source.hash(&mut hasher);
        conn.source_port.hash(&mut hasher);
        conn.dest.hash(&mut hasher);
        conn.dest_port.hash(&mut hasher);
    }

    GraphChecksum(hasher.finish())
}
```

**Cache Lookup**:
```rust
pub fn analyze(
    &mut self,
    processors: &HashMap<ProcessorId, ProcessorHandle>,
    connections: &[Connection],
) -> Result<OptimizationStrategy> {
    let checksum = self.compute_graph_checksum(processors, connections);

    // Check cache first
    if let Some(cached) = self.strategy_cache.get(&checksum) {
        tracing::info!("✅ Using cached optimization (checksum: {:x})", checksum.0);
        return Ok(cached.clone());
    }

    // Cache miss - compute fresh
    tracing::info!("🔍 Computing optimization (checksum: {:x})", checksum.0);
    let strategy = self.analyze_uncached(processors, connections)?;

    // Cache for future
    self.strategy_cache.insert(checksum, strategy.clone());

    Ok(strategy)
}
```

**Performance Impact**:
- **Without cache**: 100-500μs per graph change
- **With cache hit**: 5-10μs (hash lookup only)
- **Speedup**: 10-100x for repeated patterns

**Multi-Tenant Benefit**:
```
Service with 1000 users, all using camera→display:
- First user: Compute optimization (~200μs)
- Users 2-1000: Cache hits (~10μs each)
- Total: 200μs + 999 * 10μs ≈ 10ms
- vs without cache: 1000 * 200μs = 200ms
```

### 5. Dynamic Reoptimization

**Key Requirement**: StreamLib graphs can change at runtime (processors added/removed, connections modified).

**Approach**: Reoptimize on every graph change, apply changes incrementally.

```rust
impl StreamRuntime {
    pub fn add_processor_with_config<P: StreamProcessor>(
        &mut self,
        config: P::Config,
    ) -> Result<ProcessorHandle> {
        let proc_id = self.next_id();
        let handle = ProcessorHandle::new::<P>(proc_id, &config);

        // Add processor to graph
        self.processors.insert(proc_id, Box::new(P::from_config(config)));

        // If runtime is running, trigger reoptimization
        if self.is_running {
            tracing::info!("Processor added while running, reoptimizing...");
            self.reoptimize_and_apply()?;
        }

        Ok(handle)
    }

    fn reoptimize_and_apply(&mut self) -> Result<()> {
        // Compute new strategy (may hit cache)
        let new_strategy = self.optimizer.analyze(&self.processors, &self.connections)?;

        // Diff against current strategy
        let changes = self.compute_strategy_diff(&self.current_strategy, &new_strategy);

        // Apply changes incrementally
        self.apply_changes(changes)?;

        self.current_strategy = Some(new_strategy);
        Ok(())
    }
}
```

**Change Types**:
```rust
struct StrategyChanges {
    start: Vec<ProcessorId>,      // New processors to start
    stop: Vec<ProcessorId>,        // Removed processors to stop
    restart: Vec<ProcessorId>,     // Threading strategy changed, need restart
    resize_buffer: Vec<(ConnectionId, usize)>,  // Buffer size changes
}
```

**Incremental Application**:
- Stop threads that need to change
- Start new threads with new strategy
- Resize buffers (may require recreating connections)
- Minimize disruption to unchanged processors

## User Experience

### Simple Pipeline Example

```rust
// User code - no changes needed
let mut runtime = StreamRuntime::new();

let camera = runtime.add_processor_with_config::<CameraProcessor>(CameraConfig {
    device_id: None,
})?;

let display = runtime.add_processor_with_config::<DisplayProcessor>(DisplayConfig {
    width: 1920,
    height: 1080,
    title: Some("Camera Display".to_string()),
    scaling_mode: ScalingMode::Fit,
})?;

runtime.connect(
    camera.output_port::<VideoFrame>("video"),
    display.input_port::<VideoFrame>("video"),
)?;

runtime.start()?;
runtime.run()?;
```

**Output with optimization** (suggestion for logging format):
```
🔍 Analyzing processor graph...
   Graph topology:
     Processors: 2
     Connections: 1
     Sources: [CameraProcessor]
     Sinks: [DisplayProcessor]

   Threading decisions:
     CameraProcessor: Dedicated thread (High priority)
     DisplayProcessor: Dedicated thread (High priority)

   Buffer sizes:
     camera→display: 3 frames (low latency)

   Summary: 2 threads, 0 fused processors
▶️  Starting pipeline...
```

### Complex Pipeline with Fusion

```rust
// User adds lightweight filter
let camera = runtime.add_processor_with_config::<CameraProcessor>(/* ... */)?;
let resize = runtime.add_processor_with_config::<ResizeProcessor>(/* ... */)?;
let convert = runtime.add_processor_with_config::<ColorConvertProcessor>(/* ... */)?;
let display = runtime.add_processor_with_config::<DisplayProcessor>(/* ... */)?;

runtime.connect(camera.output_port("video"), resize.input_port("video"))?;
runtime.connect(resize.output_port("video"), convert.input_port("video"))?;
runtime.connect(convert.output_port("video"), display.input_port("video"))?;

runtime.start()?;
```

**Output with fusion optimization**:
```
🔍 Analyzing processor graph...
   Graph topology:
     Processors: 4
     Connections: 3
     Sources: [CameraProcessor]
     Sinks: [DisplayProcessor]

   Threading decisions:
     CameraProcessor: Dedicated thread (High priority)
     ResizeProcessor: Fused into CameraProcessor
     ColorConvertProcessor: Fused into CameraProcessor
     DisplayProcessor: Dedicated thread (High priority)

   Buffer sizes:
     camera→resize: eliminated (fused)
     resize→convert: eliminated (fused)
     convert→display: 3 frames (low latency)

   Summary: 2 threads (optimized from 4), 2 fused processors
▶️  Starting pipeline...
```

### Dynamic Graph Changes

```rust
// Runtime is already running with camera→display

// User adds filter mid-stream
let filter = runtime.add_processor_with_config::<BlurFilterProcessor>(/* ... */)?;

runtime.disconnect(camera.output_port("video"), display.input_port("video"))?;
runtime.connect(camera.output_port("video"), filter.input_port("video"))?;
runtime.connect(filter.output_port("video"), display.input_port("video"))?;

// Runtime automatically reoptimizes
```

**Output**:
```
INFO: Processor added while running, reoptimizing...
🔍 Analyzing processor graph...
   Threading decisions changed:
     BlurFilterProcessor: Fused into CameraProcessor (lightweight)

   Buffer sizes:
     camera→filter: eliminated (fused)
     filter→display: 3 frames (low latency)

▶️  Applied changes (no threads restarted)
```

## Implementation Tasks

### Phase 1: Foundation (Suggested Order)

**1.1 Basic Graph Analysis**
- [ ] Add `GraphOptimizer` struct to `core/graph_optimizer.rs`
- [ ] Implement DAG building from processors/connections
- [ ] Implement source/sink detection
- [ ] Add `ThreadingDecision` and `OptimizationStrategy` types
- [ ] Write tests for topology analysis

**1.2 Simple Threading Strategy**
- [ ] Implement basic strategy: sources/sinks get dedicated threads, middle gets normal priority
- [ ] Add buffer size heuristics (RT-to-RT=3, RT-to-Normal=8, etc.)
- [ ] Integrate optimizer into `StreamRuntime::start()`
- [ ] Add logging for optimization decisions
- [ ] Test with existing examples (camera-display, microphone-reverb-speaker)

**1.3 Thread Priority API**
- [ ] Add `set_thread_priority()` function for macOS (mach API)
- [ ] Add `ThreadPriority` enum (Realtime, High, Normal)
- [ ] Apply priorities when spawning processor threads
- [ ] Add feature flag `graph-optimization` to make it opt-in initially
- [ ] Test audio latency with RT priority vs without

### Phase 2: Processor Fusion (Suggested Order)

**2.1 Fusion Detection**
- [ ] Implement `can_be_fused()` heuristics
- [ ] Add whitelist of fusable processor types
- [ ] Update strategy computation to detect fusion opportunities
- [ ] Write tests for fusion detection logic

**2.2 Fused Execution**
- [ ] Modify thread spawning to include fused children
- [ ] Implement inline `process()` calls for fused processors
- [ ] Handle port connections for fused processors (eliminate intermediate queues)
- [ ] Test with camera→resize→display pipeline

**2.3 Fusion Validation**
- [ ] Add benchmarks comparing fused vs non-fused
- [ ] Measure CPU usage, context switches, latency
- [ ] Validate cache locality improvements
- [ ] Test edge cases (fan-in/fan-out prevention)

### Phase 3: Checksum Caching (Suggested Order)

**3.1 Checksum Infrastructure**
- [ ] Add `GraphChecksum` and `ProcessorChecksum` types
- [ ] Implement `compute_graph_checksum()`
- [ ] Add `Hash` derive to all processor configs
- [ ] Add `config_checksum()` to `ProcessorHandle`
- [ ] Write tests for checksum determinism

**3.2 Cache Implementation**
- [ ] Add `strategy_cache` HashMap to `GraphOptimizer`
- [ ] Implement cache lookup in `analyze()`
- [ ] Add cache eviction policy (LRU or size-based)
- [ ] Add metrics for cache hit/miss rates
- [ ] Test cache effectiveness with repeated patterns

**3.3 Persistent Cache (Optional)**
- [ ] Add `export_cache()` / `import_cache()` methods
- [ ] Implement serialization with `bincode` or `serde_json`
- [ ] Add cache file management to `StreamRuntime`
- [ ] Test cache persistence across restarts

### Phase 4: Dynamic Reoptimization (Suggested Order)

**4.1 Strategy Diffing**
- [ ] Implement `compute_strategy_diff()`
- [ ] Add `StrategyChanges` type
- [ ] Detect processors that need restart vs no change
- [ ] Test diff computation with various graph changes

**4.2 Incremental Updates**
- [ ] Implement `apply_changes()` to apply diffs
- [ ] Add thread stop/restart logic
- [ ] Add buffer resize logic
- [ ] Minimize disruption to unchanged processors
- [ ] Test with dynamic add/remove processors

**4.3 Service Mode**
- [ ] Test with empty runtime → add processors → remove processors
- [ ] Validate multi-tenant scenarios (many users, similar graphs)
- [ ] Add integration tests for dynamic graph changes
- [ ] Measure reoptimization overhead

### Phase 5: Advanced Features (Future, Optional)

**5.1 Profiling-Based Learning**
- [ ] Add `RuntimeProfiler` to track execution times
- [ ] Collect metrics on processor performance
- [ ] Detect bottlenecks and recommend optimizations
- [ ] Implement auto-tuning based on profiling data

**5.2 Thread Pooling**
- [ ] Add `ThreadingDecision::Pooled` variant
- [ ] Integrate Rayon or custom thread pool
- [ ] Implement work-stealing for bursty processors
- [ ] Test with I/O-bound processors

**5.3 Advanced Fusion**
- [ ] Implement cost-based fusion (analyze processor code)
- [ ] Add runtime measurement of fusion candidates
- [ ] Support multi-processor fusion chains
- [ ] Adaptive fusion based on observed performance

## Testing Strategy

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_detection() {
        // Create graph: Camera → Filter → Display
        // Assert: Camera is detected as source
    }

    #[test]
    fn test_sink_detection() {
        // Create graph: Camera → Filter → Display
        // Assert: Display is detected as sink
    }

    #[test]
    fn test_fusion_detection() {
        // Create graph: Camera → Resize → Display
        // Assert: Resize can be fused (single input/output)
    }

    #[test]
    fn test_no_fusion_fan_out() {
        // Create graph: Camera → Resize → Display + Encoder
        // Assert: Resize cannot be fused (multiple outputs)
    }

    #[test]
    fn test_checksum_determinism() {
        // Create same graph twice
        // Assert: Same checksum
    }

    #[test]
    fn test_cache_hit() {
        // Analyze graph
        // Remove all processors
        // Re-add same processors
        // Assert: Cache hit on second analysis
    }
}
```

### Integration Tests

```rust
#[test]
fn test_optimization_camera_display() {
    let mut runtime = StreamRuntime::new();

    let camera = runtime.add_processor_with_config::<CameraProcessor>(/* ... */)?;
    let display = runtime.add_processor_with_config::<DisplayProcessor>(/* ... */)?;

    runtime.connect(camera.output_port("video"), display.input_port("video"))?;
    runtime.start()?;

    // Assert: 2 threads spawned (camera + display)
    // Assert: Both have High priority
    // Assert: Buffer size = 3
}

#[test]
fn test_fusion_lightweight_filter() {
    let mut runtime = StreamRuntime::new();

    let camera = runtime.add_processor_with_config::<CameraProcessor>(/* ... */)?;
    let resize = runtime.add_processor_with_config::<ResizeProcessor>(/* ... */)?;
    let display = runtime.add_processor_with_config::<DisplayProcessor>(/* ... */)?;

    runtime.connect(camera.output_port("video"), resize.input_port("video"))?;
    runtime.connect(resize.output_port("video"), display.input_port("video"))?;
    runtime.start()?;

    // Assert: 2 threads spawned (not 3)
    // Assert: Resize is fused into camera thread
}

#[test]
fn test_dynamic_add_processor() {
    let mut runtime = StreamRuntime::new();
    runtime.start()?;

    // Add processor while running
    let camera = runtime.add_processor_with_config::<CameraProcessor>(/* ... */)?;

    // Assert: Thread started immediately
    // Assert: No errors
}
```

### Benchmarks

```rust
#[bench]
fn bench_graph_analysis_without_cache(b: &mut Bencher) {
    let mut optimizer = GraphOptimizer::new();
    let graph = create_complex_graph(); // 100 processors

    b.iter(|| {
        optimizer.strategy_cache.clear();
        optimizer.analyze(&graph.processors, &graph.connections)
    });
}

#[bench]
fn bench_graph_analysis_with_cache(b: &mut Bencher) {
    let mut optimizer = GraphOptimizer::new();
    let graph = create_complex_graph();

    // Prime cache
    optimizer.analyze(&graph.processors, &graph.connections)?;

    b.iter(|| {
        optimizer.analyze(&graph.processors, &graph.connections)
    });
}
```

## Configuration Options

While the optimizer should work with zero configuration, some users may want to override:

```rust
// Suggestion: Add runtime configuration (optional)
pub struct RuntimeConfig {
    pub enable_optimization: bool,           // Default: true
    pub enable_fusion: bool,                 // Default: true
    pub enable_cache: bool,                  // Default: true
    pub cache_max_entries: usize,            // Default: 1000
    pub cache_file: Option<PathBuf>,         // Default: None (no persistence)
    pub log_optimization: bool,              // Default: true
}

impl StreamRuntime {
    pub fn new_with_config(config: RuntimeConfig) -> Self {
        // ...
    }
}

// Usage
let runtime = StreamRuntime::new_with_config(RuntimeConfig {
    enable_fusion: false,  // Disable fusion for debugging
    log_optimization: true,
    ..Default::default()
});
```

## Performance Targets

**Optimization Overhead** (suggestions):
- Graph analysis (uncached): <500μs for 100 processors
- Graph analysis (cached): <10μs
- Dynamic reoptimization: <1ms for graph changes

**Runtime Impact**:
- Fusion should reduce CPU usage by 10-30% for pipelines with lightweight filters
- Thread priority should reduce audio latency jitter by 50%+
- Cache should enable sub-millisecond graph additions in multi-tenant scenarios

## Future Enhancements

**Ideas to Consider** (not requirements):

1. **Machine Learning-Based Optimization**
   - Learn from runtime behavior
   - Predict bottlenecks before they happen
   - Auto-tune buffer sizes based on observed patterns

2. **Cost-Based Analysis**
   - Profile processors to measure actual execution time
   - Use measurements to inform fusion decisions
   - Adaptive optimization based on workload

3. **GPU-Aware Scheduling**
   - Detect GPU-bound processors
   - Co-schedule GPU operations to maximize throughput
   - Minimize CPU-GPU transfers

4. **NUMA-Aware Threading**
   - Pin threads to specific CPU cores
   - Optimize for cache locality on multi-socket systems
   - Respect NUMA node boundaries

5. **Energy-Aware Optimization**
   - Prefer efficiency cores for background processing
   - Use performance cores for latency-critical paths
   - Dynamic CPU frequency scaling hints

## References

**Research Papers** (for context, not requirements):
- Kahn Process Networks (1974) - Theoretical foundation for dataflow
- StreamIt (MIT, 2002) - Compiler optimizations for stream graphs
- Halide (Stanford, 2012) - Separation of algorithm and schedule

**Similar Systems**:
- GStreamer - Manual queue placement, complex for users
- FFmpeg filters - Static graph, no runtime optimization
- Unreal Engine Task Graph - Game engine scheduling patterns

## Notes

This document describes a **suggested approach**, not a strict specification. The actual implementation may differ based on:
- Performance measurements showing different bottlenecks
- Platform-specific constraints (Windows/Linux threading differences)
- User feedback on what optimizations matter most
- Discovery of better heuristics through experimentation

The key principles (zero config, transparent optimization, dynamic graphs) should remain, but the specifics are flexible.
