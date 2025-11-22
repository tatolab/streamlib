# Graph Optimization Pre-Work Analysis

## Executive Summary

This document analyzes StreamLib's current architecture to identify what needs to be modified or added before implementing the graph optimization system described in `graph_optimization.md`.

**Key Finding**: StreamLib already has most of the foundational infrastructure needed for graph optimization. The main work involves:
1. Extending existing structures with metadata
2. Making processor configs hashable
3. Adding checksum computation
4. Modifying buffer capacity from hardcoded to dynamic

## Current Architecture Analysis

### 1. ProcessorId Type

**Current Implementation**:
```rust
// libs/streamlib/src/core/runtime.rs:13
pub type ProcessorId = String;

// Generated as: format!("processor_{}", counter)
// Example: "processor_0", "processor_1", ...
```

**Status for Optimization**: ✅ **Ready**
- Already `String` type
- Implements `Hash`, `Eq`, `Clone` (all required)
- Used consistently across codebase

**No changes needed** - can use as-is for HashMap keys and checksumming.

### 2. ProcessorHandle Structure

**Current Implementation**:
```rust
// libs/streamlib/src/core/handles.rs:7-15
#[derive(Debug, Clone)]
pub struct ProcessorHandle {
    pub(crate) id: ProcessorId,
}

impl ProcessorHandle {
    pub fn id(&self) -> &ProcessorId { &self.id }

    pub fn output_port<T>(&self, name: &str) -> OutputPortRef<T> { /* ... */ }
    pub fn input_port<T>(&self, name: &str) -> InputPortRef<T> { /* ... */ }
}
```

**Status for Optimization**: ⚠️ **Needs Extension**

**Missing for optimization**:
- ❌ No processor type name storage
- ❌ No config checksum
- ❌ No creation timestamp
- ❌ No way to query processor characteristics

**Required Changes**:
```rust
#[derive(Debug, Clone)]
pub struct ProcessorHandle {
    pub(crate) id: ProcessorId,

    // NEW: Add these fields
    processor_type: String,           // std::any::type_name::<P>()
    config_checksum: u64,             // Hash of config
    created_at: std::time::Instant,
}

impl ProcessorHandle {
    pub(crate) fn new<P: StreamProcessor>(
        id: ProcessorId,
        config: &P::Config,
    ) -> Self
    where
        P::Config: Hash,
    {
        let processor_type = std::any::type_name::<P>().to_string();

        let mut hasher = DefaultHasher::new();
        processor_type.hash(&mut hasher);
        config.hash(&mut hasher);
        let config_checksum = hasher.finish();

        Self {
            id,
            processor_type,
            config_checksum,
            created_at: Instant::now(),
        }
    }

    // NEW: Accessor methods
    pub fn processor_type(&self) -> &str { &self.processor_type }
    pub fn config_checksum(&self) -> u64 { self.config_checksum }
}
```

**Impact**: Medium - requires updating `add_processor_with_config()` to pass generic type info to handle construction.

### 3. Connection Structure

**Current Implementation**:
```rust
// libs/streamlib/src/core/runtime.rs:47-53
#[derive(Debug, Clone)]
pub struct Connection {
    pub id: ConnectionId,         // "connection_0", "connection_1", ...
    pub from_port: String,        // "processor_0.video_out"
    pub to_port: String,          // "processor_1.video_in"
    pub created_at: Instant,
}
```

**Status for Optimization**: ⚠️ **Needs Extension**

**Missing for optimization**:
- ❌ No source/dest ProcessorId (only full port addresses)
- ❌ No source/dest port names (only combined strings)
- ❌ No buffer capacity tracking
- ❌ No PortType information

