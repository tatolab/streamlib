#!/usr/bin/env python3
"""
Test streamlib audio integration with real-time GPU processing.

Demonstrates:
- @audio_source decorator for microphone capture
- @audio_effect decorator for GPU effects
- @audio_sink_file decorator for saving to WAV file
- @audio_sink_speaker decorator for real-time playback
- Real-time streaming architecture (~10ms chunks)
- AudioBuffer with wgpu.GPUBuffer

Usage:
    # List audio devices
    uv run python examples/streamlib_audio_test.py --list-devices

    # Save to file with passthrough (default)
    uv run python examples/streamlib_audio_test.py

    # Save to file with reverb effect
    uv run python examples/streamlib_audio_test.py --device "MacBook" --effect reverb --duration 5

    # Play through speakers in real-time
    uv run python examples/streamlib_audio_test.py --output-mode speaker --duration 5

    # Both file and speaker output
    uv run python examples/streamlib_audio_test.py --output-mode both --effect reverb --duration 10
"""

import argparse
import asyncio
from pathlib import Path

# Import streamlib decorators
import sys
sys.path.insert(0, str(Path(__file__).parent.parent / "packages" / "streamlib" / "src"))

from streamlib import (
    audio_source,
    audio_effect,
    audio_sink_file,
    audio_sink_speaker,
    AudioBuffer,
    StreamRuntime,
    Stream
)

# TYPE_CHECKING import for type hints
from typing import TYPE_CHECKING
if TYPE_CHECKING:
    from streamlib.gpu.context import GPUContext

# Import sounddevice for listing devices
import sounddevice as sd


# WGSL Reverb Shader (embedded)
REVERB_SHADER = """
// Simple reverb effect using multiple delay taps
// Processes audio samples in parallel on GPU

@group(0) @binding(0) var<storage, read> input_audio: array<f32>;
@group(0) @binding(1) var<storage, read_write> output_audio: array<f32>;

struct ReverbParams {
    sample_rate: u32,
    num_samples: u32,
    feedback: f32,     // 0.0 to 0.9
    wet_mix: f32,      // 0.0 to 1.0
}

@group(0) @binding(2) var<uniform> params: ReverbParams;

// Multi-tap delay for reverb (times in milliseconds converted to samples)
// At 48kHz: 50ms = 2400 samples, 100ms = 4800 samples, etc.

@compute @workgroup_size(256)
fn apply_reverb(@builtin(global_invocation_id) id: vec3<u32>) {
    let idx = id.x;

    if idx >= params.num_samples {
        return;
    }

    let sample = input_audio[idx];

    // Multiple delay taps for richer reverb
    let delay1_samples = (params.sample_rate * 29u) / 1000u;  // 29ms
    let delay2_samples = (params.sample_rate * 37u) / 1000u;  // 37ms
    let delay3_samples = (params.sample_rate * 53u) / 1000u;  // 53ms
    let delay4_samples = (params.sample_rate * 71u) / 1000u;  // 71ms

    var reverb = 0.0;

    // Add delayed versions with decreasing amplitude
    if idx >= delay1_samples {
        reverb += input_audio[idx - delay1_samples] * 0.4;
    }
    if idx >= delay2_samples {
        reverb += input_audio[idx - delay2_samples] * 0.3;
    }
    if idx >= delay3_samples {
        reverb += input_audio[idx - delay3_samples] * 0.2;
    }
    if idx >= delay4_samples {
        reverb += input_audio[idx - delay4_samples] * 0.1;
    }

    // Apply feedback
    reverb = reverb * params.feedback;

    // Mix dry and wet signals
    let dry_mix = 1.0 - params.wet_mix;
    output_audio[idx] = (sample * dry_mix) + (reverb * params.wet_mix);
}
"""


def list_devices():
    """List available audio input and output devices."""
    devices = sd.query_devices()

    # List input devices
    print("\n📱 Available Audio Input Devices:")
    for idx, device in enumerate(devices):
        if device["max_input_channels"] > 0:
            default = " (default)" if idx == sd.default.device[0] else ""
            print(
                f"  [{idx}] {device['name']}{default} "
                f"({device['max_input_channels']} channels, "
                f"{device['default_samplerate']:.0f}Hz)"
            )

    # List output devices
    print("\n📢 Available Audio Output Devices:")
    for idx, device in enumerate(devices):
        if device["max_output_channels"] > 0:
            default = " (default)" if idx == sd.default.device[1] else ""
            print(
                f"  [{idx}] {device['name']}{default} "
                f"({device['max_output_channels']} channels, "
                f"{device['default_samplerate']:.0f}Hz)"
            )


# Define passthrough effect (no processing)
@audio_effect
def passthrough_effect(buffer: AudioBuffer, gpu: 'GPUContext') -> AudioBuffer:  # type: ignore
    """
    Passthrough effect - returns buffer unchanged.

    Demonstrates that GPU pipeline works without processing.

    Note: gpu parameter is injected by decorator.
    """
    # Just return the buffer unchanged
    return buffer


