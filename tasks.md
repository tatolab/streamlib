# Example Migration Tasks

Remaining examples to enable, ordered by priority (unique/untested components first).

## Priority Queue

| Priority | Example | New/Untested Components | Status |
|----------|---------|------------------------|--------|
| 1 | `camera-audio-recorder` | MP4WriterProcessor | ✅ Done |
| 2 | `microphone-reverb-speaker` | CLAP plugin hosting | ⬚ TODO |
| 3 | `whep-player` | WebRTC WHEP (inbound) | ⬚ TODO |
| 4 | `runtime-graph-json-demo` | Graph JSON serialization | ⬚ TODO |
| 5 | `simple-pipeline` | None (redundant) | ⬚ TODO |
| 6 | `graph-json-export` | None (old syntax rewrite) | ⬚ TODO |

## Blocked (Python bindings)

These require `streamlib-python` to be re-enabled first:

- `camera-python-display`
- `camera-dylib-display`

## Completed

| Example | Components Tested |
|---------|-------------------|
| `camera-audio-recorder` | MP4WriterProcessor, AudioCaptureProcessor, AudioResamplerProcessor, AudioChannelConverterProcessor |
| `camera-display` | CameraProcessor, DisplayProcessor |
| `audio-mixer-demo` | ChordGeneratorProcessor, AudioOutputProcessor, AudioClock |
| `webrtc-cloudflare-stream` | AudioCaptureProcessor, AudioResamplerProcessor, AudioChannelConverterProcessor, BufferRechunkerProcessor, WebRtcWhipProcessor |
| `api-server` | Runtime basics, wait_for_signal |
| `api-server-demo` | API server processor |
| `tokio-integration` | Runtime with external tokio |