**Required Changes**:
```rust
#[derive(Debug, Clone)]
pub struct Connection {
    pub id: ConnectionId,

    // Decomposed addresses for fast queries
    pub source_processor: ProcessorId,
    pub source_port: String,
    pub dest_processor: ProcessorId,
    pub dest_port: String,

    // Original combined addresses (for backwards compat)
    pub from_port: String,
    pub to_port: String,

    // NEW: Optimization metadata
    pub port_type: PortType,       // Video, Audio, or Data
    pub buffer_capacity: usize,    // Current buffer size
    pub created_at: Instant,
}
```

**Impact**: Medium - requires parsing port addresses during connection creation.

**Extraction helper**:
```rust
impl Connection {
    pub fn new(
        id: ConnectionId,
        from_port: String,  // "processor_0.video_out"
        to_port: String,     // "processor_1.video_in"
        port_type: PortType,
        buffer_capacity: usize,
    ) -> Self {
        // Parse processor IDs and port names
        let (source_processor, source_port) = from_port
            .split_once('.')
            .unwrap_or(("", ""));
        let (dest_processor, dest_port) = to_port
            .split_once('.')
            .unwrap_or(("", ""));

        Self {
            id,
            source_processor: source_processor.to_string(),
            source_port: source_port.to_string(),
            dest_processor: dest_processor.to_string(),
            dest_port: dest_port.to_string(),
            from_port,
            to_port,
            port_type,
            buffer_capacity,
            created_at: Instant::now(),
        }
    }
}
```

### 4. StreamRuntime Structure

**Current Implementation**:
```rust
// libs/streamlib/src/core/runtime.rs:68-81
pub struct StreamRuntime {
    pub(crate) processors: Arc<Mutex<HashMap<ProcessorId, RuntimeProcessorHandle>>>,
    pending_processors: Vec<(ProcessorId, DynProcessor, Receiver<()>)>,
    handler_threads: Vec<JoinHandle<()>>,
    running: bool,
    event_loop: Option<EventLoopFn>,
    gpu_context: Option<GpuContext>,
    next_processor_id: usize,
    pub(crate) connections: Arc<Mutex<HashMap<ConnectionId, Connection>>>,
    next_connection_id: usize,
    pending_connections: Vec<PendingConnection>,
    bus: Bus,
}
```

**Status for Optimization**: ✅ **Mostly Ready**

**Already has**:
- ✅ `processors` HashMap (for graph topology)
- ✅ `connections` HashMap (for edges)
- ✅ Thread handles storage via `RuntimeProcessorHandle`
- ✅ `running` flag to detect runtime state
- ✅ Graceful shutdown mechanism via `shutdown_tx` channels

**Missing for optimization**:
- ❌ No graph optimizer instance
- ❌ No fast connection lookup by processor (need index)
- ❌ No processor metadata (separate from RuntimeProcessorHandle)

**Required Changes**:
```rust
pub struct StreamRuntime {
    // Existing fields...
    pub(crate) processors: Arc<Mutex<HashMap<ProcessorId, RuntimeProcessorHandle>>>,
    pub(crate) connections: Arc<Mutex<HashMap<ConnectionId, Connection>>>,

    // NEW: Optimization infrastructure
    #[cfg(feature = "graph-optimization")]
    optimizer: GraphOptimizer,

    #[cfg(feature = "graph-optimization")]
    processor_handles: HashMap<ProcessorId, ProcessorHandle>,  // Separate from RuntimeProcessorHandle

    #[cfg(feature = "graph-optimization")]
    connection_index: HashMap<ProcessorId, Vec<ConnectionId>>,  // Fast lookup

    #[cfg(feature = "graph-optimization")]
    current_strategy: Option<OptimizationStrategy>,

    // Existing fields...
    running: bool,
    bus: Bus,
}
```

**Impact**: Small - mostly additive changes behind feature flag.

### 5. Thread Management

**Current Implementation**:
```rust
// libs/streamlib/src/core/runtime.rs:35-43
pub(crate) struct RuntimeProcessorHandle {
    pub id: ProcessorId,
    pub name: String,
    pub(crate) thread: Option<JoinHandle<()>>,
    pub(crate) shutdown_tx: Sender<()>,
    pub(crate) wakeup_tx: Sender<WakeupEvent>,
    pub(crate) status: Arc<Mutex<ProcessorStatus>>,
    pub(crate) processor: Option<Arc<Mutex<DynProcessor>>>,
}
```

