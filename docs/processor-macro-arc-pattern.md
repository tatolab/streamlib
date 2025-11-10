# Arc-Wrapped Ports in StreamProcessor Macro

## When to Use Arc-Wrapped Ports

The `#[derive(StreamProcessor)]` macro automatically detects and supports Arc-wrapped ports for processors that need to share port ownership across threads.

### Use Arc When:

✅ Your processor spawns **self-managed threads** that need direct write access to output ports
✅ Your processor uses **thread pools** where multiple threads write to the same port
✅ Your processor implements **custom concurrency patterns** requiring shared ownership

### Don't Use Arc When:

❌ Your processor is **callback-driven** (e.g., CoreAudio, AVFoundation callbacks)
❌ Your processor is **runtime-scheduled** (Push/Pull mode without custom threads)
❌ You're unsure - start without Arc and add it only if needed

## Pattern Comparison

### Without Arc (Most Processors)

Platform callbacks or runtime-driven processing - no custom thread management:

```rust
use streamlib_macros::StreamProcessor;

#[derive(StreamProcessor)]
pub struct CameraProcessor {
    #[output]
    video: StreamOutput<VideoFrame>,  // Regular port

    // Platform callback owns the data, we just pass it through
}

impl StreamProcessor for CameraProcessor {
    fn process(&mut self) -> Result<()> {
        // Called by runtime or platform callback
        // No custom threads spawned
        Ok(())
    }
}
```

### With Arc (Self-Managed Threads)

Processor spawns its own threads that need to write directly to ports:

```rust
use streamlib_macros::StreamProcessor;
use std::sync::Arc;

#[derive(StreamProcessor)]
pub struct ChordGeneratorProcessor {
    #[output]
    chord: Arc<StreamOutput<AudioFrame<2>>>,  // Arc-wrapped for thread sharing!

    // Config fields
    running: Arc<AtomicBool>,
    loop_handle: Option<std::thread::JoinHandle<()>>,
}

impl ChordGeneratorProcessor {
    pub fn new() -> Self {
        Self {
            chord: Arc::new(StreamOutput::new("chord")),  // User creates Arc
            running: Arc::new(AtomicBool::new(false)),
            loop_handle: None,
        }
    }
}

impl StreamProcessor for ChordGeneratorProcessor {
    fn process(&mut self) -> Result<()> {
        let chord_output = Arc::clone(&self.chord);  // Clone Arc for thread
        let running = Arc::clone(&self.running);

        let handle = std::thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                // Thread owns cloned Arc, can write directly
                chord_output.write(frame);
            }
        });

        self.loop_handle = Some(handle);
        Ok(())
    }
}
```

## How Arc Detection Works

The macro automatically detects the `Arc<StreamOutput<T>>` pattern:

1. **Type Analysis**: Macro parses field type to detect `Arc<StreamOutput<MessageType>>`
2. **Message Type Extraction**: Extracts `MessageType` from inside the Arc wrapper
3. **View Struct Generation**: Generates `&'a Arc<StreamOutput<T>>` reference (not `&'a mut T`)
4. **Port Methods**: Generated helpers work transparently with Arc due to `Deref` trait

### What the Macro Generates

```rust
// For Arc-wrapped port:
#[output]
chord: Arc<StreamOutput<AudioFrame<2>>>,

// Macro generates:
pub struct ChordGeneratorProcessorPorts<'a> {
    pub chord: &'a Arc<StreamOutput<AudioFrame<2>>>,  // Ref to Arc!
}

impl ChordGeneratorProcessor {
    // Helper method for port type lookup
    fn get_output_port_type_impl(&self, port_name: &str) -> Option<PortType> {
        match port_name {
            "chord" => Some(PortType::Audio2),
            _ => None,
        }
    }

    // Helper method for connection wiring
    fn wire_output_connection_impl(&mut self, port_name: &str, connection: Arc<dyn Any>) -> bool {
        if port_name == "chord" {
            self.chord.add_connection(connection);  // Deref makes Arc transparent
            return true;
        }
        false
    }

    // Convenience method for port access
    pub fn ports(&self) -> ChordGeneratorProcessorPorts {
        ChordGeneratorProcessorPorts {
            chord: &self.chord,  // Borrow Arc reference
        }
    }
}
```

