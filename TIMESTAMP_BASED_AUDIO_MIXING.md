# Timestamp-Based Audio Mixing - Design Proposal

## Overview

Implement GStreamer-style timestamp-based synchronization in AudioMixer to properly handle multiple audio streams with potential timing skew, variable latency, or independent clocks.

---

## Current vs Proposed Behavior

### Current: "All Ready" Pattern

```rust
// Current: Wait for all inputs to have ANY data
let all_ready = self.input_ports.iter().all(|input| input.has_data());
if !all_ready {
    return Ok(());  // Skip if any input missing
}

// Mix whatever timestamps we have (may be misaligned)
let input_frames: [AudioFrame<1>; N] = /* read all */;
// No timestamp validation - just mix
```

**Problems**:
- ❌ Doesn't verify timestamps are aligned
- ❌ Can mix frames from different time periods (e.g., T=0 + T=1000ms)
- ❌ No handling of clock drift between sources
- ❌ No handling of dropped frames or gaps
- ❌ Can't distinguish "waiting for data" from "data exists but wrong timestamp"

### Proposed: Timestamp-Based Synchronization

```rust
// Step 1: Peek at timestamps WITHOUT consuming
let input_timestamps: [Option<i64>; N] = self.input_ports.iter()
    .map(|input| input.peek_timestamp())
    .collect();

// Step 2: Determine target timestamp (earliest available from all ready inputs)
let target_timestamp = determine_target_timestamp(&input_timestamps)?;

// Step 3: Wait until ALL inputs have frames at or past target timestamp
let all_aligned = input_timestamps.iter()
    .all(|ts| ts.map_or(false, |t| t >= target_timestamp));

if !all_aligned {
    return Ok(());  // Wait for alignment
}

// Step 4: Read frames closest to target timestamp
let input_frames: [AudioFrame<1>; N] = /* read aligned frames */;

// Step 5: Mix with timestamp validation
assert!(all_frames_within_threshold(&input_frames, target_timestamp));
let mixed = mix_frames(&input_frames);
```

**Benefits**:
- ✅ Guarantees temporal alignment of mixed frames
- ✅ Handles clock drift gracefully
- ✅ Detects and reports timing issues (dropped frames, excessive latency)
- ✅ Enables proper AV sync (video can reference audio timestamps)
- ✅ Supports variable-rate sources (e.g., network streams)

---

## API Design

### New Methods Needed

#### 1. Peek Timestamp (Non-Consuming)

```rust
// In StreamInput<T: PortMessage>
impl<T: PortMessage> StreamInput<T> {
    /// Peek at the timestamp of the oldest frame without consuming it
    pub fn peek_timestamp(&self) -> Option<i64> {
        self.connection.lock()
            .as_ref()?
            .peek_timestamp()
    }
}

// In ProcessorConnection<T>
impl<T: Clone + Send + 'static> ProcessorConnection<T> {
    /// Peek at the oldest frame without consuming
    pub fn peek(&self) -> Option<T> {
        self.consumer.lock().peek().ok()
    }

    /// Peek at the timestamp of the oldest frame
    /// Requires T to provide timestamp (via trait or specific type check)
    pub fn peek_timestamp(&self) -> Option<i64>
    where
        T: HasTimestamp,
    {
        self.peek().map(|frame| frame.timestamp_ns())
    }
}
```

**Note**: RTRB's `Consumer` already has `.peek()` method:
```rust
// From rtrb crate
impl<T> Consumer<T> {
    pub fn peek(&self) -> Result<&T, PopError>  // Non-consuming!
}
```

#### 2. Timestamp Trait for Frame Types

```rust
/// Trait for frame types that have timestamps
pub trait HasTimestamp {
    fn timestamp_ns(&self) -> i64;
    fn frame_number(&self) -> u64;
}

impl<const CHANNELS: usize> HasTimestamp for AudioFrame<CHANNELS> {
    fn timestamp_ns(&self) -> i64 {
        self.timestamp_ns
    }

    fn frame_number(&self) -> u64 {
        self.frame_number
    }
}

impl HasTimestamp for VideoFrame {
    fn timestamp_ns(&self) -> i64 {
        self.timestamp_ns
    }

    fn frame_number(&self) -> u64 {
        self.frame_number
    }
}
```

