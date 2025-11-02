# Threading Model for Streamlib

## Overview

Streamlib uses a **hybrid threading model** optimized for low-latency, high-throughput media processing:

1. **Native real-time threads** - Audio I/O (via cpal/CoreAudio)
2. **High-priority threads** - Camera/display (via AVFoundation/Metal)
3. **Custom priority threads** - User processors (via spawn_priority_thread)
4. **Async runtime** - Coordination (via tokio)

## Thread Priority Levels

### Audio (Highest - Time Constraint)
- **Latency**: < 5ms (typically 2-3ms)
- **Implementation**: `cpal` → CoreAudio → `THREAD_TIME_CONSTRAINT_POLICY`
- **Characteristics**:
  - Non-preemptible
  - Time-constrained scheduling
  - Buffer underrun = audio glitches
  - **Already handled by cpal - don't touch!**

### Video Capture (High Priority)
- **Latency**: < 16.67ms @ 60fps (< 8.33ms @ 120fps)
- **Implementation**: AVFoundation native queues
- **Characteristics**:
  - Hardware-synchronized
  - Priority above normal processing
  - **Already handled by AVFoundation - don't touch!**

### Video Render (High Priority)
- **Latency**: < 16.67ms @ 60fps
- **Implementation**: Metal/CADisplayLink
- **Characteristics**:
  - Display link synchronization
  - GPU command submission
  - **Already handled by Metal/winit - don't touch!**

### Processing (Normal-High Priority)
- **Latency**: < 33ms (acceptable for most effects)
- **Implementation**: `spawn_priority_thread()` (custom)
- **Use cases**:
  - Video effects
  - ML inference
  - Custom transforms
  - **This is where you add custom priority threads**

### Background (Normal Priority)
- **Latency**: Not critical
- **Implementation**: `std::thread::spawn()` or `tokio::spawn()`
- **Use cases**:
  - File I/O
  - Network streams
  - Logging
  - Configuration

## Lock-Free Communication

All high-priority threads communicate via **lock-free data structures**:

### Audio/Video Data Flow
```
Camera (AVFoundation) ──[rtrb]──> Effect Processor ──[rtrb]──> Display (Metal)
                          ↑                              ↑
                    Lock-free SPSC          Lock-free SPSC
                    Real-time safe          Real-time safe
```

### Why Lock-Free?
- ❌ `Mutex::lock()` can cause **priority inversion**
  - High-priority thread blocks on low-priority thread
  - Audio glitches, video stutter

- ✅ `rtrb` is **wait-free** for single producer/consumer
  - No blocking
  - No priority inversion
  - Bounded latency

## Memory Allocation

### Pre-allocate Everything
```rust
// ❌ BAD: Allocates in hot path
fn process(&mut self, frame: VideoFrame) {
    let mut buffer = Vec::new();  // ALLOCATION!
    buffer.push(frame);           // REALLOCATION!
}

// ✅ GOOD: Pre-allocated
fn process(&mut self, frame: VideoFrame) {
    self.buffer.clear();  // No allocation
    self.buffer.push(frame);
}
```

### Use Object Pools for GPU Resources
```rust
// Pre-allocate GPU buffers
let buffer_pool = BufferPool::new(device, 10);

// Rent from pool (no allocation)
let buffer = buffer_pool.rent()?;
```

## Platform-Specific Notes

### macOS/iOS
- ✅ Use GCD for main thread operations
- ✅ Use QoS hints for P-core vs E-core scheduling
- ✅ Time-constraint policy for < 10ms latency
- ✅ Precedence policy for < 33ms latency

### Linux (Future)
- Use `SCHED_FIFO` or `SCHED_RR` (requires `CAP_SYS_NICE`)
- Use `chrt` for testing
- Consider `SCHED_DEADLINE` for hard real-time

### Windows (Future)
- Use `SetThreadPriority(THREAD_PRIORITY_TIME_CRITICAL)`
- Use multimedia class scheduler (MMCSS)

## Example: Custom ML Processor

```rust
use streamlib::threading::{spawn_priority_thread, ThreadPriority};

struct MLProcessor {
    input: StreamInput<VideoFrame>,
    output: StreamOutput<DetectionResults>,
    thread: Option<JoinHandle<()>>,
}

impl MLProcessor {
    fn start(&mut self) -> Result<()> {
        let input = self.input.clone();
        let output = self.output.clone();

        // Spawn high-priority processing thread
        self.thread = Some(spawn_priority_thread(
            "ml-inference",
            ThreadPriority::Processing,  // Normal-high priority
            move || {
                // Pre-allocate buffers
                let mut input_buffer = Vec::with_capacity(1920*1080*4);

                loop {
                    // Lock-free read (wait-free)
                    if let Some(frame) = input.read_latest() {
                        // Process (no allocations)
                        let results = model.inference(&frame);

                        // Lock-free write (wait-free)
                        output.write(results);
                    }
                }
            }
        )?);

        Ok(())
    }
}
```

## Performance Tips

1. **Measure, don't guess**
   ```rust
   let start = Instant::now();
   process_frame(frame);
   println!("Frame time: {:?}", start.elapsed());
   ```

2. **Use instruments/perf**
   - macOS: Instruments.app → Time Profiler
   - Linux: `perf record -g`

3. **Check for priority inversion**
   - Look for high-priority threads blocked on mutexes
   - Replace with lock-free structures

4. **Monitor CPU affinity**
   - Ensure high-priority threads run on P-cores (Apple Silicon)
   - Check with Activity Monitor → CPU History

## References

- [Apple Threading Programming Guide](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/Multithreading/)
- [Real-Time Audio Programming 101](http://www.rossbencina.com/code/real-time-audio-programming-101-time-waits-for-nothing)
- [Lock-Free Programming](https://preshing.com/20120612/an-introduction-to-lock-free-programming/)
