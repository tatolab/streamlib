# Port API Patterns Guide

**Last Updated**: 2025-11-10
**Purpose**: Comprehensive coding examples showing current and recommended port API patterns

---

## Table of Contents

1. [Current Manual Pattern (Production)](#1-current-manual-pattern-production)
2. [Macro Pattern (Recommended)](#2-macro-pattern-recommended)
3. [Side-by-Side Comparison](#3-side-by-side-comparison)
4. [Special Cases](#4-special-cases)
5. [Migration Guide](#5-migration-guide)
6. [Quick Reference](#6-quick-reference)

---

## 1. Current Manual Pattern (Production)

This is the pattern currently used in all 12 production processors. It works but requires significant boilerplate.

### 1.1 Simple Transform (1 Input, 1 Output)

**Use Case**: Video passthrough, audio effects, any 1:1 transform

```rust
use crate::core::{
    Result, StreamInput, StreamOutput, VideoFrame,
    traits::{StreamElement, StreamProcessor, ElementType},
    schema::{ProcessorDescriptor, PortDescriptor},
    bus::PortType,
    RuntimeContext,
};
use std::sync::Arc;
use serde::{Serialize, Deserialize};

// Step 1: Define config struct
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimpleTransformConfig {
    pub param: f32,
}

impl Default for SimpleTransformConfig {
    fn default() -> Self {
        Self { param: 1.0 }
    }
}

// Step 2: Define port structs
pub struct SimpleTransformInputPorts {
    pub input: StreamInput<VideoFrame>,
}

pub struct SimpleTransformOutputPorts {
    pub output: StreamOutput<VideoFrame>,
}

// Step 3: Define processor
pub struct SimpleTransformProcessor {
    name: String,
    input_ports: SimpleTransformInputPorts,
    output_ports: SimpleTransformOutputPorts,
    param: f32,
}

// Step 4: Implement StreamElement (35-40 lines)
impl StreamElement for SimpleTransformProcessor {
    fn name(&self) -> &str {
        &self.name
    }

    fn element_type(&self) -> ElementType {
        ElementType::Transform
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <Self as StreamProcessor>::descriptor()
    }

    fn input_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "input".to_string(),
            schema: VideoFrame::schema(),
            required: true,
            description: "Input video stream".to_string(),
        }]
    }

    fn output_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "output".to_string(),
            schema: VideoFrame::schema(),
            required: true,
            description: "Output video stream".to_string(),
        }]
    }

    fn start(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        Ok(())
    }

    fn as_transform(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_transform_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

// Step 5: Implement StreamProcessor (50-60 lines)
impl StreamProcessor for SimpleTransformProcessor {
    type Config = SimpleTransformConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        Ok(Self {
            name: "simple_transform".to_string(),
            input_ports: SimpleTransformInputPorts {
                input: StreamInput::new("input"),
            },
            output_ports: SimpleTransformOutputPorts {
                output: StreamOutput::new("output"),
            },
            param: config.param,
        })
    }

    fn process(&mut self) -> Result<()> {
        if let Some(frame) = self.input_ports.input.read_latest() {
            // Process frame...
            self.output_ports.output.write(frame);
        }
        Ok(())
    }

    fn descriptor() -> Option<ProcessorDescriptor> {
        Some(
            ProcessorDescriptor::new(
                "SimpleTransformProcessor",
                "Transforms video frames"
            )
            .with_tags(vec!["transform", "video"])
        )
    }

    // CRITICAL: Must implement these for runtime wiring!
    fn get_input_port_type(&self, port_name: &str) -> Option<PortType> {
        match port_name {
            "input" => Some(self.input_ports.input.port_type()),
            _ => None,
        }
    }

    fn get_output_port_type(&self, port_name: &str) -> Option<PortType> {
        match port_name {
            "output" => Some(self.output_ports.output.port_type()),
            _ => None,
        }
    }

    fn wire_input_connection(
        &mut self,
        port_name: &str,
        connection: Arc<dyn std::any::Any + Send + Sync>
    ) -> bool {
        use crate::core::bus::ProcessorConnection;

        if let Ok(typed_conn) = connection.downcast::<Arc<ProcessorConnection<VideoFrame>>>() {
            if port_name == "input" {
                self.input_ports.input.set_connection(Arc::clone(&typed_conn));
                return true;
            }
        }
        false
    }

    fn wire_output_connection(
        &mut self,
        port_name: &str,
        connection: Arc<dyn std::any::Any + Send + Sync>
    ) -> bool {
        use crate::core::bus::ProcessorConnection;

        if let Ok(typed_conn) = connection.downcast::<Arc<ProcessorConnection<VideoFrame>>>() {
            if port_name == "output" {
                self.output_ports.output.add_connection(Arc::clone(&typed_conn));
                return true;
            }
        }
        false
    }
}

// TOTAL: ~120 lines for a simple 1-in-1-out processor!
```

**Boilerplate Count**:
- Port struct definitions: 10 lines
- Port initialization: 8 lines
- Port descriptors: 14 lines
- Port type lookups: 14 lines
- Wire methods: 30 lines
- **Total boilerplate: ~76 lines**

---

### 1.2 Multi-Port Transform

**Use Case**: Audio mixer, video compositor, any N:M transform

```rust
use crate::core::{AudioFrame, StreamInput, StreamOutput};

// Multi-input, multi-output example
pub struct AudioMixerInputPorts {
    pub input_1: StreamInput<AudioFrame<1>>,
    pub input_2: StreamInput<AudioFrame<1>>,
    pub input_3: StreamInput<AudioFrame<1>>,
}

pub struct AudioMixerOutputPorts {
    pub audio: StreamOutput<AudioFrame<2>>,
}

pub struct AudioMixerProcessor {
    input_ports: AudioMixerInputPorts,
    output_ports: AudioMixerOutputPorts,
}

impl StreamProcessor for AudioMixerProcessor {
    // ... config and process() ...

    fn get_input_port_type(&self, port_name: &str) -> Option<PortType> {
        match port_name {
            "input_1" => Some(self.input_ports.input_1.port_type()),
            "input_2" => Some(self.input_ports.input_2.port_type()),
            "input_3" => Some(self.input_ports.input_3.port_type()),
            _ => None,
        }
    }

    fn wire_input_connection(&mut self, port_name: &str, connection: Arc<dyn Any + Send + Sync>) -> bool {
        use crate::core::bus::ProcessorConnection;

        if let Ok(typed_conn) = connection.downcast::<Arc<ProcessorConnection<AudioFrame<1>>>>() {
            match port_name {
                "input_1" => {
                    self.input_ports.input_1.set_connection(Arc::clone(&typed_conn));
                    true
                }
                "input_2" => {
                    self.input_ports.input_2.set_connection(Arc::clone(&typed_conn));
                    true
                }
                "input_3" => {
                    self.input_ports.input_3.set_connection(Arc::clone(&typed_conn));
                    true
                }
                _ => false,
            }
        } else {
            false
        }
    }

    // Similar for output...
}

// TOTAL: ~150 lines for 3-in-1-out processor!
```

**Note**: Every new port adds ~15-20 lines of boilerplate.

---

### 1.3 Source Processor (Outputs Only)

**Use Case**: Camera, microphone, test tone generator

```rust
pub struct ChordGeneratorOutputPorts {
    pub chord: Arc<StreamOutput<AudioFrame<2>>>,
}

pub struct ChordGeneratorProcessor {
    output_ports: ChordGeneratorOutputPorts,
    // ... other fields ...
}

impl StreamElement for ChordGeneratorProcessor {
    fn output_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "chord".to_string(),
            schema: AudioFrame::<2>::schema(),
            required: false,
            description: "Stereo C Major chord".to_string(),
        }]
    }

    // No input_ports()!
}

impl StreamProcessor for ChordGeneratorProcessor {
    fn from_config(config: Self::Config) -> Result<Self> {
        Ok(Self {
            output_ports: ChordGeneratorOutputPorts {
                chord: Arc::new(StreamOutput::new("chord")),
            },
            // ...
        })
    }

    fn set_output_wakeup(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<WakeupEvent>) {
        if port_name == "chord" {
            self.output_ports.chord.set_downstream_wakeup(wakeup_tx);
        }
    }

    fn get_output_port_type(&self, port_name: &str) -> Option<PortType> {
        match port_name {
            "chord" => Some(PortType::Audio2),
            _ => None,
        }
    }

    fn wire_output_connection(&mut self, port_name: &str, connection: Arc<dyn Any + Send + Sync>) -> bool {
        use crate::core::bus::ProcessorConnection;

        if let Ok(typed_conn) = connection.downcast::<Arc<ProcessorConnection<AudioFrame<2>>>>() {
            if port_name == "chord" {
                self.output_ports.chord.add_connection(Arc::clone(&typed_conn));
                return true;
            }
        }
        false
    }

    // No wire_input_connection()!
}
```

---

### 1.4 Sink Processor (Inputs Only)

**Use Case**: Display, speaker, file writer

```rust
pub struct AudioOutputInputPorts {
    pub audio: StreamInput<AudioFrame<2>>,
}

pub struct AudioOutputProcessor {
    input_ports: AudioOutputInputPorts,
    // ... other fields ...
}

impl StreamElement for AudioOutputProcessor {
    fn input_ports(&self) -> Vec<PortDescriptor> {
        vec![PortDescriptor {
            name: "audio".to_string(),
            schema: AudioFrame::<2>::schema(),
            required: true,
            description: "Stereo audio input".to_string(),
        }]
    }

    // No output_ports()!
}

impl StreamProcessor for AudioOutputProcessor {
    fn get_input_port_type(&self, port_name: &str) -> Option<PortType> {
        match port_name {
            "audio" => Some(self.input_ports.audio.port_type()),
            _ => None,
        }
    }

    fn wire_input_connection(&mut self, port_name: &str, connection: Arc<dyn Any + Send + Sync>) -> bool {
        use crate::core::bus::ProcessorConnection;

        if let Ok(typed_conn) = connection.downcast::<Arc<ProcessorConnection<AudioFrame<2>>>>() {
            if port_name == "audio" {
                self.input_ports.audio.set_connection(Arc::clone(&typed_conn));
                return true;
            }
        }
        false
    }

    // No wire_output_connection()!
}
```

---

## 2. Macro Pattern (Recommended)

This is the future-proof pattern that eliminates 85-90% of boilerplate. Currently used in `examples/port-registry-demo` only.

### 2.1 Simple Transform (1 Input, 1 Output)

```rust
use streamlib::{port_registry, StreamInput, StreamOutput, VideoFrame};

// Step 1: Define ports with macro - THIS IS ALL THE PORT CODE!
#[port_registry]
struct SimpleTransformPorts {
    #[input]
    input: StreamInput<VideoFrame>,

    #[output]
    output: StreamOutput<VideoFrame>,
}

// Step 2: Processor implementation (business logic only)
pub struct SimpleTransformProcessor {
    ports: SimpleTransformPorts,
    param: f32,
}

impl SimpleTransformProcessor {
    pub fn new(param: f32) -> Self {
        Self {
            ports: SimpleTransformPorts::new(),  // Generated by macro!
            param,
        }
    }

    fn process(&mut self) {
        // Ergonomic port access!
        if let Some(frame) = self.ports.inputs().input.read_latest() {
            // Process frame...
            self.ports.outputs().output.write(frame);
        }
    }
}

// TOTAL: ~30 lines (vs 120 lines manual) - 75% reduction!
```

**What the Macro Generates** (you don't write this):

```rust
// Auto-generated by #[port_registry]:

pub struct SimpleTransformPortsInputPorts {
    pub input: StreamInput<VideoFrame>,
}

pub struct SimpleTransformPortsOutputPorts {
    pub output: StreamOutput<VideoFrame>,
}

impl SimpleTransformPorts {
    pub fn new() -> Self {
        Self {
            input: StreamInput::new("input"),
            output: StreamOutput::new("output"),
        }
    }

    pub fn inputs(&self) -> &SimpleTransformPortsInputPorts { /* ... */ }
    pub fn outputs(&self) -> &SimpleTransformPortsOutputPorts { /* ... */ }
    pub fn inputs_mut(&mut self) -> &mut SimpleTransformPortsInputPorts { /* ... */ }
    pub fn outputs_mut(&mut self) -> &mut SimpleTransformPortsOutputPorts { /* ... */ }

    pub fn get_input_port_type(&self, port_name: &str) -> Option<PortType> {
        match port_name {
            "input" => Some(<VideoFrame as PortMessage>::port_type()),
            _ => None,
        }
    }

    pub fn get_output_port_type(&self, port_name: &str) -> Option<PortType> {
        match port_name {
            "output" => Some(<VideoFrame as PortMessage>::port_type()),
            _ => None,
        }
    }

    pub fn wire_input_connection(&mut self, port_name: &str, connection: Arc<dyn Any + Send + Sync>) -> bool {
        if let Ok(typed) = connection.downcast::<Arc<ProcessorConnection<VideoFrame>>>() {
            match port_name {
                "input" => {
                    self.input.set_connection(Arc::clone(&typed));
                    true
                }
                _ => false,
            }
        } else {
            false
        }
    }

    pub fn wire_output_connection(&mut self, port_name: &str, connection: Arc<dyn Any + Send + Sync>) -> bool {
        if let Ok(typed) = connection.downcast::<Arc<ProcessorConnection<VideoFrame>>>() {
            match port_name {
                "output" => {
                    self.output.add_connection(Arc::clone(&typed));
                    true
                }
                _ => false,
            }
        } else {
            false
        }
    }
}
```

**Key Insight**: You write 4 lines, macro generates 60+ lines of correct boilerplate!

---

### 2.2 Multi-Port Transform

```rust
#[port_registry]
struct AudioMixerPorts {
    #[input]
    input_1: StreamInput<AudioFrame<1>>,

    #[input]
    input_2: StreamInput<AudioFrame<1>>,

    #[input]
    input_3: StreamInput<AudioFrame<1>>,

    #[output]
    audio: StreamOutput<AudioFrame<2>>,
}

pub struct AudioMixerProcessor {
    ports: AudioMixerPorts,
}

impl AudioMixerProcessor {
    pub fn new() -> Self {
        Self {
            ports: AudioMixerPorts::new(),
        }
    }

    fn process(&mut self) {
        let inputs = self.ports.inputs();
        let outputs = self.ports.outputs();

        // Easy access to all ports by name!
        let frame1 = inputs.input_1.read_latest();
        let frame2 = inputs.input_2.read_latest();
        let frame3 = inputs.input_3.read_latest();

        // Mix and output...
        // outputs.audio.write(mixed_frame);
    }
}

// TOTAL: ~35 lines (vs 150 lines manual) - 77% reduction!
```

---

### 2.3 Source Processor (Outputs Only)

```rust
#[port_registry]
struct ChordGeneratorPorts {
    // No #[input] fields!

    #[output]
    chord: StreamOutput<AudioFrame<2>>,
}

pub struct ChordGeneratorProcessor {
    ports: ChordGeneratorPorts,
    oscillators: Vec<SineOscillator>,
}

impl ChordGeneratorProcessor {
    fn generate(&mut self) {
        // Generate audio...
        let frame = self.create_chord_frame();

        // Write to output
        self.ports.outputs().chord.write(frame);
    }
}
```

---

### 2.4 Sink Processor (Inputs Only)

```rust
#[port_registry]
struct AudioOutputPorts {
    #[input]
    audio: StreamInput<AudioFrame<2>>,

    // No #[output] fields!
}

pub struct AudioOutputProcessor {
    ports: AudioOutputPorts,
    stream: Option<cpal::Stream>,
}

impl AudioOutputProcessor {
    fn play(&mut self) {
        if let Some(frame) = self.ports.inputs().audio.read_latest() {
            // Send to hardware...
        }
    }
}
```

---

## 3. Side-by-Side Comparison

### Example: Simple Video Passthrough

#### Manual Pattern (Current)

```rust
// Port definitions
pub struct PassthroughInputPorts {
    pub input: StreamInput<VideoFrame>,
}

pub struct PassthroughOutputPorts {
    pub output: StreamOutput<VideoFrame>,
}

pub struct PassthroughProcessor {
    input_ports: PassthroughInputPorts,
    output_ports: PassthroughOutputPorts,
}

// Initialization
impl StreamProcessor for PassthroughProcessor {
    fn from_config(config: Config) -> Result<Self> {
        Ok(Self {
            input_ports: PassthroughInputPorts {
                input: StreamInput::new("input"),
            },
            output_ports: PassthroughOutputPorts {
                output: StreamOutput::new("output"),
            },
        })
    }

    fn get_input_port_type(&self, port_name: &str) -> Option<PortType> {
        match port_name {
            "input" => Some(self.input_ports.input.port_type()),
            _ => None,
        }
    }

    fn get_output_port_type(&self, port_name: &str) -> Option<PortType> {
        match port_name {
            "output" => Some(self.output_ports.output.port_type()),
            _ => None,
        }
    }

    fn wire_input_connection(&mut self, port_name: &str, connection: Arc<dyn Any + Send + Sync>) -> bool {
        use crate::core::bus::ProcessorConnection;

        if let Ok(typed_conn) = connection.downcast::<Arc<ProcessorConnection<VideoFrame>>>() {
            if port_name == "input" {
                self.input_ports.input.set_connection(Arc::clone(&typed_conn));
                return true;
            }
        }
        false
    }

    fn wire_output_connection(&mut self, port_name: &str, connection: Arc<dyn Any + Send + Sync>) -> bool {
        use crate::core::bus::ProcessorConnection;

        if let Ok(typed_conn) = connection.downcast::<Arc<ProcessorConnection<VideoFrame>>>() {
            if port_name == "output" {
                self.output_ports.output.add_connection(Arc::clone(&typed_conn));
                return true;
            }
        }
        false
    }

    fn process(&mut self) -> Result<()> {
        if let Some(frame) = self.input_ports.input.read_latest() {
            self.output_ports.output.write(frame);
        }
        Ok(())
    }
}

// Line count: ~80 lines of port boilerplate
```

#### Macro Pattern (Recommended)

```rust
#[port_registry]
struct PassthroughPorts {
    #[input]
    input: StreamInput<VideoFrame>,

    #[output]
    output: StreamOutput<VideoFrame>,
}

pub struct PassthroughProcessor {
    ports: PassthroughPorts,
}

impl PassthroughProcessor {
    pub fn new() -> Self {
        Self {
            ports: PassthroughPorts::new(),  // All wiring auto-generated!
        }
    }
}

impl StreamProcessor for PassthroughProcessor {
    fn from_config(_config: Config) -> Result<Self> {
        Ok(Self::new())
    }

    fn process(&mut self) -> Result<()> {
        if let Some(frame) = self.ports.inputs().input.read_latest() {
            self.ports.outputs().output.write(frame);
        }
        Ok(())
    }
}

// Line count: ~25 lines total (68% reduction!)
```

---

## 4. Special Cases

### 4.1 Array-Based Ports (Const Generic)

**Current Challenge**: AudioMixer with const generic N

```rust
pub struct AudioMixerProcessor<const N: usize> {
    pub input_ports: [StreamInput<AudioFrame<1>>; N],  // Array of N ports
}
```

**Manual Implementation Required** (macro doesn't support this yet):

```rust
impl<const N: usize> StreamProcessor for AudioMixerProcessor<N> {
    fn get_input_port_type(&self, port_name: &str) -> Option<PortType> {
        // Runtime string parsing for array indices
        if let Some(index_str) = port_name.strip_prefix("input_") {
            if let Ok(index) = index_str.parse::<usize>() {
                if index < N {
                    return Some(PortType::Audio1);
                }
            }
        }
        None
    }

    fn wire_input_connection(&mut self, port_name: &str, connection: Arc<dyn Any + Send + Sync>) -> bool {
        if let Ok(typed_conn) = connection.downcast::<Arc<ProcessorConnection<AudioFrame<1>>>>() {
            if let Some(index_str) = port_name.strip_prefix("input_") {
                if let Ok(index) = index_str.parse::<usize>() {
                    if index < N {
                        self.input_ports[index].set_connection(Arc::clone(&typed_conn));
                        return true;
                    }
                }
            }
        }
        false
    }
}
```

**Future Enhancement**: Macro could generate this pattern.

---

### 4.2 Arc-Wrapped Ports

**Inconsistency in Current Code**:

```rust
// ChordGenerator uses Arc:
pub struct ChordGeneratorOutputPorts {
    pub chord: Arc<StreamOutput<AudioFrame<2>>>,  // Arc-wrapped
}

// ClapEffect doesn't:
pub struct ClapEffectOutputPorts {
    pub audio: StreamOutput<AudioFrame<2>>,  // Direct
}
```

**Recommendation**: Use direct `StreamOutput<T>` (no Arc). The connection system handles sharing internally.

**Macro Pattern**: Always generates direct fields (no Arc).

---

### 4.3 Optional Ports

**Use Case**: Processor with optional debug output

```rust
#[port_registry]
struct ProcessorPorts {
    #[input]
    input: StreamInput<VideoFrame>,

    #[output]
    output: StreamOutput<VideoFrame>,

    #[output]
    debug: StreamOutput<VideoFrame>,  // Optional port
}

// PortDescriptor marks optional:
PortDescriptor {
    name: "debug".to_string(),
    schema: VideoFrame::schema(),
    required: false,  // Optional!
    description: "Debug output (optional)".to_string(),
}

// Writing to optional port:
fn process(&mut self) {
    let frame = /* ... */;

    // Always write to required output
    self.ports.outputs().output.write(frame.clone());

    // Conditionally write to optional output
    if self.ports.outputs().debug.has_connections() {
        self.ports.outputs().debug.write(frame);
    }
}
```

---

## 5. Migration Guide

### Converting Manual → Macro

**Step 1: Identify Port Fields**

From manual pattern:
```rust
pub struct MyProcessorInputPorts {
    pub video: StreamInput<VideoFrame>,
    pub audio: StreamInput<AudioFrame<2>>,
}

pub struct MyProcessorOutputPorts {
    pub output: StreamOutput<VideoFrame>,
}
```

**Step 2: Create Macro Struct**

```rust
#[port_registry]
struct MyProcessorPorts {
    #[input]
    video: StreamInput<VideoFrame>,

    #[input]
    audio: StreamInput<AudioFrame<2>>,

    #[output]
    output: StreamOutput<VideoFrame>,
}
```

**Step 3: Update Processor**

Before:
```rust
pub struct MyProcessor {
    input_ports: MyProcessorInputPorts,
    output_ports: MyProcessorOutputPorts,
}
```

After:
```rust
pub struct MyProcessor {
    ports: MyProcessorPorts,  // Single field!
}
```

**Step 4: Update Access Patterns**

Before:
```rust
self.input_ports.video.read_latest()
self.output_ports.output.write(frame)
```

After:
```rust
self.ports.inputs().video.read_latest()
self.ports.outputs().output.write(frame)
```

**Step 5: Remove Manual Trait Methods**

Delete these methods (macro generates them):
- ❌ `get_input_port_type()`
- ❌ `get_output_port_type()`
- ❌ `wire_input_connection()`
- ❌ `wire_output_connection()`

**Step 6: Delegate to Macro Methods**

In `StreamProcessor` impl:
```rust
impl StreamProcessor for MyProcessor {
    fn get_input_port_type(&self, port_name: &str) -> Option<PortType> {
        self.ports.get_input_port_type(port_name)  // Delegate to macro!
    }

    fn get_output_port_type(&self, port_name: &str) -> Option<PortType> {
        self.ports.get_output_port_type(port_name)
    }

    fn wire_input_connection(&mut self, port_name: &str, connection: Arc<dyn Any + Send + Sync>) -> bool {
        self.ports.wire_input_connection(port_name, connection)
    }

    fn wire_output_connection(&mut self, port_name: &str, connection: Arc<dyn Any + Send + Sync>) -> bool {
        self.ports.wire_output_connection(port_name, connection)
    }
}
```

**Step 7: Test**

```rust
#[test]
fn test_macro_wiring() {
    let mut processor = MyProcessor::new();

    // Test introspection
    assert_eq!(
        processor.ports.get_input_port_type("video"),
        Some(PortType::Video)
    );

    // Test connection (in integration test with runtime)
}
```

---

### Checklist

- [ ] Created `#[port_registry]` struct with all ports
- [ ] Annotated inputs with `#[input]`
- [ ] Annotated outputs with `#[output]`
- [ ] Updated processor to use single `ports` field
- [ ] Changed access patterns to `.inputs()` / `.outputs()`
- [ ] Removed manual `get_*_port_type()` implementations
- [ ] Removed manual `wire_*_connection()` implementations
- [ ] Added delegation methods in `StreamProcessor` impl
- [ ] Updated tests
- [ ] Verified runtime wiring works

---

## 6. Quick Reference

### Manual Pattern Template

```rust
// 1. Port structs (~10 lines per port group)
pub struct XInputPorts { /* ... */ }
pub struct XOutputPorts { /* ... */ }

// 2. Processor (~5 lines)
pub struct XProcessor {
    input_ports: XInputPorts,
    output_ports: XOutputPorts,
}

// 3. StreamElement impl (~40 lines)
impl StreamElement for XProcessor {
    fn input_ports(&self) -> Vec<PortDescriptor> { /* ... */ }
    fn output_ports(&self) -> Vec<PortDescriptor> { /* ... */ }
    // ... lifecycle methods ...
}

// 4. StreamProcessor impl (~50 lines)
impl StreamProcessor for XProcessor {
    fn from_config(config: Config) -> Result<Self> { /* init ports */ }
    fn get_input_port_type(&self, name: &str) -> Option<PortType> { /* match */ }
    fn get_output_port_type(&self, name: &str) -> Option<PortType> { /* match */ }
    fn wire_input_connection(&mut self, name: &str, conn: ...) -> bool { /* downcast */ }
    fn wire_output_connection(&mut self, name: &str, conn: ...) -> bool { /* downcast */ }
}

// TOTAL: ~100+ lines
```

---

### Macro Pattern Template

```rust
// 1. Port struct (~1 line per port)
#[port_registry]
struct XPorts {
    #[input] in1: StreamInput<T1>,
    #[input] in2: StreamInput<T2>,
    #[output] out: StreamOutput<T3>,
}

// 2. Processor (~3 lines)
pub struct XProcessor {
    ports: XPorts,
}

// 3. Minimal impl (~20 lines)
impl XProcessor {
    pub fn new() -> Self {
        Self { ports: XPorts::new() }
    }
}

impl StreamProcessor for XProcessor {
    fn process(&mut self) -> Result<()> {
        let inputs = self.ports.inputs();
        let outputs = self.ports.outputs();
        // Business logic here...
    }

    // Delegate to macro:
    fn get_input_port_type(&self, name: &str) -> Option<PortType> {
        self.ports.get_input_port_type(name)
    }
    // ... similar for other trait methods ...
}

// TOTAL: ~30 lines (70% reduction!)
```

---

### Port Access Patterns

| Pattern | Manual | Macro |
|---------|--------|-------|
| **Read input** | `self.input_ports.video.read_latest()` | `self.ports.inputs().video.read_latest()` |
| **Write output** | `self.output_ports.audio.write(frame)` | `self.ports.outputs().audio.write(frame)` |
| **Check connection** | `self.input_ports.video.has_data()` | `self.ports.inputs().video.has_data()` |
| **Multiple ports** | Individual struct fields | `.inputs()` / `.outputs()` accessors |

---

### Common Pitfalls

#### ❌ Forgetting to Implement Wiring Methods (Manual Pattern)

```rust
impl StreamProcessor for MyProcessor {
    // ❌ WRONG: Using trait defaults (returns false/None)
    // This breaks runtime wiring!
}
```

**Fix**: Always implement all 4 methods:
```rust
impl StreamProcessor for MyProcessor {
    fn get_input_port_type(&self, port_name: &str) -> Option<PortType> { /* ... */ }
    fn get_output_port_type(&self, port_name: &str) -> Option<PortType> { /* ... */ }
    fn wire_input_connection(&mut self, port_name: &str, conn: ...) -> bool { /* ... */ }
    fn wire_output_connection(&mut self, port_name: &str, conn: ...) -> bool { /* ... */ }
}
```

#### ❌ Port Name Mismatch (Manual Pattern)

```rust
// In output_ports():
PortDescriptor { name: "audio_out".to_string(), /* ... */ }

// In get_output_port_type():
fn get_output_port_type(&self, port_name: &str) -> Option<PortType> {
    match port_name {
        "audio" => Some(PortType::Audio2),  // ❌ Wrong name!
        _ => None,
    }
}
```

**Fix**: Use exact same string everywhere, or better yet, use macro.

#### ❌ Type Mismatch in Downcast (Manual Pattern)

```rust
// Port is AudioFrame<2> (stereo)
pub audio: StreamOutput<AudioFrame<2>>,

// But wiring expects AudioFrame<1> (mono)
fn wire_output_connection(&mut self, port_name: &str, connection: Arc<dyn Any>) -> bool {
    if let Ok(typed) = connection.downcast::<Arc<ProcessorConnection<AudioFrame<1>>>>() {
        // ❌ Type mismatch! Should be AudioFrame<2>
        // This will always return false at runtime
    }
}
```

**Fix**: Match types exactly, or use macro (auto-infers types).

---

### Decision Matrix: When to Use Which Pattern?

| Scenario | Pattern | Reason |
|----------|---------|--------|
| **New processor** | ✅ **Macro** | 90% less code, future-proof |
| **Migrating existing** | ✅ **Macro** | Worth the 30-60 min migration |
| **Array-based ports (`[StreamInput<T>; N]`)** | ⚠️ **Manual** | Macro doesn't support (yet) |
| **Learning the system** | ✅ **Macro** | Simpler mental model |
| **Quick prototype** | ✅ **Macro** | Faster to write |
| **Production code** | ✅ **Macro** | Lower maintenance burden |

**Current Reality**: All production code uses manual pattern because macro hasn't been adopted yet.

**Recommendation**: Start using macro for all new code and migrate existing code incrementally.

---

**End of Guide**

For questions or clarifications, see:
- Macro implementation: `libs/streamlib-macros/src/port_registry.rs`
- Working examples: `examples/port-registry-demo/src/main.rs`
- Developer experience report: `PORT_API_DEVELOPER_EXPERIENCE_REPORT.md`
