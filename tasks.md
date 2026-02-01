# Example Migration Tasks

Remaining examples to enable, ordered by priority (unique/untested components first).

## Priority Queue

| Priority | Example | New/Untested Components | Status |
|----------|---------|------------------------|--------|
| 1 | `camera-audio-recorder` | MP4WriterProcessor | ✅ Done |
| 2 | `microphone-reverb-speaker` | CLAP plugin hosting | ✅ Done |
| 3 | `whep-player` | WebRTC WHEP (inbound) | ⬚ TODO |
| 4 | `runtime-graph-json-demo` | Graph JSON serialization | ⬚ TODO |
| 5 | `simple-pipeline` | None (redundant) | ⬚ TODO |
| 6 | `graph-json-export` | None (old syntax rewrite) | ⬚ TODO |

## Blocked

- `camera-dylib-display` — uses old PyO3 `PythonContinuousHostProcessor` (needs rewrite to subprocess architecture)

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
| `microphone-reverb-speaker` | ClapEffectProcessor (Manual mode, deferred activation), AudioCaptureProcessor, AudioOutputProcessor |