## Real-World Example: ChordGenerator

ChordGenerator needs Arc because it spawns an audio generation thread that runs independently:

```rust
#[derive(StreamProcessor)]
pub struct ChordGeneratorProcessor {
    #[output]
    chord: Arc<StreamOutput<AudioFrame<2>>>,  // Arc for thread sharing

    osc_c4: Arc<Mutex<SineOscillator>>,
    osc_e4: Arc<Mutex<SineOscillator>>,
    osc_g4: Arc<Mutex<SineOscillator>>,
    running: Arc<AtomicBool>,
    loop_handle: Option<std::thread::JoinHandle<()>>,
}

impl StreamProcessor for ChordGeneratorProcessor {
    fn process(&mut self) -> Result<()> {
        self.running.store(true, Ordering::Relaxed);

        // Clone Arc for thread ownership
        let chord_output = Arc::clone(&self.chord);
        let osc_c4 = Arc::clone(&self.osc_c4);
        let osc_e4 = Arc::clone(&self.osc_e4);
        let osc_g4 = Arc::clone(&self.osc_g4);
        let running = Arc::clone(&self.running);

        // Spawn independent audio generation thread
        let handle = std::thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                // Generate samples
                let c4 = osc_c4.lock().unwrap().next();
                let e4 = osc_e4.lock().unwrap().next();
                let g4 = osc_g4.lock().unwrap().next();

                let mixed = c4 + e4 + g4;
                let frame = AudioFrame::new(vec![mixed, mixed], timestamp, counter);

                // Thread writes directly to port
                chord_output.write(frame);
            }
        });

        self.loop_handle = Some(handle);
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.loop_handle.take() {
            handle.join().ok();
        }
        Ok(())
    }
}
```

### Why ChordGenerator Needs Arc

- **Self-Managed Thread**: ChordGenerator spawns its own audio generation loop
- **Independent Timing**: Thread runs at audio sample rate (48kHz), not runtime tick rate
- **Shared Ownership**: Both main struct and spawned thread need access to output port
- **Thread-Safe Writing**: Spawned thread calls `chord_output.write()` directly

### Counter-Example: Camera (No Arc Needed)

Camera uses platform callbacks, doesn't spawn threads:

```rust
#[derive(StreamProcessor)]
pub struct CameraProcessor {
    #[output]
    video: StreamOutput<VideoFrame>,  // No Arc needed!

    capture_session: Option<AVCaptureSession>,
}

impl StreamProcessor for CameraProcessor {
    fn process(&mut self) -> Result<()> {
        // Platform (AVFoundation) calls our delegate
        // We write to port from delegate callback
        // No custom threads spawned by us
        Ok(())
    }
}

// Delegate callback (called by AVFoundation thread):
fn did_output_sample_buffer(&mut self, buffer: CMSampleBuffer) {
    let frame = convert_to_video_frame(buffer);
    self.video.write(frame);  // Write from platform callback
}
```

## Initialization Pattern

When using Arc-wrapped ports, you initialize them in `new()` or `from_config()`:

```rust
impl StreamProcessor for ChordGeneratorProcessor {
    type Config = ChordGeneratorConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        Ok(Self {
            // User creates Arc wrapper
            chord: Arc::new(StreamOutput::new("chord")),

            // Other Arc-wrapped config fields
            running: Arc::new(AtomicBool::new(false)),
            loop_handle: None,
        })
    }
}
```

The macro does **NOT** generate initialization code - you control how ports are created.

## Arc Transparency

Thanks to Rust's `Deref` trait, Arc-wrapped ports work transparently in most contexts:

```rust
// These all work the same with or without Arc:
self.chord.write(frame);                    // Write data
self.chord.add_connection(connection);      // Add connection
self.chord.set_downstream_wakeup(tx);       // Set wakeup channel

// Arc difference only matters for:
let cloned = Arc::clone(&self.chord);       // Clone for threads (Arc only)
let reference = &self.chord;                // &Arc<T> vs &mut T
```

