# Pull Mode Pattern - Complete Implementation

## Overview

Pull mode allows hardware callbacks (CoreAudio, vsync, etc.) to drive processor execution by calling `process()` directly at hardware rate. The `audio_utils::setup_audio_output()` utility abstracts away all cpal/CoreAudio boilerplate.

## Architecture Components

### 1. MediaClock - Global System Utility

```rust
// Available everywhere, no initialization needed
let timestamp = MediaClock::now();  // Duration since boot, same timebase as CoreAudio
```

### 2. SchedulingMode::Pull

```rust
fn scheduling_config(&self) -> SchedulingConfig {
    SchedulingConfig {
        mode: SchedulingMode::Pull,  // Hardware callback drives execution
        priority: ThreadPriority::RealTime,
        clock: ClockSource::Audio,
        provide_clock: true,
    }
}
```

Runtime behavior:
- Spawns a thread that just waits for shutdown
- Does NOT call `process()` - processor manages that itself

### 3. audio_utils::setup_audio_output() - Boilerplate Abstraction

```rust
pub fn setup_audio_output<F>(
    device_id: Option<usize>,
    callback: F,
) -> Result<AudioOutputSetup>
where
    F: FnMut(&mut [f32], &cpal::OutputCallbackInfo) + Send + 'static
```

Handles:
- Host initialization (`cpal::default_host()`)
- Device selection (default or by ID)
- Config parsing (sample rate, channels)
- Stream creation with your callback
- Returns `AudioOutputSetup` with stream, device info, etc.

## Complete AudioOutput Example

```rust
pub struct AudioOutputProcessor {
    device_id: Option<usize>,
    stream: Option<Stream>,
    input: StreamInput<AudioFrame>,
    ring_buffer: Arc<Mutex<rtrb::Consumer<f32>>>,
    // ... other fields
}

impl StreamElement for AudioOutputProcessor {
    fn start(&mut self, ctx: &RuntimeContext) -> Result<()> {
        // Create ring buffer
        let (mut producer, consumer) = rtrb::RingBuffer::new(8192).split();
        self.ring_buffer_producer = Some(producer);

        let consumer_arc = Arc::new(Mutex::new(consumer));
        let consumer_for_callback = Arc::clone(&consumer_arc);

        // Need Arc<Mutex<Self>> to call process() from callback
        let processor_arc = /* Arc to self */;

        // Use utility - handles ALL cpal boilerplate
        let setup = crate::apple::audio_utils::setup_audio_output(
            self.device_id,
            move |data: &mut [f32], _info| {
                // Hardware callback on CoreAudio RT thread

                // 1. Call process() to pull from input port
                {
                    let mut proc = processor_arc.lock();
                    let _ = proc.process();  // Pulls from input → ring buffer
                }

                // 2. Fill hardware buffer from ring buffer
                let mut consumer = consumer_for_callback.lock();
                for sample in data.iter_mut() {
                    *sample = consumer.pop().unwrap_or(0.0);
                }
            }
        )?;

        // Start playback
        setup.stream.play()?;
        self.stream = Some(setup.stream);
        self.sample_rate = setup.sample_rate;
        self.channels = setup.channels;

        tracing::info!("AudioOutput started: {} ({}Hz, {} ch)",
            setup.device_info.name, setup.sample_rate, setup.channels);

        Ok(())
    }
}

impl StreamProcessor for AudioOutputProcessor {
    fn process(&mut self) -> Result<()> {
        // Called from hardware callback
        if let Some(frame) = self.input.read_latest() {
            // Push samples to ring buffer
            if let Some(producer) = &mut self.ring_buffer_producer {
                for sample in frame.samples.iter() {
                    let _ = producer.push(*sample);  // Drop on full (real-time)
                }
            }
        }
        Ok(())
    }

    fn scheduling_config(&self) -> SchedulingConfig {
        SchedulingConfig {
            mode: SchedulingMode::Pull,
            priority: ThreadPriority::RealTime,
            clock: ClockSource::Audio,
            provide_clock: true,
        }
    }
}
```

## Data Flow

```
Hardware Callback (CoreAudio RT thread):
    ├─ Call processor.process()
    │   └─ Read from input port (upstream processor's output)
    │   └─ Push samples to ring buffer
    │
    └─ Pull samples from ring buffer
        └─ Fill hardware output buffer
```

## Benefits of This Pattern

1. **Abstracted boilerplate**: `audio_utils::setup_audio_output()` hides all cpal complexity
2. **Hardware-driven timing**: CoreAudio controls when `process()` is called
3. **Same timebase**: `MediaClock::now()` uses mach_absolute_time like CoreAudio
4. **Real-time safe**: Ring buffer decouples callback timing from upstream
5. **Drop frames gracefully**: If ring buffer empty, output silence (no blocking)

## Usage in Other Processors

Any processor can use MediaClock for timestamps:

```rust
// In TestToneGenerator, AudioMixer, ClapEffect, etc.
fn process(&mut self) -> Result<()> {
    let timestamp = MediaClock::now().as_nanos() as i64;
    let frame = AudioFrame::new(samples, timestamp, frame_num, channels);
    self.output.write(frame);
    Ok(())
}
```

All timestamps synchronized to the same hardware timebase!

## Comparison to Old Pattern

### Before (Old Pattern)
```rust
// Constructor setup - can't access self in callback
let stream = device.build_output_stream(..., move |data, info| {
    // Just pull from shared Vec buffer
    data.copy_from_slice(&buffer[..]);
})?;

// Runtime calls process() → pushes to Vec
fn process(&mut self) {
    self.buffer.push(samples);
}
```

### After (Pull Mode Pattern)
```rust
// start() setup - has access to self
let setup = setup_audio_output(device_id, move |data, info| {
    // Call process() from callback
    processor.lock().process().ok();
    // Pull from ring buffer
});

// Callback calls process() → pulls from input port
fn process(&mut self) {
    let frame = self.input.read_latest();
    self.ring_buffer.push(frame.samples);
}
```

## Key Difference

**Old**: Runtime drives `process()`, callback is passive
**New**: Callback drives `process()`, hardware controls timing

This is true Pull mode - hardware pulls data when it needs it!
