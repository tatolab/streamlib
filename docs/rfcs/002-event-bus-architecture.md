# RFC 002: Event Bus Architecture

## Status
Proposed

## Summary
Introduce a centralized event bus using the `bus` crate for lock-free pub/sub messaging across the runtime, processors, and external consumers (UI, keyboard, network).

## Motivation

### Current Limitations
1. **No runtime control**: Can't pause/resume processors externally
2. **No keyboard/mouse input**: No way to handle user input events
3. **No error propagation**: Processors can't signal errors to runtime/UI
4. **No observability**: Can't monitor processor state changes
5. **Pull mode shutdown**: Infinite loops can't receive shutdown signals

### Goals
1. **Unified messaging**: Single event bus for all runtime communication
2. **External control**: Keyboard, network, UI can send commands
3. **Observability**: Subscribe to any processor/runtime state change
4. **Low latency**: <100ns overhead for realtime audio/video paths
5. **Type-safe**: Compile-time checked event types

## Design

### Core Architecture

```rust
// Centralized event bus (lock-free broadcast)
pub struct EventBus {
    inner: Arc<Mutex<bus::Bus<RuntimeEvent>>>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(bus::Bus::new(capacity))),
        }
    }

    /// Subscribe to all events (like addEventListener)
    pub fn subscribe(&self) -> EventReceiver {
        EventReceiver {
            rx: self.inner.lock().unwrap().add_rx(),
        }
    }

    /// Broadcast event to all subscribers
    pub fn emit(&self, event: RuntimeEvent) {
        self.inner.lock().unwrap().broadcast(event);
    }

    /// Try to emit without blocking (for realtime paths)
    pub fn try_emit(&self, event: RuntimeEvent) -> bool {
        if let Ok(mut bus) = self.inner.try_lock() {
            bus.broadcast(event);
            true
        } else {
            false
        }
    }
}

pub struct EventReceiver {
    rx: bus::BusReader<RuntimeEvent>,
}

impl EventReceiver {
    /// Non-blocking receive (for realtime loops)
    pub fn try_recv(&mut self) -> Option<RuntimeEvent> {
        self.rx.try_recv().ok()
    }

    /// Blocking receive (for event handlers)
    pub fn recv(&mut self) -> Option<RuntimeEvent> {
        self.rx.recv().ok()
    }
}
```