## Migration Guide

### Adding Arc to Existing Processor

If you discover your processor needs Arc:

1. **Wrap the port type**:
```rust
// Before:
#[output]
video: StreamOutput<VideoFrame>,

// After:
#[output]
video: Arc<StreamOutput<VideoFrame>>,
```

2. **Update initialization**:
```rust
// Before:
Self {
    video: StreamOutput::new("video"),
}

// After:
Self {
    video: Arc::new(StreamOutput::new("video")),
}
```

3. **Clone Arc for threads**:
```rust
let video_output = Arc::clone(&self.video);
std::thread::spawn(move || {
    video_output.write(frame);
});
```

That's it! The macro handles the rest automatically.

## Best Practices

### ✅ Do

- Use Arc only when you actually spawn threads that need port access
- Initialize Arc in `new()` or `from_config()` - macro doesn't do this
- Clone Arc before moving into spawned threads
- Keep Arc clones alive as long as threads run
- Use `stop()` to join threads and clean up

### ❌ Don't

- Don't use Arc for callback-driven processors (Camera, AudioOutput, etc.)
- Don't add Arc "just in case" - adds overhead and complexity
- Don't forget to join threads in `stop()` - prevents resource leaks
- Don't assume macro creates Arc wrapper - you must wrap in initialization

## Performance Considerations

### Arc Overhead

- **Reference Counting**: Atomic increment/decrement on clone/drop
- **Indirection**: Extra pointer dereference (minimal cost due to Deref)
- **Cache**: Arc allocates on heap, but StreamOutput already does this

### When It Matters

- Arc overhead is **negligible** compared to audio processing or video capture
- Thread synchronization (Mutex, AtomicBool) dominates performance impact
- Only matters if you're cloning Arc in hot loops (don't do this!)

### Optimization Tips

```rust
// ✅ Good: Clone once before spawning thread
let output = Arc::clone(&self.chord);
std::thread::spawn(move || {
    loop {
        output.write(frame);  // No cloning in loop
    }
});

// ❌ Bad: Cloning in hot loop
std::thread::spawn(move || {
    loop {
        let output = Arc::clone(&self.chord);  // Unnecessary overhead!
        output.write(frame);
    }
});
```

## Troubleshooting

### "Cannot move out of borrowed content"

You need Arc to share ownership across threads:

```rust
// Error:
std::thread::spawn(move || {
    self.video.write(frame);  // Can't move self into closure
});

// Fix:
let video = Arc::clone(&self.video);
std::thread::spawn(move || {
    video.write(frame);  // Arc allows shared ownership
});
```

### "Macro doesn't detect Arc"

Make sure you're using the exact pattern:

```rust
// ✅ Correct: Macro detects this
#[output]
video: Arc<StreamOutput<VideoFrame>>,

// ❌ Wrong: Type alias not detected
type ArcOutput = Arc<StreamOutput<VideoFrame>>;
#[output]
video: ArcOutput,

// ❌ Wrong: Nested Arc not supported
#[output]
video: Arc<Arc<StreamOutput<VideoFrame>>>,
```

### "Expected &mut T, found &Arc<T>"

The view struct returns `&Arc<T>` for Arc-wrapped ports:

```rust
let ports = self.ports();
// ports.video is &Arc<StreamOutput<T>>, not &mut StreamOutput<T>

// ✅ This works (Deref makes it transparent):
ports.video.write(frame);

// ❌ This doesn't work (can't get &mut from &Arc):
let mutable_ref: &mut StreamOutput<_> = ports.video;  // Compile error
```

## Summary

- Use Arc for **self-managed threads** that need shared port ownership
- Macro **auto-detects** `Arc<StreamOutput<T>>` pattern from type signature
- Macro generates appropriate **view structs** with `&Arc<T>` references
- Arc is **transparent** for most operations due to Deref trait
- **You initialize** Arc wrapper - macro doesn't do this for you
- Don't use Arc for **callback-driven** or **runtime-scheduled** processors