# Define reverb effect (GPU processing)
@audio_effect
def reverb_effect(buffer: AudioBuffer, gpu: 'GPUContext') -> AudioBuffer:  # type: ignore
    """
    GPU reverb effect using WGSL compute shader.

    Processes audio on GPU using the proven architecture.

    Note: gpu parameter is injected by decorator.
    """
    # Cache pipeline on handler instance
    handler = reverb_effect
    if not hasattr(handler, 'pipeline'):
        # Create compute pipeline (cached)
        handler.pipeline = gpu.create_audio_compute_pipeline(
            REVERB_SHADER,
            entry_point="apply_reverb"
        )

        print("✅ Reverb pipeline created (cached)")

    # Create parameters buffer
    # Parameters: sample_rate, num_samples, feedback (0.6), wet_mix (0.5)
    params = [
        buffer.sample_rate,  # u32
        buffer.samples,      # u32
        0.6,                 # feedback (f32)
        0.5                  # wet_mix (f32)
    ]

    # Run compute shader (GPU → GPU, no CPU transfer)
    output_buffer = gpu.run_audio_compute(
        handler.pipeline,
        input_buffer=buffer.data,
        params=params,
        samples=buffer.samples,
        channels=buffer.channels
    )

    # Return new AudioBuffer with processed data
    return buffer.clone_with_buffer(output_buffer)


async def main():
    parser = argparse.ArgumentParser(
        description="Test streamlib audio integration"
    )
    parser.add_argument("--list-devices", action="store_true", help="List available devices")
    parser.add_argument("--device", default=None, help="Audio input device name substring")
    parser.add_argument("--effect", choices=["passthrough", "reverb"], default="passthrough",
                       help="Effect to apply")
    parser.add_argument("--duration", type=int, default=10, help="Duration in seconds")
    parser.add_argument("--output-mode", choices=["file", "speaker", "both"], default="file",
                       help="Output mode: file, speaker, or both")
    parser.add_argument("--output", default="streamlib_output.wav", help="Output audio file (for file mode)")
    parser.add_argument("--speaker-device", default=None, help="Audio output device name substring")
    args = parser.parse_args()

    # List devices
    if args.list_devices:
        list_devices()
        return

    print("🎙️  streamlib Audio Integration Test")
    print("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")
    print(f"Input Device: {args.device or 'default'}")
    print(f"Output Mode: {args.output_mode}")
    if args.output_mode in ["file", "both"]:
        print(f"Output File: {args.output}")
    if args.output_mode in ["speaker", "both"]:
        print(f"Speaker Device: {args.speaker_device or 'default'}")
    print(f"Effect: {args.effect}")
    print(f"Duration: {args.duration}s")
    print(f"Sample Rate: 48000Hz")
    print(f"Chunk Size: 512 samples (~10.7ms)")
    print()

    # Create runtime (audio-only, no display)
    runtime = StreamRuntime(fps=30)  # Tick rate for processing

    # Create microphone source with device name using decorator
    @audio_source(device_name=args.device, sample_rate=48000, chunk_size=512)
    def microphone(gpu: 'GPUContext' = None, device_name: str = None) -> AudioBuffer:  # type: ignore
        """
        Audio source from microphone - decorator handles everything!

        Returns GPU audio buffers automatically in real-time.
        """
        pass

    mic = microphone  # Decorator returns handler instance

    # Get effect handler (already instances from decorators)
    if args.effect == "passthrough":
        effect = passthrough_effect
    else:  # reverb
        effect = reverb_effect

    # Create sinks based on output mode
    sinks = []

    if args.output_mode in ["file", "both"]:
        # Create file sink with decorator
        @audio_sink_file(output_path=args.output)
        def file_output():
            """Save audio to file - decorator handles everything!"""
            pass

        sinks.append(file_output)

    if args.output_mode in ["speaker", "both"]:
        # Create speaker sink with decorator
        @audio_sink_speaker(device_name=args.speaker_device, sample_rate=48000, chunk_size=512)
        def speaker_output():
            """Play audio through speakers - decorator handles everything!"""
            pass

        sinks.append(speaker_output)

    # Add streams to runtime
    runtime.add_stream(Stream(mic))
    runtime.add_stream(Stream(effect))
    for sink in sinks:
        runtime.add_stream(Stream(sink))

    # Connect pipeline: mic → effect → sinks
    runtime.connect(mic.outputs['audio'], effect.inputs['audio'])
    for sink in sinks:
        runtime.connect(effect.outputs['audio'], sink.inputs['audio'])

    # Start runtime
    print("🔧 Starting runtime...")
    await runtime.start()
    print("✅ Runtime started")
    print()

    # Run for duration
    if args.output_mode == "speaker":
        print("🔴 Playing audio through speakers...")
    elif args.output_mode == "file":
        print("🔴 Recording audio to file...")
    else:
        print("🔴 Recording AND playing audio...")

    for i in range(args.duration):
        await asyncio.sleep(1)
        print(f"\r   {i + 1}/{args.duration}s", end="", flush=True)
    print()

    # Stop runtime
    print()
    print("⏹️  Stopping runtime...")
    await runtime.stop()

    print()
    print("✅ Test complete!")
    print()
    print("📊 Architecture:")
    print("  - Audio Device → @audio_source (GPU upload)")
    print(f"  - → @audio_effect ({args.effect}, GPU processing, ALL chunks)")
    if args.output_mode == "file":
        print("  - → @audio_sink_file (GPU download, save to WAV)")
    elif args.output_mode == "speaker":
        print("  - → @audio_sink_speaker (GPU download, real-time playback)")
    else:
        print("  - → @audio_sink_file (GPU download, save to WAV)")
        print("  - → @audio_sink_speaker (GPU download, real-time playback)")
    print()
    print("📊 Key Difference from Video:")
    print("  - Video: 30fps → read_latest() (skip old frames)")
    print("  - Audio: ~94 chunks/sec → read_all() (process every chunk)")
    print("  - Audio chunks must be processed in order, no skipping!")
    print()
    print("📊 Stats:")
    print("  - Each chunk: 512 samples (~10.7ms @ 48kHz)")
    print("  - Data stays on GPU during processing")
    print("  - Same streaming architecture as video pipeline")


if __name__ == "__main__":
    asyncio.run(main())