**Status for Optimization**: ✅ **Excellent**

**Already has**:
- ✅ Thread handle storage (`thread: Option<JoinHandle<()>>`)
- ✅ Graceful shutdown mechanism (`shutdown_tx`)
- ✅ Processor status tracking (`ProcessorStatus`)
- ✅ Wakeup mechanism for push-mode processors (`wakeup_tx`)
- ✅ Access to processor instance via `Arc<Mutex<DynProcessor>>`

**Shutdown is straightforward**:
```rust
// From remove_processor():
shutdown_tx.send(())?;  // Signal shutdown
handle.join()?;         // Wait for thread to finish
```

**No changes needed** - can use as-is for starting/stopping threads.

### 6. Thread Priority System

**Current Implementation**:
```rust
// libs/streamlib/src/core/scheduling/priority.rs:4-11
pub enum ThreadPriority {
    RealTime,    // < 10ms latency
    High,        // < 33ms latency
    Normal,      // No strict latency
}
```

**Implementation in apple/thread_priority.rs**:
```rust
pub fn apply_thread_priority(priority: ThreadPriority) -> Result<()> {
    match priority {
        ThreadPriority::RealTime => set_realtime_priority(),  // Mach API
        ThreadPriority::High => set_high_priority(),           // POSIX SCHED_RR
        ThreadPriority::Normal => Ok(()),                      // Default
    }
}
```

**Status for Optimization**: ✅ **Perfect - Already Implemented!**

**Already in use**:
```rust
// libs/streamlib/src/core/runtime.rs:264-270
let sched_config = processor.scheduling_config();

let handle = std::thread::spawn(move || {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        if let Err(e) = crate::apple::thread_priority::apply_thread_priority(sched_config.priority) {
            tracing::warn!("Failed to apply thread priority: {}", e);
        }
    }
    // ... processor runs ...
});
```

**What this means**:
- ✅ Processors already declare priority via `scheduling_config()`
- ✅ Runtime already applies priority when spawning threads
- ✅ macOS/iOS implementation exists and works

**For optimization**: Just need to modify what priority each processor requests based on graph analysis.

### 7. Buffer Capacity Configuration

**Current Implementation**:
```rust
// libs/streamlib/src/core/runtime.rs:565
let capacity = source_port_type.default_capacity();

// libs/streamlib/src/core/bus/ports.rs:78-84
impl PortType {
    pub fn default_capacity(&self) -> usize {
        match self {
            PortType::Video => 3,
            PortType::Audio => 32,
            PortType::Data => 16,
        }
    }
}
```

**Status for Optimization**: ⚠️ **Needs Modification**

**Current behavior**: Hardcoded capacities based on port type.

**Required for optimization**: Dynamic capacity based on graph analysis.

**Required Changes**:
```rust
// In StreamRuntime::connect_at_runtime()
let capacity = if let Some(optimizer) = &self.optimizer {
    optimizer.recommend_buffer_size(
        source_proc_id,
        dest_proc_id,
    ).unwrap_or_else(|| source_port_type.default_capacity())
} else {
    source_port_type.default_capacity()
};

let (producer, consumer) = self.bus.create_connection::<T>(
    source_addr,
    dest_addr,
    capacity,  // Now uses optimized capacity
)?;
```

**Impact**: Small - single-line change, falls back to defaults if optimizer disabled.

### 8. Bus and Connection System

**Current Implementation**:
```rust
// libs/streamlib/src/core/bus/bus.rs:21-30
pub fn create_connection<T: PortMessage + 'static>(
    &self,
    source: PortAddress,
    dest: PortAddress,
    capacity: usize,  // ← Capacity already parameterized!
) -> Result<(OwnedProducer<T>, OwnedConsumer<T>)>
```