#### 3. Read Frame at Timestamp

```rust
impl<T: PortMessage + HasTimestamp> StreamInput<T> {
    /// Read the frame closest to the target timestamp
    /// Drains all frames before target, returns frame at or after target
    pub fn read_at_timestamp(&self, target_ns: i64, tolerance_ns: i64) -> Option<T> {
        let conn = self.connection.lock();
        let conn = conn.as_ref()?;

        let mut candidate = None;

        // Drain frames until we reach target timestamp
        while let Some(frame) = conn.peek() {
            let frame_ts = frame.timestamp_ns();

            if frame_ts >= target_ns {
                // Found frame at or past target
                candidate = conn.read_latest();
                break;
            } else if target_ns - frame_ts > tolerance_ns {
                // Frame is too old - discard it
                let _ = conn.read_latest();
                tracing::warn!(
                    "Discarded late frame: target={}, actual={}, delta={}ms",
                    target_ns,
                    frame_ts,
                    (target_ns - frame_ts) / 1_000_000
                );
            } else {
                // Frame is close enough - use it
                candidate = conn.read_latest();
                break;
            }
        }

        candidate
    }
}
```

---

## AudioMixer Implementation

### Configuration

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioMixerConfig {
    pub strategy: MixingStrategy,
    pub sync_strategy: SyncStrategy,  // NEW
    pub max_timestamp_drift_ms: u32,  // NEW: Default 100ms
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncStrategy {
    /// Wait for all inputs to have data (current behavior)
    /// Fast but no temporal alignment guarantee
    AllReady,

    /// Wait for all inputs to have frames at same timestamp (within threshold)
    /// Proper temporal alignment, handles clock drift
    TimestampBased { tolerance_ms: u32 },

    /// Drop frames from fast inputs to match slowest input
    /// Best for real-time mixing of live sources
    DropToSlowest { max_drift_ms: u32 },
}

impl Default for SyncStrategy {
    fn default() -> Self {
        SyncStrategy::TimestampBased { tolerance_ms: 10 }  // 10ms tolerance
    }
}
```

### Enhanced Process Method

```rust
impl<const N: usize> StreamProcessor for AudioMixerProcessor<N> {
    fn process(&mut self) -> Result<()> {
        match self.sync_strategy {
            SyncStrategy::AllReady => self.process_all_ready(),
            SyncStrategy::TimestampBased { tolerance_ms } => {
                self.process_timestamp_based(tolerance_ms)
            }
            SyncStrategy::DropToSlowest { max_drift_ms } => {
                self.process_drop_to_slowest(max_drift_ms)
            }
        }
    }
}

impl<const N: usize> AudioMixerProcessor<N> {
    /// Current implementation (renamed for clarity)
    fn process_all_ready(&mut self) -> Result<()> {
        let all_ready = self.input_ports.iter().all(|input| input.has_data());
        if !all_ready {
            return Ok(());
        }

        let input_frames: [AudioFrame<1>; N] = std::array::from_fn(|i| {
            self.input_ports[i].read_latest().expect("checked has_data")
        });

        self.mix_and_output(&input_frames)
    }

    /// NEW: Timestamp-based synchronization
    fn process_timestamp_based(&mut self, tolerance_ms: u32) -> Result<()> {
        let tolerance_ns = tolerance_ms as i64 * 1_000_000;

        // Step 1: Peek at all input timestamps
        let input_timestamps: [Option<i64>; N] = std::array::from_fn(|i| {
            self.input_ports[i].peek_timestamp()
        });

        // Step 2: Check if all inputs have data
        if input_timestamps.iter().any(|ts| ts.is_none()) {
            tracing::debug!(
                "[AudioMixer<{}>] Waiting for all inputs to have data: {:?}",
                N,
                input_timestamps
            );
            return Ok(());
        }

        // Step 3: Find target timestamp (earliest from all inputs)
        let timestamps: [i64; N] = std::array::from_fn(|i| {
            input_timestamps[i].unwrap()
        });

        let min_timestamp = *timestamps.iter().min().unwrap();
        let max_timestamp = *timestamps.iter().max().unwrap();
        let timestamp_spread_ns = max_timestamp - min_timestamp;

        // Step 4: Check if timestamps are within tolerance
        if timestamp_spread_ns > tolerance_ns {
            tracing::debug!(
                "[AudioMixer<{}>] Timestamps not aligned: min={}, max={}, spread={}ms",
                N,
                min_timestamp,
                max_timestamp,
                timestamp_spread_ns / 1_000_000
            );

            // Some inputs are ahead - wait for others to catch up
            // OR drop frames from fast inputs if drift exceeds max threshold
            if timestamp_spread_ns > self.max_timestamp_drift_ns() {
                tracing::warn!(
                    "[AudioMixer<{}>] Timestamp drift exceeds maximum ({}ms), dropping old frames",
                    N,
                    timestamp_spread_ns / 1_000_000
                );

                // Drop frames from slow inputs to catch up
                for (i, ts) in timestamps.iter().enumerate() {
                    if *ts < min_timestamp + tolerance_ns {
                        // This input is behind - drop its frame
                        let _ = self.input_ports[i].read_latest();
                        tracing::debug!(
                            "[AudioMixer<{}>] Dropped frame from input {}: ts={}",
                            N,
                            i,
                            ts
                        );
                    }
                }
            }

            return Ok(());
        }

        // Step 5: All timestamps aligned - read frames
        let input_frames: [AudioFrame<1>; N] = std::array::from_fn(|i| {
            self.input_ports[i].read_latest().expect("checked peek_timestamp")
        });

        // Step 6: Validate timestamp alignment (paranoid check)
        let actual_timestamps: Vec<i64> = input_frames.iter()
            .map(|f| f.timestamp_ns)
            .collect();

        let actual_spread = actual_timestamps.iter().max().unwrap()
            - actual_timestamps.iter().min().unwrap();

        if actual_spread > tolerance_ns {
            tracing::error!(
                "[AudioMixer<{}>] Timestamp validation failed after read: {:?}",
                N,
                actual_timestamps
            );
            // Continue anyway, but log error
        }

        // Step 7: Mix with target timestamp
        self.mix_and_output_with_timestamp(&input_frames, min_timestamp)
    }

    /// NEW: Drop-to-slowest synchronization
    fn process_drop_to_slowest(&mut self, max_drift_ms: u32) -> Result<()> {
        let max_drift_ns = max_drift_ms as i64 * 1_000_000;

        // Peek at all timestamps
        let input_timestamps: [Option<i64>; N] = std::array::from_fn(|i| {
            self.input_ports[i].peek_timestamp()
        });

        // Wait for all inputs
        if input_timestamps.iter().any(|ts| ts.is_none()) {
            return Ok(());
        }

        let timestamps: [i64; N] = std::array::from_fn(|i| {
            input_timestamps[i].unwrap()
        });

        // Find slowest (oldest timestamp)
        let slowest_timestamp = *timestamps.iter().min().unwrap();

        // Drop frames from fast inputs until they're close to slowest
        for (i, ts) in timestamps.iter().enumerate() {
            let drift = ts - slowest_timestamp;

            if drift > max_drift_ns {
                // This input is too far ahead - drop frames
                tracing::warn!(
                    "[AudioMixer<{}>] Input {} ahead by {}ms, dropping frames",
                    N,
                    i,
                    drift / 1_000_000
                );

                // Drop frames until we're within tolerance
                while let Some(peek_ts) = self.input_ports[i].peek_timestamp() {
                    if peek_ts - slowest_timestamp <= max_drift_ns {
                        break;  // Close enough now
                    }

                    let _ = self.input_ports[i].read_latest();
                    tracing::debug!(
                        "[AudioMixer<{}>] Dropped frame from input {}: ts={}",
                        N,
                        i,
                        peek_ts
                    );
                }
            }
        }

        // Now process with aligned timestamps
        self.process_timestamp_based(max_drift_ms)
    }

    fn max_timestamp_drift_ns(&self) -> i64 {
        self.max_timestamp_drift_ms as i64 * 1_000_000
    }

    fn mix_and_output_with_timestamp(
        &mut self,
        input_frames: &[AudioFrame<1>; N],
        output_timestamp_ns: i64,
    ) -> Result<()> {
        // Existing mixing logic
        let mut signals: Vec<_> = input_frames.iter()
            .map(|frame| frame.read())
            .collect();

        let mut mixed_samples = Vec::with_capacity(self.buffer_size * 2);

        for _ in 0..self.buffer_size {
            let mut mixed_mono = 0.0f32;
            for signal in &mut signals {
                mixed_mono += signal.next()[0];
            }

            let final_sample = match self.strategy {
                MixingStrategy::Sum => mixed_mono,
                MixingStrategy::SumNormalized => mixed_mono / N as f32,
                MixingStrategy::SumClipped => mixed_mono.clamp(-1.0, 1.0),
            };

            mixed_samples.push(final_sample);
            mixed_samples.push(final_sample);
        }

        // Use provided timestamp instead of input[0].timestamp_ns
        let output_frame = AudioFrame::<2>::new(
            mixed_samples,
            output_timestamp_ns,  // ✅ Aligned timestamp
            self.frame_counter
        );

        self.output_ports.audio.write(output_frame);
        self.frame_counter += 1;

        Ok(())
    }
}
```

---

## Implementation Phases

### Phase 1: Add Peek Timestamp API ✅ (Foundation)
**Estimated Effort**: 2-3 hours

1. Add `peek()` wrapper to `ProcessorConnection<T>`
2. Add `HasTimestamp` trait
3. Implement `HasTimestamp` for `AudioFrame<C>` and `VideoFrame`
4. Add `peek_timestamp()` to `StreamInput<T>`
5. Unit tests for peek functionality

**Deliverables**:
- `libs/streamlib/src/core/bus/connection.rs` - `peek()` method
- `libs/streamlib/src/core/bus/ports.rs` - `peek_timestamp()` method
- `libs/streamlib/src/core/frames/mod.rs` - `HasTimestamp` trait
- Tests verifying non-consuming peek

### Phase 2: Add SyncStrategy Configuration ✅
**Estimated Effort**: 1-2 hours

1. Add `SyncStrategy` enum to `audio_mixer.rs`
2. Add `max_timestamp_drift_ms` to `AudioMixerConfig`
3. Update `AudioMixerProcessor` struct with new fields
4. Keep existing behavior as default (`AllReady` mode)

**Deliverables**:
- `libs/streamlib/src/core/transformers/audio_mixer.rs` - Config updates
- Backward compatible (default = current behavior)

### Phase 3: Implement Timestamp-Based Sync ✅
**Estimated Effort**: 4-6 hours

1. Implement `process_timestamp_based()` method
2. Add timestamp alignment checks
3. Add drift detection and logging
4. Add frame dropping on excessive drift
5. Integration tests with synthetic timestamp skew

**Deliverables**:
- Full timestamp-based synchronization
- Configurable tolerance (default 10ms)
- Drift warnings and frame drop logging

### Phase 4: Implement Drop-to-Slowest Sync ⏳
**Estimated Effort**: 2-3 hours

1. Implement `process_drop_to_slowest()` method
2. Add logic to drop fast input frames
3. Tests with variable-rate inputs

**Deliverables**:
- Alternative sync strategy for real-time sources
- Automatic frame dropping to maintain alignment

### Phase 5: Testing and Validation ⏳
**Estimated Effort**: 3-4 hours

1. Unit tests for each sync strategy
2. Integration tests with ChordGenerator
3. Synthetic tests with intentional timestamp skew
4. Performance benchmarks (overhead of timestamp checks)
5. Update examples to demonstrate sync strategies

**Deliverables**:
- Comprehensive test suite
- Updated `audio-mixer-demo` example
- Performance analysis document

---

## Usage Examples

### Example 1: ChordGenerator with Timestamp Sync

```yaml
# Pipeline configuration
processors:
  - name: chord_gen
    type: ChordGenerator

  - name: mixer
    type: AudioMixer<3>
    config:
      strategy: SumNormalized
      sync_strategy:
        TimestampBased:
          tolerance_ms: 10  # 10ms alignment tolerance
      max_timestamp_drift_ms: 100  # Warn if drift > 100ms

connections:
  - from: chord_gen.tone_c4
    to: mixer.input_0
  - from: chord_gen.tone_e4
    to: mixer.input_1
  - from: chord_gen.tone_g4
    to: mixer.input_2
```

**Expected Behavior**:
- Mixer waits until all 3 inputs have frames with timestamps within 10ms
- If timestamps drift > 100ms, drops old frames and warns
- Output has guaranteed temporal alignment

### Example 2: Microphone Array with Drop-to-Slowest

```yaml
processors:
  - name: mic_array
    type: MicrophoneArray
    config:
      mics: [0, 1, 2, 3]  # 4 USB microphones

  - name: mixer
    type: AudioMixer<4>
    config:
      sync_strategy:
        DropToSlowest:
          max_drift_ms: 50  # Drop frames if any mic drifts > 50ms
```

**Expected Behavior**:
- Mixer syncs to slowest microphone
- Fast microphones have frames dropped automatically
- Handles USB timing jitter gracefully

### Example 3: Network Streams with Timestamp Validation

```yaml
processors:
  - name: webrtc_receiver_1
    type: WebRTCReceiver

  - name: webrtc_receiver_2
    type: WebRTCReceiver

  - name: mixer
    type: AudioMixer<2>
    config:
      sync_strategy:
        TimestampBased:
          tolerance_ms: 100  # Generous tolerance for network
      max_timestamp_drift_ms: 500  # Drop if > 500ms drift
```

**Expected Behavior**:
- Handles variable network latency
- Drops frames if one stream falls too far behind
- Validates temporal alignment of mixed output

---

## Benefits Over Current Implementation

| Aspect | Current (AllReady) | Timestamp-Based |
|--------|-------------------|-----------------|
| **Temporal Alignment** | ❌ No guarantee | ✅ Guaranteed within tolerance |
| **Clock Drift Handling** | ❌ None | ✅ Automatic detection and correction |
| **Frame Drop Detection** | ❌ Silent | ✅ Logged with metrics |
| **AV Sync Support** | ⚠️ Difficult | ✅ Natural (timestamps match) |
| **Variable-Rate Sources** | ❌ Not supported | ✅ Supported via timestamp logic |
| **Debug/Monitoring** | ⚠️ Limited | ✅ Detailed timing logs |
| **CPU Overhead** | ~1 µs | ~2-3 µs (peek + checks) |
| **Correctness** | ⚠️ May mix misaligned frames | ✅ Only mixes aligned frames |

---

## Performance Analysis

### Overhead of Timestamp Checks

```rust
// Current AllReady: ~1 µs
let all_ready = self.input_ports.iter().all(|input| input.has_data());

// Timestamp-Based: ~2-3 µs
let timestamps = std::array::from_fn(|i| self.input_ports[i].peek_timestamp());
let min_ts = timestamps.iter().min();
let max_ts = timestamps.iter().max();
let spread = max_ts - min_ts;
if spread > tolerance { /* ... */ }
```

**Additional Cost**: ~1-2 µs per process() call

**Context**:
- Audio frame at 48kHz, 256 samples = 5.33ms budget
- 2 µs overhead = **0.04%** of frame time
- **Negligible impact**

### Memory Overhead

- `SyncStrategy` enum: 8 bytes
- `max_timestamp_drift_ms`: 4 bytes
- Per-process temp arrays: `[Option<i64>; N]` = N * 16 bytes (stack)

**Total**: ~12 bytes static + 16N bytes per call (stack)

For N=8 (max typical): 12 + 128 = **140 bytes overhead**

---

## Migration Path

### Backward Compatibility

**Default behavior = current behavior**:
```rust
impl Default for SyncStrategy {
    fn default() -> Self {
        SyncStrategy::AllReady  // ✅ Existing behavior preserved
    }
}
```

### Opt-In Migration

Users can enable timestamp sync explicitly:
```rust
let mixer = AudioMixerProcessor::<3>::new_with_sync(
    MixingStrategy::SumNormalized,
    SyncStrategy::TimestampBased { tolerance_ms: 10 },
);
```

Or via config:
```yaml
type: AudioMixer<3>
config:
  sync_strategy:
    TimestampBased:
      tolerance_ms: 10
```

### Gradual Rollout

1. **Phase 1**: Add API, default to `AllReady` (no behavior change)
2. **Phase 2**: Test timestamp sync with ChordGenerator
3. **Phase 3**: Update examples to use timestamp sync
4. **Phase 4**: Make timestamp sync default in v2.0

---

## Testing Strategy

### Unit Tests

```rust
#[test]
fn test_timestamp_alignment_within_tolerance() {
    let mut mixer = AudioMixerProcessor::<3>::new_with_sync(
        MixingStrategy::SumNormalized,
        SyncStrategy::TimestampBased { tolerance_ms: 10 },
    );

    // Write frames with aligned timestamps
    let base_ts = 1000_000_000;  // 1 second
    mixer.input_ports[0].write(AudioFrame::new(vec![0.1; 256], base_ts + 0, 0));
    mixer.input_ports[1].write(AudioFrame::new(vec![0.2; 256], base_ts + 5_000_000, 0));  // +5ms
    mixer.input_ports[2].write(AudioFrame::new(vec![0.3; 256], base_ts + 8_000_000, 0));  // +8ms

    // Should process (all within 10ms tolerance)
    assert!(mixer.process().is_ok());
    assert!(mixer.output_ports.audio.has_data());
}

#[test]
fn test_timestamp_misalignment_waits() {
    let mut mixer = AudioMixerProcessor::<3>::new_with_sync(
        MixingStrategy::SumNormalized,
        SyncStrategy::TimestampBased { tolerance_ms: 10 },
    );

    // Write frames with misaligned timestamps
    let base_ts = 1000_000_000;
    mixer.input_ports[0].write(AudioFrame::new(vec![0.1; 256], base_ts + 0, 0));
    mixer.input_ports[1].write(AudioFrame::new(vec![0.2; 256], base_ts + 50_000_000, 0));  // +50ms!
    mixer.input_ports[2].write(AudioFrame::new(vec![0.3; 256], base_ts + 100_000_000, 0));  // +100ms!

    // Should skip (timestamps not aligned)
    assert!(mixer.process().is_ok());
    assert!(!mixer.output_ports.audio.has_data());
}

#[test]
fn test_drop_to_slowest_discards_fast_frames() {
    let mut mixer = AudioMixerProcessor::<2>::new_with_sync(
        MixingStrategy::SumNormalized,
        SyncStrategy::DropToSlowest { max_drift_ms: 20 },
    );

    let base_ts = 1000_000_000;

    // Input 0: Slow (at base_ts)
    mixer.input_ports[0].write(AudioFrame::new(vec![0.1; 256], base_ts, 0));

    // Input 1: Fast (way ahead)
    mixer.input_ports[1].write(AudioFrame::new(vec![0.2; 256], base_ts + 100_000_000, 0));  // +100ms
    mixer.input_ports[1].write(AudioFrame::new(vec![0.3; 256], base_ts + 110_000_000, 1));  // +110ms
    mixer.input_ports[1].write(AudioFrame::new(vec![0.4; 256], base_ts + 5_000_000, 2));   // +5ms (closer)

    // Process should drop first 2 frames from input 1
    assert!(mixer.process().is_ok());

    // Verify frames were dropped (check via frame_number in output)
    let output = mixer.output_ports.audio.read_latest().unwrap();
    // Should have mixed input[0].frame_0 with input[1].frame_2
}
```

---

## Conclusion

Timestamp-based synchronization provides:

1. **Correctness**: Guaranteed temporal alignment of mixed audio
2. **Robustness**: Handles clock drift, dropped frames, variable latency
3. **Observability**: Detailed logging of timing issues
4. **Flexibility**: Multiple sync strategies for different use cases
5. **Compatibility**: Backward compatible via default config

**Recommendation**: Implement in phases, starting with Phase 1 (peek API) which has broad utility beyond just AudioMixer.

---

**Next Steps**:
1. Review and approve design
2. Implement Phase 1 (peek timestamp API)
3. Update AudioMixer with timestamp-based sync
4. Test with ChordGenerator and real-world scenarios
5. Document best practices for timestamp management

**Estimated Total Effort**: 12-18 hours across all phases
**Priority**: Medium-High (correctness improvement, enables future features)
