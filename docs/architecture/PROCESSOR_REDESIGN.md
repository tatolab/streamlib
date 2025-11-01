# Processor Architecture Redesign - Event-Driven with Declarative Scheduling

**Status**: Design Proposal
**Created**: 2025-01-11
**Author**: Architecture Team
**Target Release**: v2.0.0 (Breaking Change)

---

## Executive Summary

This document proposes a fundamental redesign of streamlib's processor architecture, replacing the current monolithic `StreamProcessor` trait with a **3-tier specialized system** inspired by GStreamer's architecture, combined with a **declarative scheduling API** that eliminates manual thread and timer management.

### The Problem

The current architecture suffers from:
1. **Buffer overrun issues** - AudioMixer buffer reaches 8000%+ capacity (700K samples vs expected 2K)
2. **Duplicate processing** - Processors wake up 3x per timer tick due to event confusion
3. **Mixed responsibilities** - Single `StreamProcessor` trait handles all use cases (sources, sinks, transforms)
4. **Manual scheduling** - Developers must manually manage threads, timers, and wakeup logic
5. **Architecture mismatch** - Timer groups partially solve synchronization but don't align with production systems (GStreamer, PipeWire)

### The Solution

**3-Tier Processor System** (based on GStreamer's base classes):
- **`StreamSource`** - Generates data (no inputs, only outputs)
  Examples: TestToneGenerator, CameraProcessor, microphone capture
- **`StreamSink`** - Consumes data (only inputs, no outputs)
  Examples: AudioOutputProcessor, DisplayProcessor, speaker output
- **`StreamTransform`** - Processes data (inputs → outputs, any configuration)
  Examples: ClapEffectProcessor (1→1), AudioMixer (N→1), Tee (1→N)

**Declarative Scheduling API** (runtime-provided infrastructure):
```rust
#[derive(StreamSource)]
#[scheduling(mode = "loop", clock = "audio", rate_hz = 23.44)]
struct TestToneGenerator {
    #[output()]
    audio: StreamOutput<AudioFrame>,
    frequency: f64,
}

impl TestToneGenerator {
    fn generate(&mut self) -> Result<AudioFrame> {
        // Runtime handles loop, clock sync, timing
        // Developer just implements generation logic
    }
}
```

### Key Benefits

| Current Architecture | New Architecture |
|---------------------|------------------|
| ❌ Manual timer thread management | ✅ Declarative `#[scheduling(...)]` attributes |
| ❌ Timer groups with duplicate wakeups | ✅ Sources self-regulate via clock |
| ❌ Mixed source/sink/transform logic | ✅ Clear separation of concerns |
| ❌ 90+ lines of boilerplate per processor | ✅ 10-15 lines with macros |
| ❌ Buffer overruns (8000%+ capacity) | ✅ Self-regulating (stays at ~2048 samples) |
| ❌ Manual clock synchronization | ✅ Runtime provides clock infrastructure |

### Impact

- **Breaking Change**: All processors must migrate to new traits
- **Timeline**: ~4 weeks implementation + testing
- **Migration**: Automated where possible via macros
- **Payoff**: Simpler API, better performance, production-ready architecture

---

## Table of Contents

1. [Background & Motivation](#background--motivation)
2. [GStreamer Research Findings](#gstreamer-research-findings)
3. [StreamElement Base Trait](#streamelement-base-trait)
4. [3-Tier Processor Architecture](#3-tier-processor-architecture)
5. [Declarative Scheduling API](#declarative-scheduling-api)
6. [Clock Infrastructure](#clock-infrastructure)
7. [Runtime Scheduling Infrastructure](#runtime-scheduling-infrastructure)
8. [Event Flow & Data Movement](#event-flow--data-movement)
9. [How This Fixes Buffer Overrun](#how-this-fixes-buffer-overrun)
10. [Migration Path](#migration-path)
11. [Implementation Plan](#implementation-plan)
12. [API Design Details](#api-design-details)
13. [Testing Strategy](#testing-strategy)
14. [Success Criteria](#success-criteria)
15. [Open Questions](#open-questions)
16. [References](#references)

---

## Background & Motivation

### Current Issues

#### 1. Buffer Overrun Problem

**Symptoms** (from audio-mixer-demo logs):
```
⚠️ BUFFER OVERRUN - buffer at 8000.0% capacity (700,000 samples when expecting ~2,048)

[AudioMixer] process() called - frame_numbers: [450, 450, 450], timestamps: [...]
[AudioMixer] process() called - frame_numbers: [450, 450, 450], timestamps: [...]  ← 0.6ms later!
[AudioMixer] process() called - frame_numbers: [450, 450, 450], timestamps: [...]  ← 0.6ms later!
```

**Root Cause**:
- 3 TestToneGenerators + AudioMixer all in same timer group ("audio_master")
- Timer tick @ 23.44 Hz wakes all 4 processors simultaneously
- Each TestTone writes to output → sends `WakeupEvent::DataAvailable` to AudioMixer
- AudioMixer receives 3 DataAvailable events
- AudioMixer processes 3 times (once per event)
- Mixer outputs 3 frames instead of 1
- Buffer floods to 8000%+ capacity
- Audio sounds choppy and distorted

**Attempted Fixes** (didn't solve root cause):
1. Timestamp-based deduplication - Failed because each TestTone generates slightly different timestamps (0.6ms apart due to thread scheduling)
2. Increased frame tolerance - Symptom treatment, doesn't address core issue
3. Timer groups - Synchronized wakeups but didn't prevent multiple events per tick

#### 2. Monolithic StreamProcessor Trait

Current `StreamProcessor` handles ALL processor types:
```rust
pub trait StreamProcessor {
    fn process(&mut self) -> Result<()>;
    // ... shared by sources, sinks, transforms, everything
}
```

**Problems**:
- ❌ Sources have no inputs but must implement input logic
- ❌ Sinks have no outputs but must implement output logic
- ❌ Transforms need different wakeup semantics than sources
- ❌ No type-level distinction between processor roles
- ❌ Runtime can't optimize based on processor type

#### 3. Manual Scheduling Management

Developers must manually:
```rust
// Example: TestToneGenerator today
impl TestToneGenerator {
    pub fn new(freq: f64, sample_rate: u32, amplitude: f64) -> Self {
        // Manual timer calculations
        let buffer_size = Self::BUFFER_SIZE;
        let timer_group_id = None; // Must manually configure

        Self {
            timer_group_id,  // Manual timer group assignment
            // ... 90 lines of boilerplate
        }
    }

    fn timer_rate_hz(&self) -> f64 {
        // Manual timer rate calculation
        self.sample_rate as f64 / self.buffer_size as f64
    }
}
```

**Problems**:
- ❌ Developers must understand timer groups, clock domains, wakeup events
- ❌ Easy to misconfigure (wrong timer rate, wrong group, etc.)
- ❌ No runtime validation of scheduling configuration
- ❌ Difficult to test scheduling logic independently

#### 4. Architecture Mismatch with Production Systems

streamlib's current architecture differs from proven systems:

| System | Architecture | streamlib Current | streamlib Goal |
|--------|--------------|------------------|----------------|
| **GStreamer** | Sources loop continuously, transforms reactive, clock is passive reference | Timer-driven tick model, mixed scheduling | Align with GStreamer |
| **PipeWire** | Graph-based scheduling with clock domains | Partial (timer groups) | Full alignment |
| **WebRTC** | Jitter buffers, adaptive playback | No jitter handling | Add for network sources |

### Why Redesign Now?

1. **Foundation for future features**: Network streaming, jitter buffers, adaptive playback all require proper architecture
2. **Before public release**: Breaking change easier before v1.0
3. **Developer experience**: Declarative API makes streamlib accessible to more developers
4. **Production readiness**: Current architecture won't scale to complex pipelines (multi-camera, multi-mic, effects chains)
5. **Lessons learned**: We now understand GStreamer's design after deep research

---

## GStreamer Research Findings

We conducted extensive research into GStreamer's architecture to inform this redesign. Key findings:

### 1. Sources Run in Continuous Loops

**From `gstbasesrc.c`**:
```c
// GstBaseSrc starts a dedicated thread for push-mode sources
gst_pad_start_task(src->srcpad, (GstTaskFunction) gst_base_src_loop,
                   src->srcpad, NULL);

static void gst_base_src_loop(GstPad *pad) {
    // This loops continuously:
    while (TRUE) {
        // 1. Call subclass create() to generate buffer
        ret = gst_base_src_create(basesrc, offset, length, &buf);

        // 2. Apply clock sync if needed
        result = gst_base_src_do_sync(basesrc, buf);

        // 3. Push buffer downstream
        ret = gst_pad_push(basesrc->srcpad, buf);
    }
}
```

**Key Insights**:
- Sources are NOT woken by timer ticks from a central clock
- They run in **continuous loops** in dedicated threads
- Clock synchronization happens AFTER buffer generation, in `gst_base_src_do_sync()`
- The loop only blocks when waiting for the clock to reach buffer presentation time

**For streamlib**: TestToneGenerator should run in a loop, not wake on timer events

### 2. Clock is Passive Reference, Not Active Scheduler

**From GStreamer synchronization docs**:
> "The clock returns the time of the clock. This value is called the `absolute_time`.
> The pipeline has a `base_time` which is the absolute_time when the pipeline went to PLAYING.
> The element timestamps (running_time) are relative to the start of the pipeline."

**Clock sync formula**:
```c
B.sync_time = B.running_time + base_time
// Wait until clock reaches sync_time
gst_clock_id_wait(clock_id, NULL);
```

**Key Insights**:
- Clock provides `now()` method (passive)
- Sources check: "Is it time to push this buffer yet?"
- Sinks check: "Is it time to render this buffer yet?"
- Clock does NOT actively wake processors
- No callbacks from clock to processors

**For streamlib**: Clock should be a trait with `now()` method, not an event dispatcher

### 3. Transform Elements Are Purely Reactive

**From scheduling documentation**:
> "In push-mode scheduling, a peer element will use `gst_pad_push()` on a srcpad,
> which will cause our `_chain()`-function to be called"

**Example data flow**:
```
audiotestsrc thread:
  create() → generate samples
  do_sync() → wait for clock time
  push() → directly calls audiomixer's chain()

audiomixer chain() [SAME THREAD]:
  mix samples
  push() → directly calls audioconvert's chain()

audioconvert chain() [SAME THREAD]:
  convert format
  push() → directly calls sink's render()
```

**Key Insights**:
- Transform elements have NO dedicated threads
- They execute in the upstream element's thread
- Chain functions are synchronous callbacks
- No separate wakeup mechanism needed
- Backpressure is automatic (blocking propagates upstream)

**For streamlib**: While we can't use synchronous blocking (need async for slow processors), transforms should only wake on `DataAvailable` events, never on timer ticks

### 4. audiomixer Synchronizes Streams

**Critical finding from GStreamer docs**:
> "audiomixer **actually synchronizes the different audio streams against each other**
> instead of just mixing samples together as they come while ignoring all timing information"

Contrast with `adder`:
> "adder mixes samples as they arrive, ignoring timestamps"

**Key Insights**:
- Proper audio mixing requires stream synchronization
- Not just "add samples when they arrive"
- Timing information is critical
- audiomixer likely uses sample-and-hold + timestamp alignment

**For streamlib**: AudioMixer needs sophisticated synchronization logic, not just reactive mixing

### 5. Sinks Provide AND Consume Clock

**From `gstaudiobasesink.c`**:

**As clock provider**:
- Sink's ringbuffer provides a clock based on hardware sample playback rate
- Pipeline queries all elements for clocks
- Sink usually "wins" because it's most accurate (hardware-driven)
- This clock is PROVIDED to other elements via `provide_clock()` method

**As synchronizer**:
```c
// In sink's render() function:
B.sync_time = B.running_time + base_time
// Wait for clock to reach this time
gst_clock_id_wait(clock_id, NULL);
render_to_hardware(B)
```

**Key Insights**:
- Hardware callback does NOT wake up sources
- Callback is used to measure/provide clock reference
- Sources wake themselves when clock time arrives
- Sink influence is passive (provides clock, doesn't drive scheduling)

**For streamlib**: AudioOutputProcessor should provide `AudioClock`, sources should sync to it

### Summary: Separate Data Flow from Clock Sync

GStreamer teaches us:
1. **Data flow** (push/pull) is about how buffers move between elements
2. **Clock sync** is about when sources generate and sinks render
3. These are **independent concerns**
4. Sources loop continuously (not tick-driven)
5. Transforms are purely reactive (data-driven)
6. Clock is passive reference, not active scheduler

**Application to streamlib**:
- Keep event-driven async architecture (we need non-blocking for ML processors)
- Sources should run in continuous loops (clock-synchronized generation)
- Transforms should only wake on DataAvailable (never on timer ticks)
- Clock infrastructure for timing decisions, not scheduling
- Remove timer groups (sources self-regulate)

---

## StreamElement Base Trait

Following GStreamer's design, all processors share a common base trait that provides uniform operations.

### GStreamer's Hierarchy

GStreamer uses inheritance-based design:
```c
GstElement (base class)
├── GstBaseSrc (inherits GstElement)
├── GstBaseSink (inherits GstElement)
└── GstBaseTransform (inherits GstElement)
```

All common functionality (name, state management, events, queries) lives in `GstElement`, while specialized behavior is in the subclasses.

### streamlib's Equivalent

```rust
trait StreamElement (base trait)
├── trait StreamSource: StreamElement
├── trait StreamSink: StreamElement
└── trait StreamTransform: StreamElement
```

### Why a Base Trait?

#### 1. Runtime Simplification

**Without base trait**:
```rust
struct StreamRuntime {
    sources: HashMap<ProcessorId, Box<dyn StreamSource>>,
    sinks: HashMap<ProcessorId, Box<dyn StreamSink>>,
    transforms: HashMap<ProcessorId, Box<dyn StreamTransform>>,
}

// Awkward: need to search all 3 collections
fn get_processor(&self, id: ProcessorId) -> ??? {
    // Which collection is it in?
}
```

**With base trait**:
```rust
struct StreamRuntime {
    elements: HashMap<ProcessorId, Box<dyn StreamElement>>,
}

// Clean: single storage, single lookup
fn get_element(&self, id: ProcessorId) -> Option<&dyn StreamElement> {
    self.elements.get(&id)
}
```

#### 2. Uniform Operations

```rust
trait StreamElement: Send + 'static {
    // Metadata
    fn name(&self) -> &str;
    fn element_type(&self) -> ElementType;
    fn descriptor(&self) -> Option<ProcessorDescriptor>;

    // Lifecycle
    fn start(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn shutdown(&mut self) -> Result<()>;

    // Introspection
    fn input_ports(&self) -> Vec<PortDescriptor>;
    fn output_ports(&self) -> Vec<PortDescriptor>;
}

// Runtime can operate uniformly on all elements:
for element in runtime.elements.values_mut() {
    element.start()?;  // Start all processors uniformly
}

runtime.stop_all()?;  // Stop all processors
```

#### 3. MCP/AI Discoverability

```rust
// MCP can query all elements uniformly
impl StreamRuntime {
    pub fn list_elements(&self) -> Vec<ElementInfo> {
        self.elements.values().map(|e| ElementInfo {
            name: e.name(),
            type: e.element_type(),
            inputs: e.input_ports(),
            outputs: e.output_ports(),
        }).collect()
    }
}
```

#### 4. Type-Safe Dispatch

```rust
impl StreamRuntime {
    pub fn add_element<E: StreamElement>(&mut self, element: E) -> ProcessorHandle<E> {
        let elem_type = element.element_type();

        // Dispatch based on type
        let handle = match elem_type {
            ElementType::Source => {
                let src = element.as_source().unwrap();
                self.spawn_source_loop(src)
            }
            ElementType::Sink => {
                let sink = element.as_sink().unwrap();
                self.spawn_sink_handler(sink)
            }
            ElementType::Transform => {
                let trans = element.as_transform().unwrap();
                self.spawn_transform_handler(trans)
            }
        };

        self.elements.insert(handle.id(), Box::new(element));
        handle
    }
}
```

### StreamElement Trait Definition

```rust
/// Base trait for all stream processing elements
///
/// Inspired by GStreamer's GstElement, this provides common functionality
/// for metadata, lifecycle management, and introspection.
pub trait StreamElement: Send + 'static {
    /// Element name (for logging, debugging, MCP)
    ///
    /// Should be unique within a runtime instance.
    fn name(&self) -> &str;

    /// Element type (Source/Sink/Transform)
    ///
    /// Used by runtime to dispatch to appropriate execution model.
    fn element_type(&self) -> ElementType;

    /// Processor descriptor for MCP/AI discoverability
    fn descriptor(&self) -> Option<ProcessorDescriptor>;

    /// Lifecycle: Start processing
    ///
    /// Called by runtime when pipeline starts.
    /// Default implementation does nothing.
    fn start(&mut self) -> Result<()> {
        Ok(())
    }

    /// Lifecycle: Stop processing
    ///
    /// Called by runtime when pipeline stops.
    /// Should gracefully pause processing but maintain state.
    fn stop(&mut self) -> Result<()> {
        Ok(())
    }

    /// Lifecycle: Shutdown (cleanup resources)
    ///
    /// Called when element is being removed from runtime.
    /// Should release all resources (hardware, memory, threads).
    fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }

    /// Get input port descriptors
    ///
    /// Returns metadata about all input ports.
    /// Default: empty (for sources with no inputs).
    fn input_ports(&self) -> Vec<PortDescriptor> {
        Vec::new()
    }

    /// Get output port descriptors
    ///
    /// Returns metadata about all output ports.
    /// Default: empty (for sinks with no outputs).
    fn output_ports(&self) -> Vec<PortDescriptor> {
        Vec::new()
    }

    /// Downcast to StreamSource (if applicable)
    fn as_source(&self) -> Option<&dyn StreamSource> {
        None
    }

    /// Downcast to StreamSink (if applicable)
    fn as_sink(&self) -> Option<&dyn StreamSink> {
        None
    }

    /// Downcast to StreamTransform (if applicable)
    fn as_transform(&self) -> Option<&dyn StreamTransform> {
        None
    }
}

pub enum ElementType {
    Source,
    Sink,
    Transform,
}
```

### Derive Macro Support

The derive macros automatically implement `StreamElement`:

```rust
#[derive(StreamSource)]
#[scheduling(mode = "loop", clock = "audio")]
struct TestToneGenerator {
    #[output()]
    audio: StreamOutput<AudioFrame>,

    frequency: f64,
}

// Macro generates:
impl StreamElement for TestToneGenerator {
    fn name(&self) -> &str {
        "TestToneGenerator"
    }

    fn element_type(&self) -> ElementType {
        ElementType::Source
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        // Auto-generated from struct definition
    }

    fn output_ports(&self) -> Vec<PortDescriptor> {
        // Extracted from #[output()] attributes
        vec![
            PortDescriptor {
                name: "audio",
                data_type: "AudioFrame",
                direction: PortDirection::Output,
            }
        ]
    }

    fn as_source(&self) -> Option<&dyn StreamSource> {
        Some(self)
    }
}

impl StreamSource for TestToneGenerator {
    // Specialized source methods
    // ...
}
```

---

## 3-Tier Processor Architecture

Based on GStreamer's architecture and streamlib's requirements, we define 3 processor types that all inherit from `StreamElement`:

### Design Principles

1. **Topology-based types**: Processor type determined by input/output topology
2. **Clear separation**: Each type has distinct scheduling semantics
3. **Runtime optimization**: Runtime can optimize based on processor type
4. **Type safety**: Compile-time verification of connections

### The Three Tiers

All three processor types inherit from `StreamElement`:

```
                    ┌──────────────────┐
                    │  StreamElement   │  Base trait (common functionality)
                    │   (base trait)   │
                    └────────┬─────────┘
                             │ inherits
                    ┌────────┴─────────┬────────────────┬─────────────┐
                    │                  │                │             │
         ┌──────────▼──────────┐  ┌───▼──────────┐  ┌──▼──────────┐
         │   StreamSource      │  │ StreamSink   │  │StreamTransform│
         │  (no inputs)        │  │(no outputs)  │  │(both I/O)   │
         └──────────┬──────────┘  └───┬──────────┘  └──┬──────────┘
                    │                 │                │
                    │                 │                │
         ┌──────────▼──────────┐  ┌───▼──────────┐  ┌──▼──────────┐
         │ TestToneGenerator   │  │AudioOutput   │  │AudioMixer   │
         │ CameraProcessor     │  │DisplayProc   │  │ClapEffect   │
         │ AudioCapture        │  │FileSink      │  │Tee          │
         └─────────────────────┘  └──────────────┘  └─────────────┘
                    │ generates         │ renders        │ processes
                    ▼                   ▼                ▼
                AudioFrame          (to hardware)    AudioFrame
                VideoFrame          (to file)        VideoFrame
```

### 1. StreamSource

**Definition**: Processors that generate data (no inputs)

**Characteristics**:
- No input ports (sources of data)
- One or more output ports
- Typically loop-based execution (continuous generation)
- Clock-synchronized (generates at specific rate)
- Can be hardware-driven (camera callbacks) or algorithmic (test tone)

**Examples**:
- `TestToneGenerator` - Algorithmic sine wave generation
- `CameraProcessor` - Hardware video capture
- `AudioCaptureProcessor` - Microphone input
- `RTSPSource` (future) - Network video stream
- `AppSource` (future) - Application-provided data

**Scheduling modes**:
- `loop` - Continuous loop with clock synchronization (most common)
- `callback` - Hardware-driven callbacks (camera, audio capture)
- `reactive` - Wake on external events (network packets, app pushes)

**Key trait methods**:
```rust
trait StreamSource: StreamElement {
    type Output: FrameData;

    fn generate(&mut self) -> Result<Self::Output>;
    // Called by runtime in loop or callback

    fn clock_sync_point(&self) -> Duration;
    // How long to wait before next generation

    // Inherits from StreamElement:
    // - fn name(&self) -> &str;
    // - fn element_type(&self) -> ElementType { ElementType::Source }
    // - fn output_ports(&self) -> Vec<PortDescriptor>; (auto-generated)
}
```

### 2. StreamSink

**Definition**: Processors that consume data (no outputs)

**Characteristics**:
- One or more input ports
- No output ports (terminal processors)
- Clock-synchronized rendering (wait until presentation time)
- Can provide clock reference for pipeline
- May use hardware callbacks for rendering

**Examples**:
- `AudioOutputProcessor` - Speaker output
- `DisplayProcessor` - Window rendering
- `RTMPSink` (future) - Network streaming
- `AppSink` (future) - Application consumes data
- `FileSink` (future) - Write to disk

**Scheduling modes**:
- `callback` - Hardware-driven (CoreAudio, CVDisplayLink)
- `pull` - Application-driven (app calls get_frame())
- `reactive` - Process when data arrives (with clock sync)

**Key trait methods**:
```rust
trait StreamSink: StreamElement {
    type Input: FrameData;

    fn render(&mut self, input: Self::Input) -> Result<()>;
    // Called when data arrives, syncs to clock before rendering

    fn provide_clock(&self) -> Option<Arc<dyn Clock>> {
        None  // Default: no clock provided
    }

    fn accept_data(&mut self, input: Self::Input) {
        // Default: immediate render
        self.render(input).ok();
    }

    // Inherits from StreamElement:
    // - fn name(&self) -> &str;
    // - fn element_type(&self) -> ElementType { ElementType::Sink }
    // - fn input_ports(&self) -> Vec<PortDescriptor>; (auto-generated)
}
```

### 3. StreamTransform

**Definition**: Processors that transform data (inputs → outputs)

**Characteristics**:
- One or more input ports
- One or more output ports
- Purely reactive (wakes on DataAvailable only)
- No clock awareness (just processes data)
- Can handle any I/O configuration (1→1, N→1, 1→N, N→M)

**Examples**:
- `ClapEffectProcessor` - Audio effect (1→1)
- `AudioMixer` - Multi-source mixing (N→1)
- `VideoMixer` - Overlay multiple video streams (N→1)
- `Tee` - Split stream to multiple outputs (1→N)
- `ColorGradeProcessor` - Video color correction (1→1)
- `ObjectDetectionProcessor` - ML inference (1→1, slow)

**Scheduling modes**:
- `reactive` - Only mode (always wake on DataAvailable)
- Implementation can be fast (real-time) or slow (drops frames via read_latest)

**Key trait methods**:
```rust
trait StreamTransform: StreamElement {
    fn transform(&mut self, event: WakeupEvent) -> Result<()>;
    // Called when DataAvailable event arrives
    // Reads from inputs, writes to outputs

    // Can use read_latest() for slow processors (auto frame drop)
    // Can use blocking read for fast processors (backpressure)

    // Inherits from StreamElement:
    // - fn name(&self) -> &str;
    // - fn element_type(&self) -> ElementType { ElementType::Transform }
    // - fn input_ports(&self) -> Vec<PortDescriptor>; (auto-generated)
    // - fn output_ports(&self) -> Vec<PortDescriptor>; (auto-generated)
}
```

### Type Distinction: What Makes Each Type Different?

| Property | StreamSource | StreamSink | StreamTransform |
|----------|-------------|------------|-----------------|
| **Has inputs?** | ❌ No | ✅ Yes | ✅ Yes |
| **Has outputs?** | ✅ Yes | ❌ No | ✅ Yes |
| **Wakes on DataAvailable?** | ❌ No (no inputs!) | ✅ Yes | ✅ Yes |
| **Wakes on clock/loop?** | ✅ Yes (generates) | ❌ No (only reactive) | ❌ No (only reactive) |
| **Clock-aware?** | ✅ Yes (sync generation) | ✅ Yes (sync rendering) | ❌ No (processes ASAP) |
| **Can provide clock?** | ⚠️ Rarely | ✅ Often (hardware) | ❌ Never |
| **Execution model** | Loop or callback | Reactive + clock sync | Purely reactive |

### Edge Cases & Special Processors

**Q: What about processors with both timer AND data behavior?**
A: Choose primary behavior:
- If primarily generates data → StreamSource
- If primarily transforms data → StreamTransform
- Use internal state to handle secondary behavior

**Q: What about Queue processors?**
A: StreamTransform (1→1, adds buffering)

**Q: What about App Source/Sink (external control)?**
A: Still Source/Sink, just reactive scheduling mode

**Q: What about complex routers (N inputs, M outputs, complex logic)?**
A: StreamTransform can handle arbitrary I/O topology

### Migration Strategy

Current processors map to new types:

| Current Processor | New Type | Scheduling Mode |
|------------------|----------|-----------------|
| TestToneGenerator | StreamSource | loop, audio clock |
| CameraProcessor | StreamSource | callback (AVFoundation) |
| AudioCaptureProcessor | StreamSource | callback (CoreAudio) |
| ClapEffectProcessor | StreamTransform | reactive |
| AudioMixer | StreamTransform | reactive (multi-input) |
| PerformanceOverlay | StreamTransform | reactive |
| DisplayProcessor | StreamSink | callback (CVDisplayLink) |
| AudioOutputProcessor | StreamSink | callback (CoreAudio), provides clock |

---

## Declarative Scheduling API

The core innovation of this redesign is **declarative scheduling** - developers describe what they need, the runtime provides the infrastructure.

### Design Philosophy

**Before (Manual)**:
```rust
impl TestToneGenerator {
    pub fn new(freq: f64, sample_rate: u32, amplitude: f64) -> Self {
        // Developer must:
        // 1. Calculate timer rate manually
        let buffer_size = 2048;
        let timer_rate = sample_rate as f64 / buffer_size as f64;
        
        // 2. Remember to add to timer group
        let timer_group_id = Some("audio_master".to_string());
        
        // 3. Implement descriptor_instance with TimerRequirements
        // 4. Track phase, frame numbers, timestamps manually
        // 5. Write to output ports correctly
        
        // ... 90+ lines of setup code
    }
    
    fn descriptor_instance(&self) -> Option<ProcessorDescriptor> {
        // More manual boilerplate
    }
}
```

**After (Declarative)**:
```rust
#[derive(StreamSource)]
#[scheduling(mode = "loop", clock = "audio", rate_hz = 23.44)]
struct TestToneGenerator {
    #[output()]
    audio: StreamOutput<AudioFrame>,
    
    // Config fields (auto-extracted)
    frequency: f64,
    amplitude: f64,
    sample_rate: u32,
}

impl TestToneGenerator {
    // Just implement the core logic
    fn generate(&mut self) -> Result<AudioFrame> {
        let samples = self.generate_sine_wave();
        Ok(AudioFrame::new(samples, self.sample_rate, 2))
    }
}
```

### Scheduling Attributes Reference

#### `#[scheduling(...)]` - Source/Sink Scheduling Configuration

Applied to struct that derives `StreamSource` or `StreamSink`.

**Attributes**:

##### `mode`
**Values**: `"loop"`, `"callback"`, `"reactive"`, `"pull"`
**Applies to**: Source, Sink
**Required**: Yes

Defines how the processor wakes up:

- `"loop"` (Source): Runtime creates dedicated thread with continuous loop
  ```rust
  #[scheduling(mode = "loop", clock = "audio")]
  // Runtime spawns: loop { generate() → sync_clock → wait → push }
  ```

- `"callback"` (Source/Sink): Runtime wires hardware callbacks
  ```rust
  #[scheduling(mode = "callback", hardware = "coreaudio")]
  // Runtime connects to CoreAudio callback
  ```

- `"reactive"` (Source): Wake on external events
  ```rust
  #[scheduling(mode = "reactive")]
  // Runtime wakes on network packets, app pushes, etc.
  ```

- `"pull"` (Sink): Application pulls data manually
  ```rust
  #[scheduling(mode = "pull")]
  // App calls sink.get_next_frame()
  ```

##### `clock`
**Values**: `"audio"`, `"vsync"`, `"software"`, `"custom"`
**Applies to**: Source, Sink
**Optional**: Defaults to `"software"`

Specifies which clock to synchronize against:

- `"audio"`: Use pipeline's audio clock (from AudioOutputProcessor)
  ```rust
  #[scheduling(mode = "loop", clock = "audio")]
  // Syncs to hardware audio sample rate
  ```

- `"vsync"`: Use display vsync clock (from DisplayProcessor)
  ```rust
  #[scheduling(mode = "loop", clock = "vsync")]
  // Syncs to 60Hz display refresh
  ```

- `"software"`: Use software timer (std::time)
  ```rust
  #[scheduling(mode = "loop", clock = "software", rate_hz = 30.0)]
  // Software timer at 30Hz
  ```

##### `rate_hz`
**Values**: Positive f64
**Applies to**: Source with `mode = "loop"` and `clock = "software"`
**Required**: Only if `clock = "software"`

Specifies software timer rate:
```rust
#[scheduling(mode = "loop", clock = "software", rate_hz = 23.44)]
// Loop at 23.44 Hz (48000 / 2048)
```

##### `provide_clock`
**Values**: `true`, `false`
**Applies to**: Source, Sink
**Optional**: Defaults to `false`

Indicates this processor provides a clock for the pipeline:
```rust
#[scheduling(mode = "callback", provide_clock = true)]
// This sink (e.g., AudioOutputProcessor) provides pipeline clock
```

Only one processor should provide the pipeline clock (typically the audio sink).

##### `hardware`
**Values**: `"coreaudio"`, `"avfoundation"`, `"cvdisplaylink"`, `"v4l2"`, etc.
**Applies to**: Source/Sink with `mode = "callback"`
**Optional**: Runtime auto-detects if not specified

Specifies hardware callback type:
```rust
#[scheduling(mode = "callback", hardware = "coreaudio")]
// Use CoreAudio callback for audio I/O
```

#### `#[input(...)]` and `#[output(...)]` - Port Configuration

Applied to struct fields of type `StreamInput<T>` or `StreamOutput<T>`.

**Attributes**:

##### `description`
Human-readable port description for MCP:
```rust
#[input(description = "Input audio stream for mixing")]
audio_in: StreamInput<AudioFrame>,
```

##### `optional`
Mark port as optional (can be unconnected):
```rust
#[input(optional = true)]
overlay: StreamInput<VideoFrame>,
```

### Complete Examples

#### Example 1: Test Tone Generator (StreamSource, Loop Mode)

```rust
use streamlib::{StreamSource, StreamOutput, AudioFrame, Result};

#[derive(StreamSource)]
#[scheduling(mode = "loop", clock = "audio")]
#[processor(
    description = "Generates sine wave test tones",
    usage = "Configure frequency and amplitude, connect audio output to mixer or speaker"
)]
struct TestToneGenerator {
    #[output(description = "Generated audio samples")]
    audio: StreamOutput<AudioFrame>,
    
    // Config fields (auto-extracted to TestToneGeneratorConfig)
    frequency: f64,
    amplitude: f64,
    sample_rate: u32,
    
    // Internal state
    #[skip_config]
    phase: f64,
}

impl TestToneGenerator {
    // Called by runtime in continuous loop
    fn generate(&mut self) -> Result<AudioFrame> {
        let mut samples = Vec::with_capacity(2048 * 2);
        
        for _ in 0..2048 {
            let sample = (self.phase.sin() * self.amplitude) as f32;
            samples.push(sample);
            samples.push(sample);
            
            self.phase += 2.0 * std::f64::consts::PI * self.frequency / self.sample_rate as f64;
            if self.phase >= 2.0 * std::f64::consts::PI {
                self.phase -= 2.0 * std::f64::consts::PI;
            }
        }
        
        Ok(AudioFrame::new(samples, self.sample_rate, 2))
    }
    
    // Called by runtime to determine wait time between generations
    fn clock_sync_point(&self) -> Duration {
        Duration::from_secs_f64(2048.0 / self.sample_rate as f64)
    }
}

// Usage:
// let tone = runtime.add_processor_with_config::<TestToneGenerator>(
//     TestToneGeneratorConfig {
//         frequency: 440.0,
//         amplitude: 0.5,
//         sample_rate: 48000,
//     }
// )?;
```

**What the runtime does**:
1. Spawns dedicated thread for this source
2. Queries audio sink for `AudioClock`
3. Continuous loop:
   - Calls `generate()` → gets AudioFrame
   - Calls `clock_sync_point()` → gets wait duration
   - Checks audio clock: "Is it time to push?"
   - Sleeps until clock time arrives
   - Writes to `audio` output port
   - Sends `DataAvailable` to downstream processors

#### Example 2: CLAP Effect (StreamTransform, Reactive)

```rust
use streamlib::{StreamTransform, StreamInput, StreamOutput, AudioFrame, Result};

#[derive(StreamTransform)]
#[processor(description = "CLAP audio effect processor")]
struct ClapEffectProcessor {
    #[input(description = "Audio input to process")]
    audio_in: StreamInput<AudioFrame>,
    
    #[output(description = "Processed audio output")]
    audio_out: StreamOutput<AudioFrame>,
    
    // Config fields
    plugin_path: PathBuf,
    plugin_name: Option<String>,
    
    // Internal state
    #[skip_config]
    clap_plugin: Option<ClapPlugin>,
}

impl ClapEffectProcessor {
    // Called when DataAvailable event arrives
    fn transform(&mut self, event: WakeupEvent) -> Result<()> {
        match event {
            WakeupEvent::DataAvailable => {
                // Read input (latest frame)
                if let Some(frame) = self.audio_in.read_latest() {
                    // Process through CLAP plugin
                    let output = self.clap_plugin.as_mut()
                        .ok_or_else(|| Error::msg("CLAP plugin not loaded"))?
                        .process(&frame.samples)?;
                    
                    // Write output
                    self.audio_out.write(AudioFrame::new(
                        output,
                        frame.sample_rate,
                        frame.channels,
                    ));
                }
            }
            WakeupEvent::Shutdown => {
                // Cleanup
                self.clap_plugin = None;
            }
            _ => {} // Ignore other events
        }
        Ok(())
    }
}
```

**What the runtime does**:
1. Spawns handler thread for this transform
2. Waits on wakeup channel
3. When upstream writes to `audio_in`:
   - Sends `DataAvailable` event
   - Transform wakes up
   - Calls `transform()` with event
4. Transform processes and writes to `audio_out`
5. Runtime sends `DataAvailable` to downstream

**No clock involvement** - purely reactive to data!

#### Example 3: Audio Mixer (StreamTransform, Multi-Input)

```rust
use streamlib::{StreamTransform, StreamInput, StreamOutput, AudioFrame, Result};

#[derive(StreamTransform)]
#[processor(description = "Mix multiple audio streams")]
struct AudioMixer {
    // Dynamic number of inputs based on config
    #[inputs(count = "num_inputs")]
    inputs: Vec<StreamInput<AudioFrame>>,
    
    #[output()]
    mixed: StreamOutput<AudioFrame>,
    
    // Config
    num_inputs: usize,
    strategy: MixingStrategy,
    
    // Internal state
    #[skip_config]
    sample_hold: HashMap<String, AudioFrame>,
}

impl AudioMixer {
    fn transform(&mut self, event: WakeupEvent) -> Result<()> {
        match event {
            WakeupEvent::DataAvailable => {
                // Read from ALL inputs (sample-and-hold)
                for (i, input) in self.inputs.iter().enumerate() {
                    if let Some(frame) = input.read_latest() {
                        self.sample_hold.insert(format!("input_{}", i), frame);
                    }
                }
                
                // Only mix if we have data from all inputs
                if self.sample_hold.len() == self.num_inputs {
                    let mixed_frame = self.mix_frames(&self.sample_hold)?;
                    self.mixed.write(mixed_frame);
                }
            }
            _ => {}
        }
        Ok(())
    }
    
    fn mix_frames(&self, frames: &HashMap<String, AudioFrame>) -> Result<AudioFrame> {
        // Mixing implementation
        // ...
    }
}
```

**Key point**: No `#[scheduling(...)]` attribute because StreamTransform is always reactive!

#### Example 4: Audio Output (StreamSink, Callback Mode)

```rust
use streamlib::{StreamSink, StreamInput, AudioFrame, Result, Clock};

#[derive(StreamSink)]
#[scheduling(mode = "callback", hardware = "coreaudio", provide_clock = true)]
#[processor(description = "Audio output to speakers")]
struct AudioOutputProcessor {
    #[input(description = "Audio to play")]
    audio: StreamInput<AudioFrame>,
    
    // Config
    device_id: Option<usize>,
    
    // Internal state
    #[skip_config]
    sample_buffer: Arc<Mutex<Vec<f32>>>,
    
    #[skip_config]
    audio_clock: Arc<AudioClock>,
}

impl AudioOutputProcessor {
    // Called when DataAvailable arrives
    fn accept_data(&mut self, input: AudioFrame) {
        // Queue data for hardware callback
        let mut buffer = self.sample_buffer.lock();
        buffer.extend_from_slice(&input.samples);
    }
    
    // Provide clock for pipeline
    fn provide_clock(&self) -> Option<Arc<dyn Clock>> {
        Some(self.audio_clock.clone())
    }
}

// Runtime also registers hardware callback:
// CoreAudio callback → drains sample_buffer → drives AudioClock
```

**What the runtime does**:
1. Queries processor for hardware type ("coreaudio")
2. Sets up CoreAudio stream with callback
3. Callback drains `sample_buffer`
4. Callback updates `audio_clock` (provides time reference)
5. When upstream writes, calls `accept_data()` to queue samples

### Macro Implementation Notes

The derive macros analyze struct definition:

1. **Detect processor type**:
   - `StreamSource` → no `StreamInput` fields
   - `StreamSink` → no `StreamOutput` fields
   - `StreamTransform` → has both

2. **Extract scheduling attributes**:
   - Parse `#[scheduling(...)]`
   - Validate combinations (e.g., `rate_hz` requires `clock = "software"`)
   - Generate runtime configuration

3. **Generate config struct**:
   - All non-`#[skip_config]` fields → config fields
   - Create `ProcessorNameConfig` struct
   - Implement `from_config()` constructor

4. **Generate descriptor**:
   - Extract input/output port schemas
   - Include scheduling requirements
   - Add to MCP schema

---

## Clock Infrastructure

Clocks provide **passive timing references** for sources and sinks to synchronize against.

### Clock Trait

```rust
/// Passive clock reference for processor synchronization
pub trait Clock: Send + Sync {
    /// Current time in nanoseconds (monotonic)
    fn now_ns(&self) -> i64;
    
    /// Current time as Duration (convenience)
    fn now(&self) -> Duration {
        Duration::from_nanos(self.now_ns() as u64)
    }
    
    /// Clock rate in Hz (for variable-rate clocks)
    fn rate_hz(&self) -> Option<f64> {
        None
    }
    
    /// Human-readable clock description
    fn description(&self) -> &str;
}
```

### AudioClock (Hardware-Driven)

**Provided by**: `AudioOutputProcessor` (CoreAudio sink)
**Rate**: ~48000 Hz sample rate
**Accuracy**: Sample-accurate (hardware clock)

```rust
pub struct AudioClock {
    /// CoreAudio device reference
    device: AudioDeviceID,
    
    /// Timestamp of when playback started
    base_time: i64,
    
    /// Total samples played since start
    samples_played: AtomicU64,
    
    /// Sample rate (e.g., 48000)
    sample_rate: u32,
}

impl Clock for AudioClock {
    fn now_ns(&self) -> i64 {
        let samples = self.samples_played.load(Ordering::Relaxed);
        let elapsed_ns = (samples as f64 / self.sample_rate as f64 * 1e9) as i64;
        self.base_time + elapsed_ns
    }
    
    fn rate_hz(&self) -> Option<f64> {
        Some(self.sample_rate as f64)
    }
    
    fn description(&self) -> &str {
        "CoreAudio Hardware Clock"
    }
}

// Updated by CoreAudio callback:
impl AudioOutputProcessor {
    fn audio_callback(&self, buffer: &mut [f32]) {
        // ... fill buffer ...
        
        // Update clock
        let samples_written = buffer.len() / 2; // stereo
        self.audio_clock.samples_played.fetch_add(
            samples_written as u64,
            Ordering::Relaxed
        );
    }
}
```

**Why sample-accurate?**
- CoreAudio callback fires exactly when hardware needs samples
- Each callback processes fixed number of samples (e.g., 2048)
- Total samples played = exact hardware time
- No drift (unlike software timers)

### VideoClock (CVDisplayLink)

**Provided by**: `DisplayProcessor` (macOS display sink)
**Rate**: 60Hz (or display's actual refresh rate)
**Accuracy**: Frame-accurate (vsync)

```rust
pub struct VideoClock {
    /// CVDisplayLink reference
    display_link: CVDisplayLink,
    
    /// Timestamp of when display started
    base_time: i64,
    
    /// Total frames rendered
    frames_rendered: AtomicU64,
    
    /// Display refresh rate (e.g., 60.0)
    refresh_rate: f64,
}

impl Clock for VideoClock {
    fn now_ns(&self) -> i64 {
        let frames = self.frames_rendered.load(Ordering::Relaxed);
        let elapsed_ns = (frames as f64 / self.refresh_rate * 1e9) as i64;
        self.base_time + elapsed_ns
    }
    
    fn rate_hz(&self) -> Option<f64> {
        Some(self.refresh_rate)
    }
    
    fn description(&self) -> &str {
        "CVDisplayLink Hardware Clock"
    }
}
```

### SoftwareClock (Fallback)

**Used when**: No hardware clock available
**Rate**: Configurable
**Accuracy**: ~1ms (OS-dependent)

```rust
pub struct SoftwareClock {
    /// When clock started
    start_time: Instant,
    
    /// Configured rate (if applicable)
    rate_hz: Option<f64>,
}

impl Clock for SoftwareClock {
    fn now_ns(&self) -> i64 {
        self.start_time.elapsed().as_nanos() as i64
    }
    
    fn rate_hz(&self) -> Option<f64> {
        self.rate_hz
    }
    
    fn description(&self) -> &str {
        "Software Timer Clock"
    }
}
```

### Clock Selection & Pipeline Configuration

**Pipeline clock selection** (like GStreamer):
1. Runtime queries all processors for `provide_clock()`
2. Priority:
   - AudioClock (highest - most accurate, drives AV sync)
   - VideoClock (video-only pipelines)
   - SoftwareClock (fallback)
3. Selected clock stored in `StreamRuntime.pipeline_clock`
4. All processors sync to this clock

**Usage in processors**:
```rust
// In TestToneGenerator (StreamSource with loop mode):
fn run_loop(&mut self, clock: Arc<dyn Clock>) {
    loop {
        // Generate frame
        let frame = self.generate()?;
        
        // Calculate when to push (GStreamer-style sync)
        let sync_time = self.next_sample_time;
        let now = clock.now_ns();
        
        if now < sync_time {
            // Wait until clock reaches sync time
            let wait_ns = sync_time - now;
            std::thread::sleep(Duration::from_nanos(wait_ns as u64));
        }
        
        // Push downstream
        self.audio.write(frame);
        
        // Advance sync time
        self.next_sample_time += self.buffer_duration_ns;
    }
}
```

---

## Runtime Scheduling Infrastructure

The runtime provides all scheduling infrastructure based on processor declarations.

### Source Loop Execution

For `StreamSource` with `#[scheduling(mode = "loop")]`:

```rust
// In StreamRuntime::spawn_source_loop()
fn spawn_source_loop<S: StreamSource>(
    mut source: S,
    clock: Arc<dyn Clock>,
    wakeup_tx: Sender<WakeupEvent>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        // Initial sync time
        let mut next_sync_time_ns = clock.now_ns();
        
        loop {
            // 1. Generate data
            match source.generate() {
                Ok(frame) => {
                    // 2. Write to output port
                    source.write_output(frame); // Generated by macro
                    
                    // 3. Calculate next sync time
                    let sync_point = source.clock_sync_point();
                    next_sync_time_ns += sync_point.as_nanos() as i64;
                    
                    // 4. Wait for clock (GStreamer's gst_base_src_wait)
                    let now = clock.now_ns();
                    if now < next_sync_time_ns {
                        let wait_ns = next_sync_time_ns - now;
                        std::thread::sleep(Duration::from_nanos(wait_ns as u64));
                    } else {
                        // Clock drift - we're behind schedule
                        tracing::warn!(
                            "Source {} is {}ms behind clock",
                            source.name(),
                            (now - next_sync_time_ns) / 1_000_000
                        );
                        // Skip ahead to current time
                        next_sync_time_ns = now;
                    }
                }
                Err(e) => {
                    tracing::error!("Source generate() error: {}", e);
                    // Emit error event, maybe reconnect hardware, etc.
                }
            }
            
            // Check for shutdown
            if wakeup_tx.send(WakeupEvent::Shutdown).is_err() {
                break; // Runtime shutting down
            }
        }
    })
}
```

**Key differences from current timer groups**:
- ✅ ONE thread per source (not shared timer thread)
- ✅ Source controls its own timing (self-regulating)
- ✅ Clock-synchronized wait (no timer tick events)
- ✅ Handles clock drift gracefully
- ✅ No downstream wakeup spam (only when writes to port)

### Transform Reactive Execution

For `StreamTransform` (always reactive):

```rust
// In StreamRuntime::spawn_transform_handler()
fn spawn_transform_handler<T: StreamTransform>(
    mut transform: T,
    wakeup_rx: Receiver<WakeupEvent>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        loop {
            // Wait for wakeup event
            match wakeup_rx.recv() {
                Ok(WakeupEvent::DataAvailable) => {
                    // Process data
                    if let Err(e) = transform.transform(WakeupEvent::DataAvailable) {
                        tracing::error!("Transform error: {}", e);
                    }
                }
                Ok(WakeupEvent::Shutdown) => {
                    transform.transform(WakeupEvent::Shutdown).ok();
                    break;
                }
                Ok(WakeupEvent::TimerTick { .. }) => {
                    // IGNORE! Transforms don't care about timer ticks
                    tracing::warn!("Transform received unexpected TimerTick");
                }
                Err(_) => break, // Channel closed
            }
        }
    })
}
```

**Critical insight**: Transforms ONLY wake on DataAvailable, never on TimerTick!

### Sink Reactive Execution with Clock Sync

For `StreamSink` with `#[scheduling(mode = "callback")]`:

```rust
// In StreamRuntime::spawn_sink_handler()
fn spawn_sink_handler<S: StreamSink>(
    mut sink: S,
    wakeup_rx: Receiver<WakeupEvent>,
    clock: Arc<dyn Clock>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        loop {
            match wakeup_rx.recv() {
                Ok(WakeupEvent::DataAvailable) => {
                    // Read input
                    if let Some(frame) = sink.read_input() {
                        // Check clock: is it time to render?
                        // (GStreamer's gst_base_sink_do_sync)
                        let present_time = frame.timestamp_ns;
                        let now = clock.now_ns();
                        
                        if now < present_time {
                            // Wait until presentation time
                            let wait_ns = present_time - now;
                            std::thread::sleep(Duration::from_nanos(wait_ns as u64));
                        }
                        
                        // Render
                        if let Err(e) = sink.render(frame) {
                            tracing::error!("Sink render error: {}", e);
                        }
                    }
                }
                Ok(WakeupEvent::Shutdown) => break,
                _ => {}
            }
        }
    })
}
```

**For callback-driven sinks** (e.g., CoreAudio):

```rust
// Alternative: Sink doesn't have handler thread
// Instead, hardware callback directly calls sink.accept_data()
impl AudioOutputProcessor {
    pub fn start(&mut self) -> Result<()> {
        let sample_buffer = self.sample_buffer.clone();
        
        self.stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _info| {
                // Hardware callback (audio thread)
                let mut buffer = sample_buffer.lock();
                
                if buffer.len() >= data.len() {
                    data.copy_from_slice(&buffer[..data.len()]);
                    buffer.drain(..data.len());
                } else {
                    // Underrun - fill with silence
                    data.fill(0.0);
                }
            },
            |err| { tracing::error!("Audio error: {}", err); },
            None,
        )?;
        
        self.stream.play()?;
        Ok(())
    }
    
    // Called from handler thread when DataAvailable arrives
    pub fn accept_data(&mut self, frame: AudioFrame) {
        let mut buffer = self.sample_buffer.lock();
        buffer.extend_from_slice(&frame.samples);
    }
}
```

### Runtime Initialization

```rust
impl StreamRuntime {
    pub fn add_processor<P>(&mut self, processor: P) -> ProcessorHandle<P>
    where
        P: ProcessorType, // Union of Source/Sink/Transform
    {
        let (wakeup_tx, wakeup_rx) = crossbeam_channel::unbounded();
        
        // Spawn appropriate execution model
        let handle = match processor.processor_type() {
            ProcessorTypeEnum::Source(source) => {
                let scheduling = source.scheduling_config();
                
                match scheduling.mode {
                    SchedulingMode::Loop => {
                        // Get or create pipeline clock
                        let clock = self.get_or_create_clock(scheduling.clock);
                        
                        // Spawn source loop
                        self.spawn_source_loop(source, clock, wakeup_tx)
                    }
                    SchedulingMode::Callback => {
                        // Set up hardware callbacks
                        self.spawn_callback_source(source)
                    }
                    SchedulingMode::Reactive => {
                        // Reactive source (like network stream)
                        self.spawn_reactive_source(source, wakeup_rx)
                    }
                }
            }
            
            ProcessorTypeEnum::Sink(sink) => {
                let scheduling = sink.scheduling_config();
                
                // Check if sink provides clock
                if scheduling.provide_clock {
                    if let Some(clock) = sink.provide_clock() {
                        self.pipeline_clock = Some(clock);
                    }
                }
                
                match scheduling.mode {
                    SchedulingMode::Callback => {
                        // Set up hardware callback
                        sink.start_hardware_callback()?;
                        // Handler thread just feeds callback
                        self.spawn_sink_feeder(sink, wakeup_rx)
                    }
                    SchedulingMode::Reactive => {
                        let clock = self.get_pipeline_clock();
                        self.spawn_sink_handler(sink, wakeup_rx, clock)
                    }
                    _ => unreachable!("Sinks don't use loop mode")
                }
            }
            
            ProcessorTypeEnum::Transform(transform) => {
                // Always reactive
                self.spawn_transform_handler(transform, wakeup_rx)
            }
        };
        
        // Store handle
        let proc_id = self.next_processor_id();
        self.processors.insert(proc_id, handle);
        
        ProcessorHandle::new(proc_id, wakeup_tx)
    }
}
```

---

## Event Flow & Data Movement

### Complete Pipeline Example

Pipeline: `TestToneGenerator → AudioMixer → ClapEffect → AudioOutputProcessor`

#### Initialization

```rust
let mut runtime = StreamRuntime::new();

// 1. Add audio sink (provides clock)
let speaker = runtime.add_processor_with_config::<AudioOutputProcessor>(
    AudioOutputConfig { device_id: None }
)?;
// Runtime: Sets pipeline_clock = speaker.audio_clock

// 2. Add test tone generators
let tone1 = runtime.add_processor_with_config::<TestToneGenerator>(
    TestToneConfig { frequency: 440.0, amplitude: 0.15, sample_rate: 48000 }
)?;
// Runtime: Spawns source loop thread, syncs to pipeline_clock (audio)

let tone2 = runtime.add_processor_with_config::<TestToneGenerator>(
    TestToneConfig { frequency: 554.37, amplitude: 0.15, sample_rate: 48000 }
)?;

let tone3 = runtime.add_processor_with_config::<TestToneGenerator>(
    TestToneConfig { frequency: 659.25, amplitude: 0.15, sample_rate: 48000 }
)?;

// 3. Add mixer (reactive transform)
let mixer = runtime.add_processor_with_config::<AudioMixer>(
    AudioMixerConfig { num_inputs: 3, strategy: MixingStrategy::SumNormalized }
)?;
// Runtime: Spawns handler thread, waits on DataAvailable

// 4. Add effect (reactive transform)
let effect = runtime.add_processor_with_config::<ClapEffectProcessor>(
    ClapEffectConfig { plugin_path: "/path/to/reverb.clap".into(), ... }
)?;
// Runtime: Spawns handler thread, waits on DataAvailable

// 5. Connect pipeline
runtime.connect(tone1.output("audio"), mixer.input("input_0"))?;
runtime.connect(tone2.output("audio"), mixer.input("input_1"))?;
runtime.connect(tone3.output("audio"), mixer.input("input_2"))?;
runtime.connect(mixer.output("mixed"), effect.input("audio"))?;
runtime.connect(effect.output("audio"), speaker.input("audio"))?;

runtime.start().await?;
```

#### Runtime Execution

**Thread Model**:
- Thread 1: TestToneGenerator #1 source loop
- Thread 2: TestToneGenerator #2 source loop
- Thread 3: TestToneGenerator #3 source loop
- Thread 4: AudioMixer handler (reactive)
- Thread 5: ClapEffect handler (reactive)
- Thread 6: AudioOutputProcessor handler (reactive)
- Hardware: CoreAudio callback thread (drives audio clock)

**Timeline** (at 23.44 Hz audio clock):

```
Time: 0ms - Clock: 0ns
├─ AudioOutputProcessor: Start CoreAudio, provide clock
├─ TestTone1: Loop starts, sync_time = 0ns
├─ TestTone2: Loop starts, sync_time = 0ns
├─ TestTone3: Loop starts, sync_time = 0ns
└─ AudioMixer: Waiting on DataAvailable
└─ ClapEffect: Waiting on DataAvailable
└─ Speaker: Waiting on DataAvailable

Time: 0ms - TestTone1 generates first buffer
├─ TestTone1.generate() → AudioFrame (2048 samples)
├─ TestTone1.clock_sync_point() → Duration(42.67ms)
├─ TestTone1: next_sync_time = 42.67ms
├─ Clock.now() = 0.3ms (took 0.3ms to generate)
├─ Wait: 42.67ms - 0.3ms = 42.37ms
├─ TestTone1: sleep(42.37ms)

Time: 0.1ms - TestTone2 generates first buffer
├─ TestTone2.generate() → AudioFrame
├─ Similar timing calculation
├─ TestTone2: sleep(~42ms)

Time: 0.2ms - TestTone3 generates first buffer
├─ TestTone3.generate() → AudioFrame
├─ Similar timing calculation
├─ TestTone3: sleep(~42ms)

Time: 42.67ms - Clock reaches first sync point
├─ TestTone1 wakes up
├─ TestTone1.audio.write(frame) → Writes to port
├─ Port sends DataAvailable to AudioMixer
├─ AudioMixer wakes up (Event #1)
├─ AudioMixer.transform(DataAvailable):
│   ├─ Read input_0 → Some(frame from tone1)
│   ├─ Read input_1 → None (not ready yet)
│   ├─ Read input_2 → None (not ready yet)
│   ├─ sample_hold["input_0"] = frame
│   └─ Can't mix yet (only 1/3 inputs)
├─ AudioMixer goes back to waiting

Time: 42.77ms - TestTone2 reaches sync point (~0.1ms after tone1)
├─ TestTone2.audio.write(frame)
├─ Port sends DataAvailable to AudioMixer
├─ AudioMixer wakes up (Event #2)
├─ AudioMixer.transform(DataAvailable):
│   ├─ Read input_0 → None (already read)
│   ├─ Read input_1 → Some(frame from tone2)
│   ├─ Read input_2 → None (not ready yet)
│   ├─ sample_hold["input_1"] = frame
│   └─ Can't mix yet (only 2/3 inputs)

Time: 42.87ms - TestTone3 reaches sync point
├─ TestTone3.audio.write(frame)
├─ Port sends DataAvailable to AudioMixer
├─ AudioMixer wakes up (Event #3)
├─ AudioMixer.transform(DataAvailable):
│   ├─ Read input_0 → None
│   ├─ Read input_1 → None
│   ├─ Read input_2 → Some(frame from tone3)
│   ├─ sample_hold["input_2"] = frame
│   ├─ Check: have all 3 inputs? YES!
│   ├─ Mix: combine all 3 frames
│   ├─ Write to mixed output port
│   └─ Port sends DataAvailable to ClapEffect

├─ ClapEffect wakes up
├─ ClapEffect.transform(DataAvailable):
│   ├─ Read audio_in → Some(mixed frame)
│   ├─ Process through CLAP plugin
│   ├─ Write to audio_out port
│   └─ Port sends DataAvailable to Speaker

├─ Speaker wakes up
├─ Speaker.accept_data(reverb_frame):
│   └─ sample_buffer.extend(frame.samples)

Time: 43ms - CoreAudio callback fires (needs 2048 samples)
├─ Callback reads sample_buffer
├─ Drains 2048 samples
├─ Updates audio_clock.samples_played += 2048
├─ Plays to hardware

Time: 43-85ms - TestTones waiting for next clock sync
├─ All 3 TestTone threads sleeping until ~85ms
├─ Mixer, Effect, Speaker waiting on DataAvailable

Time: 85.34ms - Next sync point
├─ Cycle repeats!
```

**Key Observations**:
1. ✅ AudioMixer wakes **3 times** per generation cycle (once per input)
2. ✅ This is CORRECT behavior (mixer needs to collect all inputs)
3. ✅ Only mixes when ALL inputs ready (sample-and-hold pattern)
4. ✅ Entire chain processes in ~0.5ms (43ms → 43.5ms)
5. ✅ No buffer overrun because sources self-regulate via clock
6. ✅ Sources generate at 23.44 Hz (matched to audio consumption)

**Why this fixes buffer overrun**:
- OLD: Timer group wakes mixer 3x → mixer processes 3x → outputs 3 frames
- NEW: Mixer wakes 3x but only mixes ONCE (when all inputs ready)
- NEW: Sources self-regulate (can't generate faster than clock allows)

---

## How This Fixes Buffer Overrun

### Root Cause Revisited

**Current architecture problem**:
```
Timer Group "audio_master" @ 23.44 Hz
├─ Wakes: TestTone1, TestTone2, TestTone3, AudioMixer
│
├─ TestTone1: process() → write → DataAvailable → Mixer
├─ TestTone2: process() → write → DataAvailable → Mixer
├─ TestTone3: process() → write → DataAvailable → Mixer
│
└─ AudioMixer: Receives 3 DataAvailable events
    ├─ process() #1 → reads all 3 inputs → mixes → writes
    ├─ process() #2 → reads all 3 inputs → mixes → writes  ← DUPLICATE!
    └─ process() #3 → reads all 3 inputs → mixes → writes  ← DUPLICATE!

Result: 3 mixed frames per tick instead of 1
Buffer: Fills 3x faster than consumed → 8000%+ overrun
```

**Why timestamp deduplication didn't work**:
```
TestTone1 generates at t=0.0ms → timestamp = 1000000000ns
TestTone2 generates at t=0.6ms → timestamp = 1000600000ns  (different!)
TestTone3 generates at t=1.2ms → timestamp = 1001200000ns  (different!)

Mixer checks: "Are timestamps same as last mix?"
NO! Because each tone has different generation timestamp
→ Deduplication fails
→ Mixer processes multiple times
```

### New Architecture Solution

**Key changes**:
1. **Remove AudioMixer from timer group** - It's a StreamTransform, purely reactive
2. **Sources self-regulate via clock** - Can't generate faster than clock allows
3. **Mixer wakes 3x but mixes once** - Sample-and-hold pattern

**New flow**:
```
AudioClock @ 48000 Hz (hardware-driven)
│
├─ TestTone1 Loop (independent thread)
│   ├─ generate() → frame
│   ├─ clock_sync_point() → 42.67ms
│   ├─ Check clock: wait 42.67ms
│   ├─ write → DataAvailable → Mixer wakes (#1)
│   └─ Loop again
│
├─ TestTone2 Loop (independent thread)
│   ├─ Similar timing
│   ├─ write → DataAvailable → Mixer wakes (#2)
│   └─ Loop again
│
├─ TestTone3 Loop (independent thread)
│   ├─ Similar timing
│   ├─ write → DataAvailable → Mixer wakes (#3)
│   └─ Loop again
│
└─ AudioMixer Handler (reactive)
    ├─ Wakeup #1: Read input_0, wait for others
    ├─ Wakeup #2: Read input_1, wait for input_2
    ├─ Wakeup #3: Read input_2, NOW MIX!
    └─ Output: 1 mixed frame per cycle
```

**Why it works**:
1. ✅ **Sources can't overproduce**: Clock sync prevents generating faster than 23.44 Hz
2. ✅ **Mixer collects, not duplicates**: Wakes 3x but sample-and-hold prevents re-mixing same data
3. ✅ **Hardware clock accuracy**: AudioClock tied to actual hardware consumption rate
4. ✅ **No timestamp conflicts**: Each source timestamps when it generates (accurate)
5. ✅ **Buffer stays healthy**: Production rate (23.44 Hz) matches consumption rate (23.44 Hz)

### Performance Comparison

**Before (Timer Groups)**:
```
Time: 0ms - Timer tick
├─ TestTone1: process() → 2048 samples
├─ TestTone2: process() → 2048 samples
├─ TestTone3: process() → 2048 samples
├─ AudioMixer: process() #1 → 2048 samples
├─ AudioMixer: process() #2 → 2048 samples  ← Extra!
├─ AudioMixer: process() #3 → 2048 samples  ← Extra!
└─ Buffer: += 6144 samples (should be 2048!)

Time: 42.67ms - Timer tick
├─ Repeat
└─ Buffer: += 6144 samples

Time: 85.34ms - Timer tick
├─ Repeat
└─ Buffer: += 6144 samples

After 1 second (23.44 ticks):
├─ Expected buffer: 23.44 * 2048 = 48,011 samples
├─ Actual buffer: 23.44 * 6144 = 144,034 samples
└─ Overrun: 300%!

After 5 seconds:
└─ Buffer: 720,170 samples → 8000%+ overrun!
```

**After (New Architecture)**:
```
Time: 0ms - TestTones start loops
├─ TestTone1: generate() → write
├─ TestTone2: generate() → write
├─ TestTone3: generate() → write
├─ AudioMixer: Collect all 3 → mix once → 2048 samples
└─ Buffer: += 2048 samples ✅

Time: 42.67ms - Clock sync point
├─ TestTones wake, generate, write
├─ Mixer collects, mixes once
└─ Buffer: += 2048 samples ✅

Time: 85.34ms - Clock sync point
├─ Repeat
└─ Buffer: += 2048 samples ✅

After 1 second:
├─ Expected: 48,011 samples
├─ Actual: 48,011 samples
└─ Perfect match! ✅

After 5 seconds:
├─ Buffer: ~48,000 samples (steady state)
└─ No overrun! ✅
```

### Additional Benefits

1. **Graceful clock drift handling**:
   ```rust
   let now = clock.now_ns();
   if now > next_sync_time_ns {
       // We're behind schedule - skip ahead
       tracing::warn!("Source lagging by {}ms", (now - next_sync_time) / 1_000_000);
       next_sync_time_ns = now;
   }
   ```

2. **Hardware synchronization**: AudioClock tracks actual hardware sample consumption
   - No drift between generation and consumption
   - Sample-accurate timing

3. **Supports variable-rate sources**: Network streams, variable frame rate video
   ```rust
   // Network source can adjust rate based on jitter buffer
   fn clock_sync_point(&self) -> Duration {
       self.jitter_buffer.target_delay()
   }
   ```

4. **Clear debugging**: Each processor type has obvious role
   - Sources: Check `now()` vs `next_sync_time`
   - Transforms: Check wakeup events
   - Sinks: Check render timing

---

## Migration Path

This is a breaking change requiring all processors to migrate to new traits.

### Migration Strategy

#### Phase 1: Parallel Implementation (Week 1-2)
- Implement new traits alongside existing `StreamProcessor`
- Both systems coexist temporarily
- New processors use new traits
- Existing processors still work

#### Phase 2: Migrate Core Processors (Week 2-3)
- Migrate streamlib-provided processors first
- Provides examples for custom processors
- Update all examples
- Test thoroughly

#### Phase 3: Deprecation & Migration Tools (Week 3)
- Mark old `StreamProcessor` as deprecated
- Provide migration guide
- Create automated migration tools where possible
- Compile-time warnings for old API

#### Phase 4: Remove Old System (Week 4)
- Remove deprecated `StreamProcessor`
- Clean up runtime code
- Final testing
- Documentation updates

### Automated Migration Support

**Macro-based migration**:
```rust
// OLD:
impl StreamProcessor for TestToneGenerator {
    fn process(&mut self) -> Result<()> {
        // ... 90 lines of boilerplate
    }
}

// NEW (automated via macro):
#[derive(StreamSource)]
#[scheduling(mode = "loop", clock = "audio")]
struct TestToneGenerator {
    // Macro auto-generates everything!
}
```

**Migration CLI tool**:
```bash
$ cargo streamlib migrate --file src/my_processor.rs

Analyzing TestToneGenerator...
✓ Detected: Source processor (no inputs)
✓ Detected: Timer-driven (has TimerRequirements)
✓ Detected: Audio clock (rate = 23.44 Hz)

Suggested migration:
  - Derive: StreamSource
  - Scheduling: mode = "loop", clock = "audio"
  - Extract: generate() from process()

Apply changes? [y/N]
```

### Per-Processor Migration Guide

#### TestToneGenerator: StreamProcessor → StreamSource

**Before**:
```rust
pub struct TestToneGenerator {
    frequency: f64,
    sample_rate: u32,
    channels: u32,
    phase: f64,
    amplitude: f64,
    frame_number: u64,
    buffer_size: usize,
    timer_group_id: Option<String>,  // OLD: Manual timer group
    output_ports: TestToneGeneratorOutputPorts,
}

impl StreamProcessor for TestToneGenerator {
    type Config = TestToneConfig;
    
    fn from_config(config: Self::Config) -> Result<Self> {
        let mut gen = Self::new(config.frequency, config.sample_rate, config.amplitude);
        gen.timer_group_id = config.timer_group_id;
        Ok(gen)
    }
    
    fn descriptor_instance(&self) -> Option<ProcessorDescriptor> {
        Self::descriptor().map(|desc| {
            desc.with_timer_requirements(TimerRequirements {
                rate_hz: self.timer_rate_hz(),
                group_id: self.timer_group_id.clone(),
                description: Some(format!("...")),
            })
        })
    }
    
    fn process(&mut self) -> Result<()> {
        let timestamp_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;
        
        let frame = self.generate_frame(timestamp_ns);
        self.output_ports.audio.write(frame);
        Ok(())
    }
    
    // ... 60 more lines of boilerplate
}
```

**After**:
```rust
#[derive(StreamSource)]
#[scheduling(mode = "loop", clock = "audio")]
struct TestToneGenerator {
    #[output()]
    audio: StreamOutput<AudioFrame>,
    
    // Config fields (auto-extracted)
    frequency: f64,
    amplitude: f64,
    sample_rate: u32,
    
    // Internal state (not config)
    #[skip_config]
    phase: f64,
    
    #[skip_config]
    frame_number: u64,
}

impl TestToneGenerator {
    fn generate(&mut self) -> Result<AudioFrame> {
        // Just the core logic
        let samples = self.generate_sine_wave();
        self.frame_number += 1;
        Ok(AudioFrame::new(samples, self.sample_rate, 2))
    }
    
    fn clock_sync_point(&self) -> Duration {
        Duration::from_secs_f64(2048.0 / self.sample_rate as f64)
    }
}
```

**Changes**:
- ❌ Remove: `timer_group_id`, manual timer requirements
- ✅ Add: `#[scheduling(...)]` attribute
- ✅ Simplify: `generate()` instead of `process()`
- ✅ Remove: 90 lines of boilerplate (macro generates)

#### ClapEffectProcessor: StreamProcessor → StreamTransform

**Before**:
```rust
impl StreamProcessor for ClapEffectProcessor {
    fn process(&mut self) -> Result<()> {
        // Read input
        if let Some(frame) = self.input_ports.audio.read_latest() {
            // Process
            let output = self.clap_process(&frame.samples)?;
            // Write output
            self.output_ports.audio.write(AudioFrame::new(output, ...));
        }
        Ok(())
    }
}
```

**After**:
```rust
#[derive(StreamTransform)]
struct ClapEffectProcessor {
    #[input()]
    audio_in: StreamInput<AudioFrame>,
    
    #[output()]
    audio_out: StreamOutput<AudioFrame>,
    
    plugin_path: PathBuf,
    // ... config fields
}

impl ClapEffectProcessor {
    fn transform(&mut self, event: WakeupEvent) -> Result<()> {
        if let WakeupEvent::DataAvailable = event {
            if let Some(frame) = self.audio_in.read_latest() {
                let output = self.clap_process(&frame.samples)?;
                self.audio_out.write(AudioFrame::new(output, ...));
            }
        }
        Ok(())
    }
}
```

**Changes**:
- ✅ Add: `#[derive(StreamTransform)]`
- ✅ Rename: `process()` → `transform()`
- ✅ Add: `WakeupEvent` parameter
- ✅ Automatic: No scheduling attribute (transforms always reactive)

#### AudioOutputProcessor: StreamProcessor → StreamSink

**Before**:
```rust
impl StreamProcessor for AudioOutputProcessor {
    fn process(&mut self) -> Result<()> {
        if let Some(frame) = self.input_ports.audio.read_latest() {
            let mut buffer = self.sample_buffer.lock();
            buffer.extend_from_slice(&frame.samples);
        }
        Ok(())
    }
}
```

**After**:
```rust
#[derive(StreamSink)]
#[scheduling(mode = "callback", hardware = "coreaudio", provide_clock = true)]
struct AudioOutputProcessor {
    #[input()]
    audio: StreamInput<AudioFrame>,
    
    device_id: Option<usize>,
    
    #[skip_config]
    sample_buffer: Arc<Mutex<Vec<f32>>>,
    
    #[skip_config]
    audio_clock: Arc<AudioClock>,
}

impl AudioOutputProcessor {
    fn accept_data(&mut self, frame: AudioFrame) {
        let mut buffer = self.sample_buffer.lock();
        buffer.extend_from_slice(&frame.samples);
    }
    
    fn provide_clock(&self) -> Option<Arc<dyn Clock>> {
        Some(self.audio_clock.clone())
    }
}
```

**Changes**:
- ✅ Add: `#[scheduling(mode = "callback", provide_clock = true)]`
- ✅ Rename: `process()` → `accept_data()`
- ✅ Add: `provide_clock()` method
- ✅ Runtime handles: Hardware callback setup

### Breaking Changes Checklist

**Trait Changes**:
- ❌ REMOVED: `StreamProcessor` trait
- ✅ NEW: `StreamSource` trait
- ✅ NEW: `StreamSink` trait
- ✅ NEW: `StreamTransform` trait

**Method Signature Changes**:
- ❌ REMOVED: `process(&mut self) -> Result<()>`
- ✅ NEW (Source): `generate(&mut self) -> Result<Output>`
- ✅ NEW (Source): `clock_sync_point(&self) -> Duration`
- ✅ NEW (Sink): `render(&mut self, input: Input) -> Result<()>`
- ✅ NEW (Sink): `provide_clock(&self) -> Option<Arc<dyn Clock>>`
- ✅ NEW (Transform): `transform(&mut self, event: WakeupEvent) -> Result<()>`

**Config Changes**:
- ❌ REMOVED: `timer_group_id` field
- ❌ REMOVED: Manual `TimerRequirements` in descriptors
- ✅ NEW: `#[scheduling(...)]` attributes replace manual configuration

**Runtime API Changes**:
- ❌ REMOVED: `runtime.add_processor::<P>()` (generic over old trait)
- ✅ NEW: `runtime.add_processor::<P>()` (generic over Source/Sink/Transform)
- ✅ SAME: Connection API unchanged (`connect()` still works)

---

## Implementation Plan

### Week 1: Foundation & Traits

**Goals**: Define new trait hierarchy, clock infrastructure, basic runtime support

**Tasks**:
1. **Define traits** (libs/streamlib/src/core/traits/)
   - [ ] Create `traits/` module
   - [ ] Define `StreamSource` trait
   - [ ] Define `StreamSink` trait
   - [ ] Define `StreamTransform` trait
   - [ ] Define `ProcessorType` enum (union type)

2. **Clock infrastructure** (libs/streamlib/src/core/clock/)
   - [ ] Define `Clock` trait
   - [ ] Implement `SoftwareClock`
   - [ ] Implement `AudioClock` (CoreAudio integration)
   - [ ] Implement `VideoClock` (CVDisplayLink integration)

3. **Runtime foundation** (libs/streamlib/src/core/runtime.rs)
   - [ ] Add `pipeline_clock: Option<Arc<dyn Clock>>`
   - [ ] Implement clock selection logic
   - [ ] Update `add_processor()` to handle new traits

4. **Tests**:
   - [ ] Clock trait tests
   - [ ] SoftwareClock accuracy tests
   - [ ] AudioClock mock tests

**Deliverables**:
- ✅ All 3 traits defined
- ✅ Clock infrastructure working
- ✅ Runtime can query clocks
- ✅ Tests passing

### Week 2: Core Processors & Execution

**Goals**: Implement execution models, migrate core processors

**Tasks**:
1. **Source loop execution** (runtime.rs)
   - [ ] Implement `spawn_source_loop()`
   - [ ] Clock synchronization logic
   - [ ] Drift handling

2. **Transform reactive execution** (runtime.rs)
   - [ ] Implement `spawn_transform_handler()`
   - [ ] Ensure only DataAvailable wakeups

3. **Sink callback execution** (runtime.rs)
   - [ ] Implement `spawn_sink_handler()`
   - [ ] Implement `spawn_callback_sink()` for hardware
   - [ ] Clock sync before rendering

4. **Migrate core processors**:
   - [ ] TestToneGenerator → StreamSource
   - [ ] ClapEffectProcessor → StreamTransform
   - [ ] AudioOutputProcessor → StreamSink
   - [ ] AudioCaptureProcessor → StreamSource
   - [ ] DisplayProcessor → StreamSink

5. **Tests**:
   - [ ] Source loop timing tests
   - [ ] Transform reactive tests
   - [ ] Sink clock sync tests
   - [ ] End-to-end pipeline test

**Deliverables**:
- ✅ All execution models implemented
- ✅ Core audio processors migrated
- ✅ audio-mixer-demo working with new architecture
- ✅ No buffer overruns!

### Week 3: Macros & Tooling

**Goals**: Declarative scheduling API, migration tools

**Tasks**:
1. **Derive macros** (libs/streamlib-macros/)
   - [ ] `#[derive(StreamSource)]`
   - [ ] `#[derive(StreamSink)]`
   - [ ] `#[derive(StreamTransform)]`
   - [ ] `#[scheduling(...)]` attribute parsing
   - [ ] Config extraction
   - [ ] Descriptor generation

2. **Migration tooling**:
   - [ ] CLI tool for automated migration
   - [ ] Processor analysis
   - [ ] Suggested changes
   - [ ] Apply transformations

3. **Update all processors** using macros:
   - [ ] Refactor TestToneGenerator to use macro
   - [ ] Refactor all processors to use macros
   - [ ] Remove manual boilerplate

4. **Update examples**:
   - [ ] audio-mixer-demo
   - [ ] microphone-reverb-speaker
   - [ ] camera-display
   - [ ] All other examples

5. **Tests**:
   - [ ] Macro expansion tests
   - [ ] Config extraction tests
   - [ ] Descriptor generation tests

**Deliverables**:
- ✅ All derive macros working
- ✅ Migration tool functional
- ✅ All processors using declarative API
- ✅ All examples working

### Week 4: Polish, Testing, Documentation

**Goals**: Production-ready, comprehensive testing, complete docs

**Tasks**:
1. **Remove old system**:
   - [ ] Delete old `StreamProcessor` trait
   - [ ] Delete timer group code
   - [ ] Clean up runtime
   - [ ] Remove deprecated APIs

2. **Comprehensive testing**:
   - [ ] Unit tests for all components
   - [ ] Integration tests for all pipelines
   - [ ] Performance benchmarks
   - [ ] Stress tests (long-running stability)
   - [ ] Clock accuracy validation

3. **Documentation**:
   - [ ] Update CLAUDE.md files
   - [ ] API documentation (rustdoc)
   - [ ] Migration guide
   - [ ] Tutorial for new processors
   - [ ] Architecture diagrams

4. **Performance validation**:
   - [ ] Buffer stability tests (no overrun)
   - [ ] Latency measurements
   - [ ] CPU usage profiling
   - [ ] Memory usage validation

5. **Edge case handling**:
   - [ ] Clock drift scenarios
   - [ ] Processor failures (reconnection)
   - [ ] Network jitter (for future network sources)
   - [ ] Hot-swap processors

**Deliverables**:
- ✅ Old system removed
- ✅ All tests passing
- ✅ Performance validated (buffer healthy)
- ✅ Documentation complete
- ✅ Ready for v2.0.0 release

---

## Testing Strategy

### Unit Tests

#### Clock Tests
```rust
#[test]
fn test_software_clock_monotonic() {
    let clock = SoftwareClock::new(None);
    let t1 = clock.now_ns();
    std::thread::sleep(Duration::from_millis(10));
    let t2 = clock.now_ns();
    assert!(t2 > t1);
}

#[test]
fn test_audio_clock_sample_accurate() {
    let clock = AudioClock::new(48000);
    assert_eq!(clock.samples_played.load(Ordering::Relaxed), 0);
    
    // Simulate callback
    clock.samples_played.fetch_add(2048, Ordering::Relaxed);
    
    let expected_ns = (2048.0 / 48000.0 * 1e9) as i64;
    let actual_ns = clock.now_ns() - clock.base_time;
    assert_eq!(actual_ns, expected_ns);
}
```

#### Source Loop Tests
```rust
#[test]
fn test_source_loop_timing() {
    let tone = TestToneGenerator::new(440.0, 48000, 0.5);
    let clock = Arc::new(SoftwareClock::new(Some(23.44)));
    
    // Run loop for 100ms
    let start = Instant::now();
    let mut frames = 0;
    
    while start.elapsed() < Duration::from_millis(100) {
        tone.generate().unwrap();
        frames += 1;
        // Simulate clock sync wait
        std::thread::sleep(tone.clock_sync_point());
    }
    
    // Should generate ~2-3 frames (23.44 Hz * 0.1s ≈ 2.344)
    assert!(frames >= 2 && frames <= 3);
}
```

### Integration Tests

#### Simple Pipeline Test
```rust
#[tokio::test]
async fn test_source_sink_pipeline() {
    let mut runtime = StreamRuntime::new();
    
    // Add source
    let tone = runtime.add_processor_with_config::<TestToneGenerator>(
        TestToneConfig { frequency: 440.0, amplitude: 0.5, sample_rate: 48000 }
    )?;
    
    // Add sink
    let speaker = runtime.add_processor_with_config::<AudioOutputProcessor>(
        AudioOutputConfig { device_id: None }
    )?;
    
    // Connect
    runtime.connect(tone.output("audio"), speaker.input("audio"))?;
    
    // Run for 1 second
    runtime.start().await?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    runtime.stop().await?;
    
    // Verify no buffer overrun
    let buffer_size = speaker.buffer_len();
    assert!(buffer_size < 10000, "Buffer overrun: {} samples", buffer_size);
}
```

#### Multi-Source Mixer Test
```rust
#[tokio::test]
async fn test_multi_source_mixing() {
    let mut runtime = StreamRuntime::new();
    
    // 3 sources
    let tone1 = runtime.add_processor_with_config::<TestToneGenerator>(
        TestToneConfig { frequency: 440.0, amplitude: 0.15, sample_rate: 48000 }
    )?;
    let tone2 = runtime.add_processor_with_config::<TestToneGenerator>(
        TestToneConfig { frequency: 554.37, amplitude: 0.15, sample_rate: 48000 }
    )?;
    let tone3 = runtime.add_processor_with_config::<TestToneGenerator>(
        TestToneConfig { frequency: 659.25, amplitude: 0.15, sample_rate: 48000 }
    )?;
    
    // Mixer
    let mixer = runtime.add_processor_with_config::<AudioMixer>(
        AudioMixerConfig { num_inputs: 3, strategy: MixingStrategy::SumNormalized }
    )?;
    
    // Sink
    let speaker = runtime.add_processor_with_config::<AudioOutputProcessor>(
        AudioOutputConfig { device_id: None }
    )?;
    
    // Connect
    runtime.connect(tone1.output("audio"), mixer.input("input_0"))?;
    runtime.connect(tone2.output("audio"), mixer.input("input_1"))?;
    runtime.connect(tone3.output("audio"), mixer.input("input_2"))?;
    runtime.connect(mixer.output("mixed"), speaker.input("audio"))?;
    
    // Run
    runtime.start().await?;
    tokio::time::sleep(Duration::from_secs(5)).await;
    
    // Verify buffer stability
    let buffer_size = speaker.buffer_len();
    assert!(buffer_size < 10000, "Buffer overrun: {} samples", buffer_size);
    
    runtime.stop().await?;
}
```

### Performance Benchmarks

#### Buffer Stability Benchmark
```rust
#[bench]
fn bench_buffer_stability(b: &mut Bencher) {
    let mut runtime = StreamRuntime::new();
    // ... set up pipeline ...
    
    runtime.start().await?;
    
    let mut max_buffer = 0;
    let mut samples = Vec::new();
    
    for _ in 0..1000 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let buffer_size = speaker.buffer_len();
        max_buffer = max_buffer.max(buffer_size);
        samples.push(buffer_size);
    }
    
    // Statistics
    let avg = samples.iter().sum::<usize>() / samples.len();
    let stddev = /* calculate */;
    
    println!("Buffer stats: avg={}, max={}, stddev={}", avg, max_buffer, stddev);
    assert!(max_buffer < 10000);
    assert!(stddev < 1000); // Low variance = stable
}
```

#### Latency Measurement
```rust
#[bench]
fn bench_pipeline_latency(b: &mut Bencher) {
    // Measure time from source.generate() to sink.render()
    // Target: < 10ms for real-time
}
```

### Stress Tests

#### Long-Running Stability
```rust
#[test]
#[ignore] // Run with --ignored
fn test_24_hour_stability() {
    let mut runtime = StreamRuntime::new();
    // ... set up pipeline ...
    
    runtime.start().await?;
    
    let duration = Duration::from_secs(24 * 60 * 60);
    let mut buffer_samples = Vec::new();
    let start = Instant::now();
    
    while start.elapsed() < duration {
        tokio::time::sleep(Duration::from_secs(60)).await;
        buffer_samples.push(speaker.buffer_len());
    }
    
    // Verify no drift, no overrun over 24 hours
    let max = buffer_samples.iter().max().unwrap();
    assert!(max < &10000);
}
```

---

## Success Criteria

### Functional Requirements

- ✅ **Buffer overrun fixed**: Buffer stays at ~2048 samples (±20%) indefinitely
- ✅ **Audio plays smoothly**: No pops, clicks, or distortion
- ✅ **Logs show correct behavior**: Mixer called once per input arrival, mixes when all ready
- ✅ **Clock synchronization works**: Sources generate at exact clock rate (no drift)
- ✅ **All processors migrated**: No processors left using old `StreamProcessor`
- ✅ **Examples work**: audio-mixer-demo and all others run without errors

### Performance Requirements

- ✅ **Latency**: < 10ms from source to sink (real-time)
- ✅ **CPU usage**: < 5% for typical pipeline (3 sources + mixer + effect + sink)
- ✅ **Memory stable**: No leaks, steady-state memory usage
- ✅ **Clock accuracy**: < 1ms drift over 1 hour

### Developer Experience

- ✅ **Simple API**: New processor in < 20 lines (with macros)
- ✅ **Clear errors**: Compile-time errors for misconfigurations
- ✅ **Good docs**: Every processor type has tutorial + examples
- ✅ **Easy migration**: Automated tool migrates 80%+ of processors

### Architecture Quality

- ✅ **Matches GStreamer principles**: Clock as reference, sources loop, transforms reactive
- ✅ **Type safety**: Compiler prevents wrong processor type in wrong context
- ✅ **Extensible**: Easy to add new scheduling modes, clock types
- ✅ **Testable**: Each component independently testable

---

## Open Questions

Track questions that arise during design/implementation:

1. **Q**: Should we support mixed-mode processors (both timer AND reactive)?
   **Status**: Open
   **Discussion**: Might be needed for complex processors, but adds complexity

2. **Q**: How to handle network sources with variable jitter?
   **Status**: Open  
   **Discussion**: Need WebRTC-style jitter buffer, adaptive playback

3. **Q**: Should sinks be able to pull from sources (pull mode)?
   **Status**: Deferred to v2.1
   **Discussion**: Useful for file I/O, app-driven playback

4. **Q**: How to handle hot-swapping processors while running?
   **Status**: Open
   **Discussion**: Important for power armor (camera/mic failures), but complex

5. **Q**: Should we support GStreamer's SEGMENT events for complex timing?
   **Status**: Deferred
   **Discussion**: Might be needed for video editing use cases

---

## References

### GStreamer Documentation

- [Plugin Development - Scheduling](https://gstreamer.freedesktop.org/documentation/plugin-development/advanced/scheduling.html)
- [Design - Synchronisation](https://gstreamer.freedesktop.org/documentation/additional/design/synchronisation.html)
- [GstBaseSrc source code](https://gitlab.freedesktop.org/gstreamer/gstreamer/-/blob/main/libs/gst/base/gstbasesrc.c)
- [GstBaseSink source code](https://gitlab.freedesktop.org/gstreamer/gstreamer/-/blob/main/libs/gst/base/gstbasesink.c)
- [audiomixer documentation](https://gstreamer.freedesktop.org/documentation/audiomixer/audiomixer.html)

### WebRTC

- [NetEQ Jitter Buffer](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/)
- [WebRTC Architecture](https://webrtc.org/architecture/)

### Related Systems

- [PipeWire Graph Scheduling](https://docs.pipewire.org/page_scheduling.html)
- [JACK Audio Connection Kit](https://jackaudio.org/api/)
- [CoreAudio](https://developer.apple.com/documentation/coreaudio)
- [CVDisplayLink](https://developer.apple.com/documentation/corevideo/cvdisplaylink)

### streamlib Current Architecture

- [/CLAUDE.md](../../CLAUDE.md) - Repository-wide guidelines
- [libs/streamlib/CLAUDE.md](../../libs/streamlib/CLAUDE.md) - Unified crate docs
- [libs/streamlib/src/core/CLAUDE.md](../../libs/streamlib/src/core/CLAUDE.md) - Core layer docs

### Commit History

- Commit 86aa735: "feat(audio): Implement timer groups and AudioMixer improvements"
  - Timer groups implementation
  - Timestamp-based deduplication
  - GStreamer research findings

---

## Appendix: Complete API Reference

### StreamElement Trait (Base)

```rust
/// Base trait for all stream processing elements
///
/// Inspired by GStreamer's GstElement, provides common functionality
/// for metadata, lifecycle management, and introspection.
///
/// All processors (Source/Sink/Transform) inherit from this trait.
pub trait StreamElement: Send + 'static {
    /// Element name (for logging, debugging, MCP)
    ///
    /// Should be unique within a runtime instance.
    /// Auto-generated from struct name by derive macro.
    fn name(&self) -> &str;

    /// Element type (Source/Sink/Transform)
    ///
    /// Used by runtime to dispatch to appropriate execution model.
    /// Auto-generated based on which trait is derived.
    fn element_type(&self) -> ElementType;

    /// Processor descriptor for MCP/AI discoverability
    ///
    /// Auto-generated from struct definition, attributes, and doc comments.
    fn descriptor(&self) -> Option<ProcessorDescriptor>;

    /// Lifecycle: Start processing
    ///
    /// Called by runtime when pipeline starts.
    /// Override to initialize hardware, open files, etc.
    /// Default implementation does nothing.
    fn start(&mut self) -> Result<()> {
        Ok(())
    }

    /// Lifecycle: Stop processing
    ///
    /// Called by runtime when pipeline stops.
    /// Should gracefully pause processing but maintain state.
    /// Override to pause hardware, flush buffers, etc.
    /// Default implementation does nothing.
    fn stop(&mut self) -> Result<()> {
        Ok(())
    }

    /// Lifecycle: Shutdown (cleanup resources)
    ///
    /// Called when element is being removed from runtime.
    /// Should release all resources (hardware, memory, threads).
    /// Override to close hardware, free resources, etc.
    /// Default implementation does nothing.
    fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }

    /// Get input port descriptors
    ///
    /// Returns metadata about all input ports.
    /// Auto-generated from #[input(...)] attributes by derive macro.
    /// Default: empty (for sources with no inputs).
    fn input_ports(&self) -> Vec<PortDescriptor> {
        Vec::new()
    }

    /// Get output port descriptors
    ///
    /// Returns metadata about all output ports.
    /// Auto-generated from #[output(...)] attributes by derive macro.
    /// Default: empty (for sinks with no outputs).
    fn output_ports(&self) -> Vec<PortDescriptor> {
        Vec::new()
    }

    /// Downcast to StreamSource (if applicable)
    ///
    /// Returns Some if this element is a Source, None otherwise.
    /// Auto-generated by derive macro.
    fn as_source(&self) -> Option<&dyn StreamSource> {
        None
    }

    /// Downcast to StreamSink (if applicable)
    ///
    /// Returns Some if this element is a Sink, None otherwise.
    /// Auto-generated by derive macro.
    fn as_sink(&self) -> Option<&dyn StreamSink> {
        None
    }

    /// Downcast to StreamTransform (if applicable)
    ///
    /// Returns Some if this element is a Transform, None otherwise.
    /// Auto-generated by derive macro.
    fn as_transform(&self) -> Option<&dyn StreamTransform> {
        None
    }
}

pub enum ElementType {
    Source,
    Sink,
    Transform,
}
```

### StreamSource Trait

```rust
/// Source processors that generate data
///
/// Sources have no inputs, only outputs.
/// They run in continuous loops or hardware callbacks.
///
/// Inherits from StreamElement for common functionality.
pub trait StreamSource: StreamElement {
    /// Output data type
    type Output: FrameData;

    /// Configuration struct type
    type Config: DeserializeOwned + Serialize;

    /// Construct from config
    fn from_config(config: Self::Config) -> Result<Self> where Self: Sized;

    /// Generate next output frame
    /// Called by runtime in loop or callback
    fn generate(&mut self) -> Result<Self::Output>;

    /// Duration to wait before next generation
    /// Used by runtime for clock synchronization
    fn clock_sync_point(&self) -> Duration;

    /// Processor descriptor for MCP
    fn descriptor() -> Option<ProcessorDescriptor>;

    /// Scheduling configuration
    /// Generated by #[scheduling(...)] macro
    fn scheduling_config(&self) -> SchedulingConfig;

    // Inherits from StreamElement:
    // - fn name(&self) -> &str;
    // - fn element_type(&self) -> ElementType; (returns ElementType::Source)
    // - fn start/stop/shutdown(&mut self) -> Result<()>;
    // - fn output_ports(&self) -> Vec<PortDescriptor>;
    // - fn as_source(&self) -> Option<&dyn StreamSource>; (returns Some(self))
}
```

### StreamSink Trait

```rust
/// Sink processors that consume data
///
/// Sinks have inputs but no outputs (terminal processors).
/// They render to hardware, network, or application.
///
/// Inherits from StreamElement for common functionality.
pub trait StreamSink: StreamElement {
    /// Input data type
    type Input: FrameData;

    /// Configuration struct type
    type Config: DeserializeOwned + Serialize;

    /// Construct from config
    fn from_config(config: Self::Config) -> Result<Self> where Self: Sized;

    /// Render input frame
    /// For reactive sinks: called when data arrives
    /// For callback sinks: called from accept_data()
    fn render(&mut self, input: Self::Input) -> Result<()>;

    /// Accept data for callback-driven sinks
    /// Queues data for hardware callback
    /// Optional: only needed for callback mode
    fn accept_data(&mut self, input: Self::Input) {
        // Default: immediate render
        self.render(input).ok();
    }

    /// Provide clock for pipeline
    /// Return Some if this sink provides hardware clock
    fn provide_clock(&self) -> Option<Arc<dyn Clock>> {
        None
    }

    /// Processor descriptor for MCP
    fn descriptor() -> Option<ProcessorDescriptor>;

    /// Scheduling configuration
    fn scheduling_config(&self) -> SchedulingConfig;

    // Inherits from StreamElement:
    // - fn name(&self) -> &str;
    // - fn element_type(&self) -> ElementType; (returns ElementType::Sink)
    // - fn start/stop/shutdown(&mut self) -> Result<()>;
    // - fn input_ports(&self) -> Vec<PortDescriptor>;
    // - fn as_sink(&self) -> Option<&dyn StreamSink>; (returns Some(self))
}
```

### StreamTransform Trait

```rust
/// Transform processors that process data
///
/// Transforms have both inputs and outputs (any configuration).
/// They are purely reactive, waking only on DataAvailable events.
///
/// Inherits from StreamElement for common functionality.
pub trait StreamTransform: StreamElement {
    /// Configuration struct type
    type Config: DeserializeOwned + Serialize;

    /// Construct from config
    fn from_config(config: Self::Config) -> Result<Self> where Self: Sized;
    
    /// Transform data
    /// Called when WakeupEvent arrives
    /// Typically only processes on DataAvailable
    fn transform(&mut self, event: WakeupEvent) -> Result<()>;

    /// Processor descriptor for MCP
    fn descriptor() -> Option<ProcessorDescriptor>;

    // Inherits from StreamElement:
    // - fn name(&self) -> &str;
    // - fn element_type(&self) -> ElementType; (returns ElementType::Transform)
    // - fn start/stop/shutdown(&mut self) -> Result<()>;
    // - fn input_ports(&self) -> Vec<PortDescriptor>;
    // - fn output_ports(&self) -> Vec<PortDescriptor>;
    // - fn as_transform(&self) -> Option<&dyn StreamTransform>; (returns Some(self))
}
```

### SchedulingConfig Struct

```rust
pub struct SchedulingConfig {
    /// Scheduling mode
    pub mode: SchedulingMode,
    
    /// Clock source to sync against
    pub clock: ClockSource,
    
    /// Software timer rate (if clock = Software)
    pub rate_hz: Option<f64>,
    
    /// Does this processor provide a clock?
    pub provide_clock: bool,
    
    /// Hardware type (if mode = Callback)
    pub hardware: Option<HardwareType>,
}

pub enum SchedulingMode {
    Loop,      // Continuous loop (sources)
    Callback,  // Hardware-driven (sources/sinks)
    Reactive,  // Event-driven (sources/transforms/sinks)
    Pull,      // App-driven (sinks)
}

pub enum ClockSource {
    Audio,     // Use pipeline audio clock
    Vsync,     // Use display vsync clock
    Software,  // Software timer
    Custom,    // Custom clock provided by processor
}

pub enum HardwareType {
    CoreAudio,
    AVFoundation,
    CVDisplayLink,
    V4L2,
    // ... more hardware types
}
```

---

**End of Document**

---

## Document Metadata

- **Total Length**: ~15,000 words
- **Sections**: 15 major sections
- **Code Examples**: 50+ snippets
- **Diagrams**: 5 flow diagrams (ASCII art)
- **References**: 15+ external resources

**Review Status**: ✅ Ready for review
**Next Steps**: Team review → Approve → Begin implementation Week 1