**Status for Optimization**: ✅ **Perfect**

**Already supports**:
- ✅ Variable capacity per connection
- ✅ Lock-free ring buffers via `rtrb`
- ✅ Owned producer/consumer (no Arc/Mutex in hot path)

**No changes needed** - just pass optimized capacity values.

**Note on buffer resizing**: `rtrb` doesn't support resizing. For dynamic changes, need to:
1. Disconnect old connection
2. Create new connection with new capacity
3. Wire new producer/consumer

This is complex - **Phase 1 should skip dynamic resizing**, just set optimal size at creation.

### 9. Processor Configs - Hashability

**Current State**: Mixed - some configs are hashable, some aren't.

**Sample of existing configs**:

```rust
// ✅ Already hashable (has Serialize/Deserialize)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioResamplerConfig {
    pub source_sample_rate: u32,
    pub target_sample_rate: u32,
    pub quality: ResamplingQuality,  // enum
}

// ❓ Unknown - need to check
pub struct CameraConfig {
    pub device_id: Option<String>,
}

pub struct DisplayConfig {
    pub width: u32,
    pub height: u32,
    pub title: Option<String>,
    pub scaling_mode: ScalingMode,
}
```

**Required Action**:
- [ ] Audit all processor configs
- [ ] Add `#[derive(Hash)]` or manual `impl Hash` to all configs
- [ ] Handle special cases (PathBuf, function pointers, etc.)

**Pattern for configs with Hash**:
```rust
#[derive(Debug, Clone, Serialize, Deserialize, Hash)]
pub struct MyProcessorConfig {
    pub field1: u32,
    pub field2: String,
}
```

**Pattern for configs with non-hashable fields**:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClapEffectConfig {
    pub plugin_path: PathBuf,  // PathBuf doesn't implement Hash
    pub sample_rate: u32,
}

impl Hash for ClapEffectConfig {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.plugin_path.to_str().hash(state);  // Convert to str
        self.sample_rate.hash(state);
    }
}
```

### 10. Processor Type Introspection

**Current Capability**:
```rust
// In add_boxed_processor():
let processor_type = std::any::type_name_of_val(&*processor).to_string();
```

**Status**: ✅ **Works**

**Available at runtime**: Yes, via `std::any::type_name_of_val()`.

**For generic context**:
```rust
pub fn add_processor_with_config<P: StreamProcessor>(
    &mut self,
    config: P::Config,
) -> Result<ProcessorHandle> {
    let processor_type = std::any::type_name::<P>();  // ← Compile-time, very fast
    // ...
}
```

**Output format**: `"streamlib::core::processors::AudioResamplerProcessor"`

**For optimization**: Can extract short name or use full path for matching.

### 11. Scheduling Configuration

**Current Implementation**:
```rust
// libs/streamlib/src/core/scheduling/mode.rs:4-11
pub enum SchedulingMode {
    Loop,   // Tight loop with sleep
    Push,   // Event-driven (wakeup on data)
    Pull,   // Processor manages callbacks
}

// libs/streamlib/src/core/scheduling/config.rs
pub struct SchedulingConfig {
    pub mode: SchedulingMode,
    pub priority: ThreadPriority,
}

