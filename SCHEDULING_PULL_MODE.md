# SchedulingMode::Pull - Hardware Callback-Driven Execution

## Overview

`SchedulingMode::Pull` allows processors to be driven entirely by hardware callbacks. The runtime does **NOT** spawn a thread or drive execution - instead, the processor's hardware callback directly calls `process()` to pull data from input ports.

## Use Cases

- **Audio Output**: CoreAudio render callback pulls samples from ring buffer
- **Video Display**: Vsync callback pulls frames for display
- **Hardware sinks**: Any processor where hardware controls the timing

## How It Works

### 1. Processor declares Pull mode

```rust
impl StreamProcessor for AudioOutputProcessor {
    fn scheduling_config(&self) -> SchedulingConfig {
        SchedulingConfig {
            mode: SchedulingMode::Pull,  // Hardware callback drives execution
            priority: ThreadPriority::RealTime,
            clock: ClockSource::Audio,
            provide_clock: true,
        }
    }
}
```

### 2. Runtime behavior

When the runtime sees `SchedulingMode::Pull`:
- It **spawns a thread** but that thread just waits for shutdown
- It does **NOT** call `process()` - the processor manages that itself
- The processor's `start()` method sets up the hardware callback

### 3. Processor implements hardware callback

```rust
impl AudioOutputProcessor {
    fn start(&mut self, ctx: &RuntimeContext) -> Result<()> {
        // Get reference to input port consumer (ring buffer)
        let consumer = Arc::clone(&self.input_consumer);

        // Clone Arc to processor for callback
        let processor_for_callback = Arc::clone(&self.processor_arc);

        // Build hardware callback
        let stream = device.build_output_stream(
            &config.into(),
            move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                // Hardware callback - running on RT thread
                let timestamp = MediaClock::now();

                // Call process() directly from callback
                {
                    let mut proc = processor_for_callback.lock();
                    if let Err(e) = proc.process() {
                        tracing::error!("Process error in callback: {}", e);
                    }
                }

                // Pull samples from ring buffer
                for sample in data.iter_mut() {
                    *sample = consumer.lock().pop().unwrap_or(0.0);
                }
            },
            |err| eprintln!("Stream error: {}", err),
            None,
        )?;

        stream.play()?;
        self.stream = Some(stream);
        Ok(())
    }
}
```

### 4. The process() method just updates state

```rust
impl StreamProcessor for AudioOutputProcessor {
    fn process(&mut self) -> Result<()> {
        // Called from hardware callback (CoreAudio RT thread)

        // Read from input port and push to ring buffer
        if let Some(frame) = self.input.read_latest() {
            // Deinterleave and push samples
            for sample in frame.samples.iter() {
                // Push with overwrite on full (real-time priority)
                let _ = self.ring_buffer_producer.push(*sample);
            }
        }

        Ok(())
    }
}
```

## Complete Example Pattern

```rust
pub struct AudioOutputProcessor {
    // Input port (connected to upstream processor)
    input: StreamInput<AudioFrame>,

    // Ring buffer producer (writes to buffer)
    ring_buffer_producer: rtrb::Producer<f32>,

    // Ring buffer consumer (reads in callback) - Arc for sharing
    ring_buffer_consumer: Arc<Mutex<rtrb::Consumer<f32>>>,

    // Hardware stream
    stream: Option<cpal::Stream>,

    // Self-reference for callback
    processor_arc: Arc<Mutex<Self>>,
}

impl StreamElement for AudioOutputProcessor {
    fn start(&mut self, ctx: &RuntimeContext) -> Result<()> {
        // Create ring buffer
        let (producer, consumer) = rtrb::RingBuffer::new(8192).split();
        self.ring_buffer_producer = producer;
        self.ring_buffer_consumer = Arc::new(Mutex::new(consumer));

        // Clone for callback
        let consumer_for_callback = Arc::clone(&self.ring_buffer_consumer);
        let processor_for_callback = Arc::clone(&self.processor_arc);

        // Build hardware stream
        let stream = device.build_output_stream(
            &config.into(),
            move |data: &mut [f32], _| {
                // Pull from upstream by calling process()
                {
                    let mut proc = processor_for_callback.lock();
                    let _ = proc.process();  // Fills ring buffer
                }

                // Pull samples at hardware rate
                let mut consumer = consumer_for_callback.lock();
                for sample in data.iter_mut() {
                    *sample = consumer.pop().unwrap_or(0.0);
                }
            },
            |err| eprintln!("Stream error: {}", err),
            None,
        )?;

        stream.play()?;
        self.stream = Some(stream);
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(stream) = self.stream.take() {
            drop(stream);  // Stop playback
        }
        Ok(())
    }
}

impl StreamProcessor for AudioOutputProcessor {
    fn process(&mut self) -> Result<()> {
        // Called from hardware callback
        if let Some(frame) = self.input.read_latest() {
            for sample in frame.samples.iter() {
                let _ = self.ring_buffer_producer.push(*sample);
            }
        }
        Ok(())
    }

    fn scheduling_config(&self) -> SchedulingConfig {
        SchedulingConfig {
            mode: SchedulingMode::Pull,  // Hardware-driven
            priority: ThreadPriority::RealTime,
            clock: ClockSource::Audio,
            provide_clock: true,
        }
    }
}
```

## Key Points

1. **Runtime spawns a "dummy" thread** that just waits for shutdown
2. **Processor manages its own callback** via hardware API (CoreAudio, vsync, etc.)
3. **Callback calls process() directly** on hardware RT thread
4. **Ring buffer decouples** callback timing from upstream processors
5. **MediaClock::now()** provides synchronized timestamps
6. **No backpressure** - if ring buffer is empty, output silence (real-time priority)

## Thread Model

```
Runtime Thread (spawned by runtime):
  └─ Just waits for shutdown signal

Hardware Callback Thread (managed by OS):
  ├─ Calls process() → pulls from input port → fills ring buffer
  └─ Reads from ring buffer → outputs to hardware
```

## Comparison to Other Modes

| Mode | Who spawns thread? | Who calls process()? | Use case |
|------|-------------------|---------------------|----------|
| **Loop** | Runtime | Runtime (continuous loop) | Test tone generator |
| **Reactive** | Runtime | Runtime (on data arrival) | Video effects |
| **Callback** | Hardware | Hardware callback | Camera capture |
| **Pull** | Runtime (dummy) | Processor's callback | Audio/video output |
| **Timer** | Runtime | Runtime (periodic) | Metrics |

## Benefits

- **Zero-copy possible**: Hardware callback can read directly from shared memory
- **Lowest latency**: Direct path from hardware to processing
- **Real-time guarantees**: Runs on hardware RT thread
- **Hardware-synchronized**: Timing controlled by audio clock, vsync, etc.
