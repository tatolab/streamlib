# Microphone → CLAP Reverb → Speaker

Demonstrates streamlib's audio processing pipeline using **CLAP as the "shader language for audio"**.

Just as video shaders transform pixels on GPU, CLAP plugins transform audio in real-time.

## Pipeline

```text
[Microphone] → [CLAP Reverb Plugin] → [Speaker]
```

## What This Demonstrates

- **CLAP as Core**: CLAP is a first-class citizen in streamlib (required dependency like wgpu)
- **Zero-copy audio**: AudioFrame flows through pipeline without copying
- **Real-time processing**: Sub-10ms latency from mic to speaker
- **Plugin discovery**: Automatic scanning of installed CLAP plugins
- **Runtime audio config**: Consistent sample rate across all processors

## Requirements

- **CLAP reverb plugin** installed (e.g., Surge XT Effects, Airwindows Consolidated)
- **Microphone access** (will request permission on macOS)
- **Audio output device**

### Installing CLAP Plugins

**Surge XT Effects** (recommended):
- Download: https://surge-synthesizer.github.io/
- Includes high-quality reverb, EQ, compressor, and more
- Free and open source

**Airwindows Consolidated**:
- Download: https://github.com/baconpaul/airwin2rack
- Massive collection of unique effects
- Free and open source

**Installation Paths:**
- **macOS**: `~/Library/Audio/Plug-Ins/CLAP/` or `/Library/Audio/Plug-Ins/CLAP/`
- **Linux**: `~/.clap/` or `/usr/lib/clap/`
- **Windows**: `%COMMONPROGRAMFILES%\CLAP\` or `%LOCALAPPDATA%\Programs\Common\CLAP\`

## Running

```bash
# From repository root
nx run microphone-reverb-speaker:run

# Or directly
cd examples/microphone-reverb-speaker
cargo run
```

## What You'll See

```
🎙️  Microphone → CLAP Reverb → Speaker Example

🔍 Scanning for installed CLAP plugins...
✅ Found 15 CLAP plugins:
   [0] Surge XT Effects by Surge Synth Team
   [1] Airwindows Consolidated by Airwindows
   ...

🔍 Looking for reverb plugin...
✅ Using: Surge XT Effects by Surge Synth Team
   Path: /Library/Audio/Plug-Ins/CLAP/Surge XT Effects.clap

🎛️  Creating audio runtime...
   Sample rate: 48000 Hz
   Buffer size: 2048 samples
   Channels: 2

🎤 Setting up microphone input...
✅ Using microphone: Built-in Microphone

🎛️  Loading CLAP plugin...
✅ Plugin loaded: Surge XT Effects
   Activating plugin...
✅ Plugin activated
   Plugin has 42 parameters
   Set Mix: 30%
   Set Room Size: 60%

🔊 Setting up speaker output...
✅ Using speaker: Built-in Speakers

🔗 Building audio pipeline...
✅ Pipeline connected:
   processor_0 (mic) → processor_1 (reverb) → processor_2 (speaker)

▶️  Starting audio processing...
   Press Ctrl+C to stop

🎙️  Speak into your microphone - you should hear yourself with reverb!
```

## How It Works

1. **Plugin Discovery**: Scans system paths for installed CLAP plugins
2. **Audio Config**: Runtime creates shared audio config (48kHz, stereo, 2048 buffer)
3. **Microphone Setup**: Creates `AudioCaptureProcessor` using runtime's config
4. **Plugin Loading**: Loads CLAP plugin and activates with same config
5. **Parameter Setup**: Sets reverb mix and room size
6. **Speaker Setup**: Creates `AudioOutputProcessor`
7. **Pipeline Connection**: Wires mic → reverb → speaker
8. **Real-time Processing**: Runtime ticks at 60 FPS, audio flows through pipeline

## Code Architecture

This example demonstrates streamlib's design philosophy:

- **CLAP = Audio Shaders**: Just as WGSL shaders process video on GPU, CLAP plugins process audio
- **Consistent Audio Config**: All processors use `runtime.audio_config()` to avoid pitch shifts
- **Zero-copy Pipeline**: `AudioFrame` passes by reference through connected ports
- **Plugin-first Design**: CLAP is the standard way to process audio in streamlib

## Next Steps

- Try different CLAP plugins (delay, chorus, distortion)
- Adjust plugin parameters in real-time
- Chain multiple effects (mic → EQ → compressor → reverb → speaker)
- Build complex audio graphs with parallel processing