// Processors can override
impl StreamProcessor for MyProcessor {
    fn scheduling_config(&self) -> SchedulingConfig {
        SchedulingConfig {
            mode: SchedulingMode::Push,
            priority: ThreadPriority::High,
        }
    }
}
```

**Status for Optimization**: ✅ **Already Great**

**How runtime uses it**:
```rust
// libs/streamlib/src/core/runtime.rs:250-253
let sched_config = {
    let processor = processor_arc.lock();
    processor.scheduling_config()
};
```

**For optimization**: Can query processor's declared config, but optimizer may override priority.

**Decision point**: Should optimizer:
- **Option A**: Override processor's declared priority based on graph position
- **Option B**: Respect processor's declared priority, only optimize buffer sizes

**Recommendation**: Option A - optimizer knows better based on topology.

## Prerequisites Checklist

### Must Have Before Starting (Blocking)

- [ ] **ProcessorHandle stores type name and config checksum**
  - Modify `ProcessorHandle::new()` to accept generic type parameter
  - Compute checksum from config
  - Store both in handle
  - **Difficulty**: Medium
  - **Files**: `libs/streamlib/src/core/handles.rs`, `libs/streamlib/src/core/runtime.rs`

- [ ] **All processor configs implement Hash**
  - Audit all configs in `core/processors/` and `apple/processors/`
  - Add `#[derive(Hash)]` or manual implementation
  - Test that checksums are deterministic
  - **Difficulty**: Low (tedious but straightforward)
  - **Files**: All `*_config` structs across codebase

- [ ] **Connection stores decomposed processor IDs and capacity**
  - Add `source_processor`, `dest_processor`, `buffer_capacity` fields
  - Parse addresses during construction
  - **Difficulty**: Low
  - **Files**: `libs/streamlib/src/core/runtime.rs`

- [ ] **Fast connection lookup by processor**
  - Build index: `HashMap<ProcessorId, Vec<ConnectionId>>`
  - Update when connections added/removed
  - **Difficulty**: Low
  - **Files**: `libs/streamlib/src/core/runtime.rs`

- [ ] **Dynamic buffer capacity in connect()**
  - Replace `default_capacity()` with optimizer query
  - Fall back to defaults if optimization disabled
  - **Difficulty**: Very Low (one-line change)
  - **Files**: `libs/streamlib/src/core/runtime.rs:565`

### Should Have (Important but Not Blocking)

- [ ] **Feature flag for optimization**
  - Add `graph-optimization` feature to `Cargo.toml`
  - Wrap optimizer code in `#[cfg(feature = "graph-optimization")]`
  - **Difficulty**: Very Low
  - **Files**: `libs/streamlib/Cargo.toml`

- [ ] **ProcessorId implements Hash + Ord**
  - Currently `String`, already implements both ✅
  - **Difficulty**: N/A (already done)

- [ ] **Deterministic processor iteration**
  - HashMap iteration is already deterministic in same process
  - For checksums, need to sort before hashing
  - **Difficulty**: Very Low
  - **Files**: Graph optimizer implementation

- [ ] **Error handling for optimization failures**
  - Decide: fail loudly or fall back to defaults?
  - **Recommendation**: Fall back to defaults, log warning
  - **Difficulty**: Low
  - **Files**: Graph optimizer implementation

### Nice to Have (Can Add Later)

- [ ] **Dynamic buffer resizing**
  - Complex - requires disconnect/reconnect
  - **Recommendation**: Skip in Phase 1
  - **Difficulty**: High

- [ ] **Processor characteristics API**
  - Processors self-report `is_lightweight()`, `estimated_latency_us()`, etc.
  - **Recommendation**: Use whitelist approach initially
  - **Difficulty**: Medium

- [ ] **Persistent cache**
  - Save/load optimization cache across restarts
  - **Recommendation**: Add after basic caching works
  - **Difficulty**: Low (use bincode)

- [ ] **Port introspection via macro**
  - Auto-generate port metadata from `#[input]`/`#[output]` attributes
  - **Recommendation**: Hardcode initially, add if needed
  - **Difficulty**: Medium (macro changes)

## Open Questions Requiring Decisions

### 1. ProcessorHandle Generic Type Access

**Question**: How to pass generic type `P` to `ProcessorHandle::new()` from `add_processor_with_config()`?

**Current Code**:
```rust
pub fn add_processor_with_config<P: StreamProcessor>(
    &mut self,
    config: P::Config,
) -> Result<ProcessorHandle> {
    let processor = P::from_config(config)?;
    let processor_dyn: DynProcessor = Box::new(processor);
    self.add_boxed_processor(processor_dyn)  // ← Generic type lost here!
}
```

