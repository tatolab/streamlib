# Audio Mixer Demo

Demonstrates streamlib's `AudioMixerProcessor` - mixing multiple audio streams into a single output.

## What This Example Shows

- Creating multiple audio sources (test tone generators)
- Mixing them together using `AudioMixerProcessor`
- Different mixing strategies (sum normalized vs sum clipped)
- Real-time audio processing pipeline

## How It Works

This example creates three test tone generators at different frequencies:
- **440 Hz** - Musical note A4 (left channel louder)
- **554.37 Hz** - Musical note C#5 (right channel louder)
- **659.25 Hz** - Musical note E5 (centered)

The mixer combines all three tones and outputs to your speakers. You'll hear a chord!

## Running the Example

```bash
# From the root of the streamlib repository
cargo run -p audio-mixer-demo

# Or using Nx
nx run audio-mixer-demo:run
```

## Expected Behavior

You should hear a pleasant chord (A major) through your speakers:
- Three distinct tones blended together
- Stereo panning (left, right, center)
- Clean audio with no distortion (thanks to SumNormalized strategy)

Press Ctrl+C to stop.

## Mixing Strategies

The example demonstrates **SumNormalized** strategy by default:
- Adds all input signals together
- Divides by the number of active inputs
- **Prevents clipping** automatically
- Safe for real-time mixing

To try **SumClipped** strategy (may cause distortion with loud inputs):
1. Edit `src/main.rs`
2. Change `MixingStrategy::SumNormalized` to `MixingStrategy::SumClipped`
3. Rebuild and run

## Use Cases

This pattern is useful for:
- **Mixing microphone + music** for streaming/podcasting
- **Combining multiple audio sources** for recording
- **Agent-controlled audio routing** via MCP
- **Real-time audio production** pipelines

## Architecture

```
TestTone (440 Hz) → mixer.input_0
                         ↓
TestTone (554 Hz) → mixer.input_1  →  Speaker
                         ↓
TestTone (659 Hz) → mixer.input_2
```

## Code Highlights

- **Dynamic inputs**: Mixer created with 3 inputs, but supports any number
- **Type-safe connections**: Compiler enforces AudioFrame → AudioFrame
- **Real-time safe**: No allocations in audio processing thread
- **Zero-copy**: Audio flows through Arc references

## Related Examples

- `microphone-reverb-speaker` - CLAP audio effects
- `camera-display` - Video processing pipeline
- `simple-pipeline` - Basic streamlib concepts