### Event Types

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum RuntimeEvent {
    // ===== Processor Lifecycle =====
    ProcessorSetup {
        id: ProcessorId,
        name: String,
    },
    ProcessorTeardown {
        id: ProcessorId,
    },

    // ===== Processor State Control =====
    ProcessorStart {
        id: Option<ProcessorId>, // None = all processors
    },
    ProcessorStop {
        id: Option<ProcessorId>,
    },
    ProcessorPause {
        id: Option<ProcessorId>,
    },
    ProcessorResume {
        id: Option<ProcessorId>,
    },

    // ===== Processor Commands (Custom State Changes) =====
    ProcessorCommand {
        id: ProcessorId,
        command: ProcessorCommand,
    },

    // ===== Processor Status Events =====
    ProcessorError {
        id: ProcessorId,
        error: String,
    },
    ProcessorStateChanged {
        id: ProcessorId,
        old_state: ProcessorState,
        new_state: ProcessorState,
    },

    // ===== Runtime Control =====
    RuntimeStart,
    RuntimeStop,
    RuntimeShutdown,
    RuntimeError {
        error: String,
    },

    // ===== Input Events =====
    KeyboardInput {
        key: KeyCode,
        modifiers: Modifiers,
        state: KeyState, // Pressed, Released, Held
    },
    MouseInput {
        button: MouseButton,
        position: (f64, f64),
        state: MouseState,
    },
    WindowEvent {
        event: WindowEventType,
    },

    // ===== Network/External Events =====
    NetworkCommand {
        source: String, // IP or identifier
        command: ProcessorCommand,
        target: Option<ProcessorId>,
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ProcessorCommand {
    // Generic commands
    SetParameter {
        name: String,
        value: serde_json::Value,
    },

    // Processor-specific commands (namespaced)
    Custom {
        namespace: String, // e.g., "chord_generator", "clap_plugin"
        command: String,   // e.g., "play_note", "set_clap_param"
        args: serde_json::Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ProcessorState {
    Idle,      // Setup complete, not processing
    Running,   // Actively processing
    Paused,    // Paused (resources still allocated)
    Error,     // Error state
}

// Input types
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum KeyCode {
    A, B, C, /* ... */, Space, Enter, Escape, /* ... */
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum KeyState {
    Pressed,
    Released,
    Held,
}
```

### Runtime Integration

```rust
pub struct StreamRuntime {
    processors: Vec<Box<dyn DynStreamProcessor>>,
    event_bus: EventBus,
    context: RuntimeContext,
    event_loop: Option<EventLoopFn>,
}

impl StreamRuntime {
    pub fn new() -> Self {
        Self {
            processors: Vec::new(),
            event_bus: EventBus::new(1024), // 1024 event capacity
            context: RuntimeContext::default(),
            event_loop: None,
        }
    }

    /// Get reference to event bus for external subscriptions
    pub fn event_bus(&self) -> &EventBus {
        &self.event_bus
    }

    pub fn add_processor<P: StreamProcessorFactory>(&mut self) -> Result<ProcessorHandle<P>> {
        let config = P::Config::default();
        let mut processor = P::from_config_with_bus(config, &self.event_bus)?;

        let id = ProcessorId::new();
        let name = std::any::type_name::<P>().to_string();

        // Setup processor
        processor.setup(&self.context)?;

        // Emit setup event
        self.event_bus.emit(RuntimeEvent::ProcessorSetup {
            id,
            name: name.clone(),
        });

        // Store processor
        self.processors.push(Box::new(processor));

        Ok(ProcessorHandle::new(id, name))
    }

    pub fn run(&mut self) -> Result<()> {
        // Emit runtime start
        self.event_bus.emit(RuntimeEvent::RuntimeStart);

        // Start all processors
        self.event_bus.emit(RuntimeEvent::ProcessorStart { id: None });

        // Spawn processor threads (they subscribe to events internally)
        for (id, processor) in self.processors.iter_mut().enumerate() {
            self.spawn_processor_thread(ProcessorId(id), processor);
        }

        // Run platform event loop (keyboard, window events)
        if let Some(event_loop) = self.event_loop.take() {
            event_loop()?;
        } else {
            // Default: wait for shutdown event
            let mut rx = self.event_bus.subscribe();
            loop {
                if let Some(RuntimeEvent::RuntimeShutdown) = rx.recv() {
                    break;
                }
            }
        }

        // Cleanup
        self.event_bus.emit(RuntimeEvent::RuntimeStop);
        for processor in &mut self.processors {
            processor.teardown()?;
        }

        Ok(())
    }

    /// Spawn processor thread with event handling
    fn spawn_processor_thread(&mut self, id: ProcessorId, processor: &mut dyn DynStreamProcessor) {
        let event_rx = self.event_bus.subscribe();
        let processor_ptr = processor as *mut dyn DynStreamProcessor;

        std::thread::spawn(move || {
            let processor = unsafe { &mut *processor_ptr };
            let mut event_rx = event_rx;
            let mut state = ProcessorState::Idle;

            loop {
                // Check events (non-blocking)
                while let Some(event) = event_rx.try_recv() {
                    match event {
                        RuntimeEvent::ProcessorStart { id: target }
                            if target.is_none() || target == Some(id) =>
                        {
                            state = ProcessorState::Running;
                        }
                        RuntimeEvent::ProcessorStop { id: target }
                            if target.is_none() || target == Some(id) =>
                        {
                            state = ProcessorState::Idle;
                        }
                        RuntimeEvent::RuntimeShutdown => {
                            return;
                        }
                        _ => {
                            // Forward to processor's on_event
                            if let Err(e) = processor.on_event(event) {
                                // Emit error event
                                // (need access to event_bus here)
                            }
                        }
                    }
                }

                // Process if running
                if state == ProcessorState::Running {
                    if let Err(e) = processor.process() {
                        state = ProcessorState::Error;
                        // Emit error event
                    }
                }

                // Small yield to prevent busy loop when paused
                if state != ProcessorState::Running {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        });
    }
}
```

### Processor Integration

#### Macro-Generated Code

```rust
// Generated by #[derive(StreamProcessor)]
impl StreamProcessorFactory for MyProcessor {
    fn from_config_with_bus(config: Self::Config, event_bus: &EventBus) -> Result<Self> {
        Ok(Self {
            // ... ports and config ...
            event_rx: event_bus.subscribe(),
            event_bus: event_bus.clone(),
        })
    }
}

// Generated trait impl
impl DynStreamProcessor for MyProcessor {
    fn on_event(&mut self, event: RuntimeEvent) -> Result<()> {
        // If user defined on_event, call it
        Self::on_event(self, event)
    }
}
```

#### User Code

```rust
#[derive(StreamProcessor)]
#[processor(
    mode = Pull,
    // Optional: filter events (only receive these)
    subscribe = [
        RuntimeEvent::ProcessorCommand,
        RuntimeEvent::KeyboardInput,
    ]
)]
pub struct ChordGeneratorProcessor {
    #[output]
    audio: Arc<StreamOutput<AudioFrame<2>>>,

    // Auto-injected by macro
    #[event_bus]
    event_bus: EventBus,

    #[event_rx]
    event_rx: EventReceiver,

    current_chord: Chord,
}

impl ChordGeneratorProcessor {
    // User-defined event handler
    fn on_event(&mut self, event: RuntimeEvent) -> Result<()> {
        match event {
            RuntimeEvent::ProcessorCommand { command, .. } => {
                match command {
                    ProcessorCommand::Custom { namespace, command, args }
                        if namespace == "chord_generator" =>
                    {
                        match command.as_str() {
                            "play_chord" => {
                                let chord: Chord = serde_json::from_value(args)?;
                                self.current_chord = chord;
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            RuntimeEvent::KeyboardInput { key, .. } => {
                // Map keyboard to chords
                match key {
                    KeyCode::C => self.current_chord = Chord::CMajor,
                    KeyCode::D => self.current_chord = Chord::DMajor,
                    KeyCode::G => self.current_chord = Chord::GMajor,
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        // Generate audio based on current_chord
        // ...
    }
}
```

### External Usage (addEventListener Pattern)

```rust
fn main() -> Result<()> {
    let mut runtime = StreamRuntime::new();

    // Add processors
    let chord_gen = runtime.add_processor::<ChordGeneratorProcessor>()?;
    let audio_out = runtime.add_processor::<AudioOutputProcessor>()?;

    // Connect pipeline
    runtime.connect(
        chord_gen.output_port("audio"),
        audio_out.input_port("audio"),
    )?;

    // Subscribe to events (like addEventListener)
    let event_rx = runtime.event_bus().subscribe();

    // Spawn event monitor thread
    std::thread::spawn(move || {
        let mut rx = event_rx;
        while let Some(event) = rx.recv() {
            match event {
                RuntimeEvent::ProcessorError { id, error } => {
                    eprintln!("Processor {} error: {}", id, error);
                }
                RuntimeEvent::ProcessorStateChanged { id, new_state, .. } => {
                    println!("Processor {} state: {:?}", id, new_state);
                }
                _ => {}
            }
        }
    });

    // Send keyboard commands from main thread
    std::thread::spawn({
        let event_bus = runtime.event_bus().clone();
        move || {
            loop {
                // Simulate keyboard input
                std::thread::sleep(std::time::Duration::from_secs(2));
                event_bus.emit(RuntimeEvent::KeyboardInput {
                    key: KeyCode::C,
                    modifiers: Modifiers::default(),
                    state: KeyState::Pressed,
                });
            }
        }
    });

    // Run runtime
    runtime.run()?;

    Ok(())
}
```

## Implementation Plan

### Phase 1: Core Event Bus (Week 1)

#### 1. Add Dependencies
**File**: `libs/streamlib/Cargo.toml`

```toml
[dependencies]
bus = "2.4"
serde_json = "1.0"
```

#### 2. Implement Event Types
**File**: `libs/streamlib/src/core/events.rs` (new)

- Define `RuntimeEvent` enum
- Define `ProcessorCommand` enum
- Define input event types (KeyCode, MouseButton, etc.)
- Add serialization support

#### 3. Implement EventBus
**File**: `libs/streamlib/src/core/event_bus.rs` (new)

- Implement `EventBus` wrapper around `bus::Bus`
- Implement `EventReceiver` wrapper
- Add thread-safe cloning
- Add try_emit for realtime paths

#### 4. Update RuntimeContext
**File**: `libs/streamlib/src/core/context.rs`

```rust
pub struct RuntimeContext {
    pub audio: AudioContext,
    pub video: VideoContext,
    pub gpu: Arc<GpuContext>,
    pub event_bus: EventBus,  // Add event bus
}
```

### Phase 2: Runtime Integration (Week 1-2)

#### 1. Update StreamRuntime
**File**: `libs/streamlib/src/core/runtime.rs`

- Add `event_bus: EventBus` field
- Update `add_processor` to pass event bus
- Update `run()` to emit lifecycle events
- Implement processor thread spawning with event handling

#### 2. Update DynStreamProcessor Trait
**File**: `libs/streamlib/src/core/processor.rs`

```rust
pub trait DynStreamProcessor: Send {
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        Ok(())
    }

    fn on_event(&mut self, event: RuntimeEvent) -> Result<()> {
        Ok(()) // Default: ignore events
    }

    fn process(&mut self) -> Result<()>;
}
```

### Phase 3: Macro Support (Week 2)

#### 1. Update Macro Attributes
**File**: `libs/streamlib-macros/src/attributes.rs`

Add `subscribe` attribute:

```rust
pub struct ProcessorAttributes {
    // ... existing fields ...
    pub subscribed_events: Vec<String>, // Event filter list
}
```

#### 2. Auto-Inject Event Bus Fields
**File**: `libs/streamlib-macros/src/codegen.rs`

Detect `#[event_bus]` and `#[event_rx]` attributes, auto-inject if missing:

```rust
// In struct definition:
#[event_bus]
event_bus: EventBus,

#[event_rx]
event_rx: EventReceiver,
```

#### 3. Generate on_event Wrapper
Generate code to call user's `on_event` if defined.

### Phase 4: Keyboard/Input Support (Week 2-3)

#### 1. Platform-Specific Input Handling

**macOS**: `libs/streamlib/src/apple/input.rs` (new)

```rust
use objc2_app_kit::{NSEvent, NSEventType};

pub fn setup_keyboard_handler(event_bus: EventBus) {
    NSEvent::addGlobalMonitorForEventsMatchingMask(mask, {
        let event_bus = event_bus.clone();
        move |event| {
            let key_code = event.keyCode();
            let modifiers = event.modifierFlags();

            event_bus.emit(RuntimeEvent::KeyboardInput {
                key: map_key_code(key_code),
                modifiers: map_modifiers(modifiers),
                state: KeyState::Pressed,
            });
        }
    });
}
```

**Linux/Windows**: TBD (use `winit` crate for cross-platform?)

#### 2. Integrate into Runtime Event Loop

Update `configure_macos_event_loop` to forward NSEvents to event bus.

### Phase 5: Update Processors (Week 3)

Update all processors to use event bus:

1. **ChordGeneratorProcessor**
   - Add keyboard support (C, D, G keys → chords)
   - Add custom commands (play_chord)

2. **AudioOutputProcessor**
   - Handle pause/resume events
   - Emit error events

3. **CameraProcessor**
   - Handle start/stop events
   - Emit frame capture events

4. **DisplayProcessor**
   - Handle window events
   - Handle vsync state changes

### Phase 6: Examples (Week 3)

#### 1. Keyboard Control Example
**File**: `examples/keyboard-audio/src/main.rs` (new)

```rust
fn main() -> Result<()> {
    let mut runtime = StreamRuntime::new();

    let chord = runtime.add_processor::<ChordGeneratorProcessor>()?;
    let output = runtime.add_processor::<AudioOutputProcessor>()?;

    runtime.connect(chord.output_port("audio"), output.input_port("audio"))?;

    println!("Press C, D, G keys to play chords!");
    println!("Press ESC to quit");

    runtime.run()?;
    Ok(())
}
```

#### 2. Network Control Example
**File**: `examples/network-control/src/main.rs` (new)

HTTP server that sends commands to runtime via event bus.

### Phase 7: Documentation (Week 4)

1. **Event Bus Guide**: `docs/guides/event-bus.md`
2. **Keyboard Input Guide**: `docs/guides/keyboard-input.md`
3. **Custom Commands Guide**: `docs/guides/custom-commands.md`
4. **API Documentation**: Update rustdoc

### Phase 8: Testing (Week 4)

#### 1. Unit Tests

```rust
#[test]
fn test_event_bus_broadcast() {
    let bus = EventBus::new(10);
    let mut rx1 = bus.subscribe();
    let mut rx2 = bus.subscribe();

    bus.emit(RuntimeEvent::RuntimeStart);

    assert_eq!(rx1.recv(), Some(RuntimeEvent::RuntimeStart));
    assert_eq!(rx2.recv(), Some(RuntimeEvent::RuntimeStart));
}

#[test]
fn test_processor_command() {
    // Test processor receives and handles commands
}
```

#### 2. Integration Tests

```rust
#[test]
fn test_keyboard_to_processor() {
    // Simulate keyboard event → processor receives command
}
```

## Performance Considerations

### Lock-Free Design
- `bus` crate is lock-free for readers
- Only locks on `add_rx()` (rare)
- Broadcast ~50-200ns depending on subscriber count

### Realtime Safety
- Use `try_emit()` in audio/video hot paths
- Use `try_recv()` in processor loops (non-blocking)
- Bounded channels prevent unbounded memory growth

### Event Filtering
- Processors subscribe only to relevant events
- Reduces message processing overhead
- Macro generates filtered receive loops

## Global Singleton (Optional Enhancement)

For convenience, we could provide a global runtime accessor:

```rust
// Global runtime instance (optional convenience)
static RUNTIME: OnceLock<Arc<Mutex<StreamRuntime>>> = OnceLock::new();

pub fn init_runtime() -> &'static Arc<Mutex<StreamRuntime>> {
    RUNTIME.get_or_init(|| Arc::new(Mutex::new(StreamRuntime::new())))
}

pub fn emit_event(event: RuntimeEvent) {
    if let Some(runtime) = RUNTIME.get() {
        runtime.lock().unwrap().event_bus().emit(event);
    }
}
```

**Trade-offs**:
- **Pro**: Easy to emit events from anywhere
- **Con**: Global mutable state, harder to test
- **Recommendation**: Only use for top-level applications, not libraries

## Migration Path

### For Existing Code
1. Processors automatically get event bus injected by macro
2. No breaking changes to existing processors
3. `on_event()` is optional (default: no-op)

### New Features Enabled
1. Ctrl+C shutdown (emit `RuntimeShutdown`)
2. Keyboard control
3. Network control
4. Error monitoring
5. State observability

## Alternatives Considered

### 1. flume instead of bus
**Rejected**: flume is MPMC (each message consumed once), not broadcast

### 2. tokio::sync::broadcast
**Rejected**: Requires async runtime (moving away from Tokio)

### 3. Custom lock-free broadcast
**Rejected**: `bus` crate is mature and well-tested

## Success Metrics

1. Keyboard input latency <10ms
2. Event broadcast overhead <100ns
3. Ctrl+C shutdown works for all processors
4. Zero allocations in realtime audio paths
5. All examples demonstrate event usage

## Related RFCs

- RFC 001: Setup/Teardown Lifecycle (defines processor state machine)

## Open Questions

1. Should we use `winit` for cross-platform input? (keyboard, mouse, gamepad)
2. Should global runtime singleton be provided?
3. Should we support event replay/recording for debugging?
4. Should we add rate limiting for high-frequency events?

## Implementation Task List

Use this checklist when implementing this RFC. Copy tasks to your todo tracker as you begin work.

### Phase 1: Core Event Bus
- [ ] Add dependencies to `libs/streamlib/Cargo.toml`
  - [ ] Add `bus = "2.4"`
  - [ ] Add `serde_json = "1.0"` (if not already present)
- [ ] Create event types (`libs/streamlib/src/core/events.rs`)
  - [ ] Define `RuntimeEvent` enum with all event variants
  - [ ] Define `ProcessorCommand` enum
  - [ ] Define `ProcessorState` enum
  - [ ] Define input types (KeyCode, Modifiers, KeyState, etc.)
  - [ ] Add serde Serialize/Deserialize derives
  - [ ] Add Clone, Debug derives
- [ ] Implement EventBus (`libs/streamlib/src/core/event_bus.rs`)
  - [ ] Create `EventBus` struct wrapping `bus::Bus`
  - [ ] Implement `new(capacity: usize)`
  - [ ] Implement `subscribe()` → `EventReceiver`
  - [ ] Implement `emit(event: RuntimeEvent)`
  - [ ] Implement `try_emit(event: RuntimeEvent)` (non-blocking)
  - [ ] Add thread-safe cloning (Arc-based)
- [ ] Implement EventReceiver (`libs/streamlib/src/core/event_bus.rs`)
  - [ ] Create `EventReceiver` struct wrapping `bus::BusReader`
  - [ ] Implement `recv()` (blocking)
  - [ ] Implement `try_recv()` (non-blocking)
- [ ] Update RuntimeContext (`libs/streamlib/src/core/context.rs`)
  - [ ] Add `event_bus: EventBus` field
  - [ ] Update constructor to initialize event bus
- [ ] Update module exports (`libs/streamlib/src/core/mod.rs`)
  - [ ] Export `events` module
  - [ ] Export `event_bus` module

### Phase 2: Runtime Integration
- [ ] Update StreamRuntime (`libs/streamlib/src/core/runtime.rs`)
  - [ ] Add `event_bus: EventBus` field to struct
  - [ ] Initialize event bus in `new()`
  - [ ] Add `event_bus()` getter method
  - [ ] Update `add_processor()` to pass event bus to processor
  - [ ] Emit `ProcessorSetup` event after setup
  - [ ] Update `run()` to emit `RuntimeStart`
  - [ ] Emit `ProcessorStart` event to start all processors
  - [ ] Implement processor thread spawning with event loop
  - [ ] Emit `RuntimeStop` on shutdown
  - [ ] Call `teardown()` on all processors during shutdown
- [ ] Update DynStreamProcessor trait (`libs/streamlib/src/core/processor.rs`)
  - [ ] Add `on_event(&mut self, event: RuntimeEvent) -> Result<()>` method
  - [ ] Provide default implementation (no-op)
- [ ] Update StreamProcessorFactory trait (`libs/streamlib/src/core/processor.rs`)
  - [ ] Add `from_config_with_bus(config, event_bus)` method
  - [ ] Keep backward compat with `from_config()`
- [ ] Update scheduler to handle processor state changes
  - [ ] Handle `ProcessorStart` event
  - [ ] Handle `ProcessorStop` event
  - [ ] Handle `ProcessorPause` event
  - [ ] Handle `ProcessorResume` event

### Phase 3: Macro Support
- [ ] Update attributes (`libs/streamlib-macros/src/attributes.rs`)
  - [ ] Add `subscribed_events` field to `ProcessorAttributes`
  - [ ] Parse `subscribe = [...]` attribute
- [ ] Update analysis (`libs/streamlib-macros/src/analysis.rs`)
  - [ ] Detect `#[event_bus]` attribute on fields
  - [ ] Detect `#[event_rx]` attribute on fields
  - [ ] Detect `on_event` method
- [ ] Update code generation (`libs/streamlib-macros/src/codegen.rs`)
  - [ ] Auto-inject `event_bus: EventBus` if `#[event_bus]` present
  - [ ] Auto-inject `event_rx: EventReceiver` if `#[event_rx]` present
  - [ ] Generate `from_config_with_bus()` implementation
  - [ ] Generate event filtering logic based on `subscribe` attribute
  - [ ] Generate `on_event()` wrapper to call user method
- [ ] Update macro documentation (`libs/streamlib-macros/src/lib.rs`)
  - [ ] Document `subscribe` attribute
  - [ ] Document `#[event_bus]` field attribute
  - [ ] Document `#[event_rx]` field attribute
  - [ ] Add examples showing event handling

### Phase 4: Keyboard/Input Support
- [ ] Create macOS input handler (`libs/streamlib/src/apple/input.rs`)
  - [ ] Implement `setup_keyboard_handler(event_bus: EventBus)`
  - [ ] Use NSEvent global monitor for keyboard events
  - [ ] Map NSEvent key codes to KeyCode enum
  - [ ] Map NSEvent modifiers to Modifiers struct
  - [ ] Emit `KeyboardInput` events
- [ ] Integrate into macOS runtime (`libs/streamlib/src/apple/runtime_ext.rs`)
  - [ ] Update `configure_macos_event_loop()` to setup keyboard handler
  - [ ] Forward NSEvents to event bus
- [ ] Add cross-platform input (optional, future work)
  - [ ] Research `winit` crate integration
  - [ ] Add Linux keyboard support
  - [ ] Add Windows keyboard support

### Phase 5: Update Processors
- [ ] Update ChordGeneratorProcessor (`libs/streamlib/src/core/processors/chord_generator.rs`)
  - [ ] Add `#[event_bus]` and `#[event_rx]` fields
  - [ ] Implement `on_event()` method
  - [ ] Handle `KeyboardInput` events (C, D, G keys)
  - [ ] Handle `ProcessorCommand::Custom` for "play_chord"
  - [ ] Handle `RuntimeShutdown` to stop thread
  - [ ] Test keyboard control
- [ ] Update AudioOutputProcessor (`libs/streamlib/src/apple/processors/audio_output.rs`)
  - [ ] Add event bus fields
  - [ ] Implement `on_event()` method
  - [ ] Handle `ProcessorPause`/`ProcessorResume` events
  - [ ] Emit `ProcessorError` on failures
  - [ ] Test pause/resume functionality
- [ ] Update CameraProcessor (`libs/streamlib/src/apple/processors/camera.rs`)
  - [ ] Add event bus fields
  - [ ] Implement `on_event()` method
  - [ ] Handle `ProcessorStart`/`ProcessorStop` for capture control
  - [ ] Emit status events
  - [ ] Test start/stop via events
- [ ] Update DisplayProcessor (`libs/streamlib/src/apple/processors/display.rs`)
  - [ ] Add event bus fields
  - [ ] Implement `on_event()` method
  - [ ] Handle window events
  - [ ] Handle `RuntimeShutdown` to exit vsync loop
  - [ ] Test clean shutdown

### Phase 6: Examples
- [ ] Create keyboard-audio example (`examples/keyboard-audio/`)
  - [ ] Create `Cargo.toml`
  - [ ] Create `src/main.rs` with keyboard-controlled chord generator
  - [ ] Test C, D, G keys play different chords
  - [ ] Test ESC key exits cleanly
  - [ ] Add README with usage instructions
- [ ] Create network-control example (`examples/network-control/`)
  - [ ] Create HTTP server that accepts commands
  - [ ] Forward HTTP requests to event bus as commands
  - [ ] Demonstrate remote processor control
  - [ ] Add README with API documentation
- [ ] Update existing examples
  - [ ] Add event monitoring to camera-display
  - [ ] Add event monitoring to audio-mixer-demo
  - [ ] Show how to subscribe to events

### Phase 7: Documentation
- [ ] Write event bus guide (`docs/guides/event-bus.md`)
  - [ ] Explain event bus architecture
  - [ ] Show how to subscribe to events
  - [ ] Show how to emit events
  - [ ] Explain event filtering
  - [ ] Document performance characteristics
- [ ] Write keyboard input guide (`docs/guides/keyboard-input.md`)
  - [ ] Show how to handle keyboard events
  - [ ] Document KeyCode enum
  - [ ] Show modifier key handling
  - [ ] Platform-specific notes
- [ ] Write custom commands guide (`docs/guides/custom-commands.md`)
  - [ ] Explain `ProcessorCommand::Custom`
  - [ ] Show namespace conventions
  - [ ] Provide examples for different processor types
  - [ ] Show JSON argument encoding
- [ ] Update API documentation
  - [ ] Add rustdoc for all event types
  - [ ] Add rustdoc for EventBus methods
  - [ ] Add examples to rustdoc

### Phase 8: Testing
- [ ] Write event bus unit tests (`libs/streamlib/src/core/event_bus.rs`)
  - [ ] Test broadcast to multiple subscribers
  - [ ] Test `try_emit()` non-blocking behavior
  - [ ] Test `try_recv()` non-blocking behavior
  - [ ] Test event ordering
- [ ] Write processor event tests (`libs/streamlib/tests/processor_events_test.rs`)
  - [ ] Test processor receives commands
  - [ ] Test processor state changes
  - [ ] Test error event propagation
- [ ] Write keyboard input tests (`libs/streamlib/tests/keyboard_test.rs`)
  - [ ] Simulate keyboard events
  - [ ] Verify processor receives events
  - [ ] Test modifier key combinations
- [ ] Write integration tests (`libs/streamlib/tests/event_integration_test.rs`)
  - [ ] Test full pipeline with events
  - [ ] Test runtime shutdown via event
  - [ ] Test pause/resume
- [ ] Performance benchmarks (`benches/event_bus_bench.rs`)
  - [ ] Measure broadcast latency
  - [ ] Measure event throughput
  - [ ] Verify <100ns overhead
- [ ] Run all examples as smoke tests
  - [ ] Test keyboard-audio example
  - [ ] Test network-control example
  - [ ] Test camera-display with events
  - [ ] Test audio-mixer-demo with events

### Phase 9: Optional Enhancements
- [ ] Consider global runtime singleton
  - [ ] Design API for global access
  - [ ] Document trade-offs
  - [ ] Implement if beneficial
- [ ] Consider event replay/recording
  - [ ] Design event log format
  - [ ] Implement record/playback
  - [ ] Add debugging tools
- [ ] Consider rate limiting
  - [ ] Identify high-frequency event sources
  - [ ] Implement rate limiter
  - [ ] Add configuration options

### Final Checks
- [ ] All unit tests passing (`cargo test`)
- [ ] All integration tests passing
- [ ] All benchmarks showing <100ns latency
- [ ] All examples working
- [ ] Keyboard input working on target platforms
- [ ] Ctrl+C shutdown working for all processors
- [ ] Documentation complete
- [ ] Code review ready
- [ ] Ready to merge