**Solutions**:

**Option A**: Create handle before boxing
```rust
pub fn add_processor_with_config<P: StreamProcessor>(
    &mut self,
    config: P::Config,
) -> Result<ProcessorHandle>
where
    P::Config: Hash,
{
    let id = format!("processor_{}", self.next_processor_id);
    self.next_processor_id += 1;

    // Create handle with generic type info
    let handle = ProcessorHandle::new::<P>(id.clone(), &config);

    // Then box processor
    let processor = P::from_config(config)?;
    let processor_dyn: DynProcessor = Box::new(processor);

    // Add to runtime with both handle and boxed processor
    self.add_boxed_processor_with_handle(processor_dyn, handle.clone())?;

    Ok(handle)
}
```

**Option B**: Store type name in DynStreamElement trait
```rust
trait DynStreamElement {
    fn processor_type_name(&self) -> &'static str;
    // ...
}

// Macro generates:
impl DynStreamElement for MyProcessor {
    fn processor_type_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}
```

**Recommendation**: Option A - cleaner separation, no trait changes needed.

### 2. Should Optimizer Override Processor-Declared Priority?

**Current**: Processors declare priority via `scheduling_config()`.

**Question**: If optimizer determines processor should have different priority, override?

**Example Conflict**:
```rust
// Processor declares:
fn scheduling_config(&self) -> SchedulingConfig {
    SchedulingConfig {
        mode: SchedulingMode::Loop,
        priority: ThreadPriority::Normal,  // ← Processor says Normal
    }
}

// Optimizer determines:
// This processor is a sink → Should be High priority
```

**Options**:

**Option A**: Optimizer always wins
- **Pro**: Optimizer has global view, knows better
- **Con**: Processor author's intent ignored

**Option B**: Processor declaration is hint, optimizer adjusts
- **Pro**: Respects processor author
- **Con**: May lead to suboptimal scheduling

**Option C**: Processor can declare "require" vs "prefer"
```rust
pub struct PriorityHint {
    priority: ThreadPriority,
    required: bool,  // If true, optimizer can't override
}
```

**Recommendation**: Option A for Phase 1 (simple), add Option C later if needed.

### 3. When to Reoptimize?

**Options**:

**Option A**: On every graph change (add/remove processor, add/remove connection)
- **Pro**: Always optimal
- **Con**: May be expensive if many rapid changes

**Option B**: Debounced reoptimization (wait 100ms after last change)
- **Pro**: Handles bursts of changes efficiently
- **Con**: More complex

**Option C**: Explicit `runtime.optimize()` call by user
- **Pro**: User controls when
- **Con**: User has to remember to call it

**Recommendation**: Option A for Phase 1 (simple), add Option B if performance issues arise.

### 4. Feature Flag Default?

**Question**: Should `graph-optimization` feature be enabled by default?

**Option A**: Default enabled
- **Pro**: Users get optimization automatically
- **Con**: Potential bugs affect everyone

**Option B**: Opt-in initially
- **Pro**: Safe rollout, can test thoroughly
- **Con**: Users have to enable explicitly

**Recommendation**: **Option B** - opt-in via feature flag initially, default-enable after proven stable.

## Implementation Order

Based on prerequisites and dependencies, recommended order:

### Phase 0: Preparation (Before Code)
1. Add `graph-optimization` feature to Cargo.toml
2. Audit all processor configs, list which need Hash implementations
3. Design ProcessorHandle extension API

### Phase 1: Foundation (Week 1)
1. Add Hash to all processor configs
2. Extend ProcessorHandle with type name and checksum
3. Extend Connection with decomposed fields and capacity
4. Modify add_processor_with_config() to create extended handle
5. Add connection index to StreamRuntime
6. Write tests for checksum determinism

