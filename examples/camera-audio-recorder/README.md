# Camera + Audio → MP4 Recorder

This example demonstrates recording synchronized camera video and microphone audio to an MP4 file using the `Mp4WriterProcessor`.

## Features

- **Camera Capture**: Captures video from the default camera (or specified device)
- **Audio Capture**: Captures stereo audio from the default microphone (or specified device)
- **A/V Synchronization**: Maintains audio/video sync using streamlib's sync primitives
  - Video frames ahead of audio: dropped
  - Video frames behind audio: duplicated
  - Configurable sync tolerance (default: 16.6ms for 60fps)
- **MP4 Output**: Writes synchronized video and audio to an MP4 file
  - Video: H.264 codec at 5 Mbps
  - Audio: LPCM (uncompressed) stereo at 48kHz

## Building

```bash
cargo build --release
```

## Running

### Basic Usage (default output)

```bash
cargo run --release
```

This will record to `/tmp/recording.mp4`.

### Custom Output Path

```bash
OUTPUT_PATH=/path/to/output.mp4 cargo run --release
```

### Permissions (macOS)

The example will request camera and microphone permissions on first run. Grant these permissions to allow recording.

## Pipeline Architecture

```
┌─────────────┐
│   Camera    │
│  Processor  │
└──────┬──────┘
       │ VideoFrame
       │
       ▼
┌─────────────┐
│  MP4 Writer │◄──── AudioFrame<2>
│  Processor  │
└─────────────┘
       ▲
       │ AudioFrame<2>
       │
┌──────┴──────┐
│    Audio    │
│   Capture   │
│  Processor  │
└─────────────┘
```

## Stopping Recording

- **macOS**: Press `Cmd+Q` or `Ctrl+C`
- **Other platforms**: Press `Ctrl+C`

The MP4 file will be properly finalized on shutdown, ensuring the file is playable.

## Playing the Recording

### Using ffplay

```bash
ffplay /tmp/recording.mp4
```

### Using QuickTime Player (macOS)

```bash
open /tmp/recording.mp4
```

### Analyzing with ffprobe

```bash
ffprobe /tmp/recording.mp4
```

## Sync Statistics

The example logs sync statistics including:
- Total frames written
- Frames dropped (video ahead)
- Frames duplicated (video behind)

Check the console output for these statistics when the recording stops.

## Configuration

The MP4 writer can be configured with:

```rust
Mp4WriterConfig {
    output_path: PathBuf::from("/path/to/output.mp4"),
    sync_tolerance_ms: Some(16.6),  // Sync tolerance in milliseconds
    video_codec: Some("avc1".to_string()),  // H.264
    video_bitrate: Some(5_000_000),  // 5 Mbps
    audio_codec: Some("aac".to_string()),  // AAC (currently LPCM)
    audio_bitrate: Some(128_000),  // 128 kbps
}
```

## Notes

- Audio is currently written as LPCM (uncompressed PCM)
- AAC audio encoding will be added in a future update
- Video frame writing is fully implemented with H.264 encoding
- The sync tolerance determines how much A/V drift is acceptable before correction