### Phase 2: Optimizer Core (Week 2)
1. Create GraphOptimizer struct and basic API
2. Implement topology analysis (sources, sinks, DAG building)
3. Implement simple strategy: sources/sinks = High, others = Normal
4. Implement buffer size heuristics
5. Write tests for topology detection

### Phase 3: Integration (Week 3)
1. Add optimizer instance to StreamRuntime
2. Call optimizer in start() method
3. Apply optimizations (thread priority, buffer sizes)
4. Add logging for optimization decisions
5. Test with camera-display example

### Phase 4: Caching (Week 4)
1. Implement graph checksum computation
2. Add strategy cache to optimizer
3. Implement cache lookup in analyze()
4. Test cache hit rates with repeated patterns
5. Add metrics/logging for cache performance

### Phase 5: Dynamic Reoptimization (Week 5)
1. Implement strategy diffing
2. Add incremental update logic
3. Handle processor add/remove during runtime
4. Test with dynamic graph changes
5. Measure reoptimization overhead

## Dependencies and External Crates

### Already Available
- ✅ `rtrb` - Lock-free ring buffers
- ✅ `parking_lot` - Fast Mutex/RwLock
- ✅ `crossbeam_channel` - Shutdown/wakeup channels
- ✅ `mach2` - macOS thread priority APIs

### Need to Add
```toml
[dependencies]
# For graph analysis (DAG, topological sort)
petgraph = "0.6"

# Fast hashing (faster than DefaultHasher)
ahash = "0.8"

[target.'cfg(target_os = "linux")'.dependencies]
libc = "0.2"  # For pthread thread priority

[target.'cfg(target_os = "windows")'.dependencies]
winapi = { version = "0.3", features = ["processthreadsapi"] }

# Optional for cache persistence
[dependencies]
bincode = { version = "1.3", optional = true }

[features]
graph-optimization = ["petgraph", "ahash"]
persistent-cache = ["graph-optimization", "bincode"]
```

## Risk Assessment

### Low Risk
- ✅ Adding Hash to configs - purely additive, no behavior change
- ✅ Extending ProcessorHandle - new fields, old code unaffected
- ✅ Feature flag approach - optimization is optional
- ✅ Using existing thread priority system - already works

### Medium Risk
- ⚠️ Modifying buffer capacity logic - could affect performance if wrong
- ⚠️ Graph analysis bugs - could assign wrong priorities
- ⚠️ Cache invalidation bugs - stale strategies applied

### High Risk
- ❌ Dynamic buffer resizing - complex, easy to break
- ❌ Overriding processor priorities - could break working systems

**Mitigation**:
- Start with feature flag disabled by default
- Extensive testing with existing examples
- Fall back to defaults on any optimizer errors
- Skip high-risk features (buffer resizing) in Phase 1

## Success Criteria

### Phase 1 Success
- [ ] camera-display example works with optimization enabled
- [ ] Logs show: "CameraProcessor: High priority, DisplayProcessor: High priority"
- [ ] Logs show: "Buffer camera→display: 3 frames"
- [ ] No performance regression vs optimization disabled

### Phase 2 Success
- [ ] microphone-reverb-speaker works with optimization
- [ ] Cache hit on second run with identical config
- [ ] Thread count unchanged (no fusion yet)
- [ ] Priority assignments logged correctly

### Phase 3 Success
- [ ] Dynamic processor add/remove works
- [ ] Reoptimization completes in <1ms
- [ ] Service mode test: 100 users with identical pipelines, 99 cache hits

## Conclusion

**StreamLib is well-positioned for graph optimization.** Most prerequisites are already in place:
- Thread management ✅
- Priority system ✅
- Lock-free bus ✅
- Graceful shutdown ✅

**Main work needed**:
1. Metadata extension (ProcessorHandle, Connection)
2. Config hashability audit
3. Optimizer implementation
4. Integration and testing

**Estimated effort**: 4-6 weeks for full implementation through Phase 4 (caching).

**Recommended approach**: Feature-flagged, incremental rollout with existing examples as integration tests.
