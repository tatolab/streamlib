# WebRTC WHIP Streaming to Cloudflare Stream

This example demonstrates real-time video and audio streaming to Cloudflare Stream using the WebRTC WHIP protocol.

## What It Does

This example creates a complete streaming pipeline:

```
Camera (1280x720 @ 30fps)
  ↓
  └─→ H.264 Baseline Encoder (2.5 Mbps)
       ↓
       └─→ WebRTC WHIP Streamer ─→ Cloudflare Stream
            ↑
Microphone (mono @ 24kHz)
  ↓
  └─→ Resampler (24kHz → 48kHz)
       ↓
       └─→ Channel Converter (mono → stereo)
            ↓
            └─→ Opus Encoder (128 kbps)

```

## Features

- **Low Latency**: ~50ms glass-to-glass latency via WebRTC
- **H.264 Baseline Profile**: Optimized for Cloudflare Stream compatibility
- **Opus Audio**: High-quality stereo audio at 48kHz
- **WHIP Signaling**: Standards-compliant WebRTC-HTTP Ingestion Protocol (RFC 9725)
- **Real-time Streaming**: Sub-second latency for live interactions

## Prerequisites

- macOS (for VideoToolbox H.264 encoder and AVFoundation)
- Camera and microphone permissions granted
- Network connectivity to Cloudflare

## Running the Example

### Step 1: Build the example

```bash
cargo build -p webrtc-cloudflare-stream
```

### Step 2: Run the streaming pipeline

```bash
cargo run -p webrtc-cloudflare-stream
```

The example will:
1. Request camera and microphone permissions
2. Initialize the streaming pipeline
3. Connect to Cloudflare Stream via WHIP
4. Start streaming live video and audio
5. Display the viewer URL

### Step 3: View your stream

Open your browser to:
```
https://customer-5xiy6nkciicmt85v.cloudflarestream.com/4e48912c1e10e84c9bab3777695145dbk0072e99f6ddb152545830a794d165fce
```

You should see your camera feed with audio streaming in real-time!

### Step 4: Stop streaming

Press `Ctrl+C` to gracefully stop the stream. The pipeline will:
- Close the WebRTC connection
- Send DELETE to WHIP endpoint
- Clean up all resources

## Configuration

The example uses these settings optimized for Cloudflare Stream:

### Video Encoding
- **Resolution**: 1280x720
- **Frame Rate**: 30 fps
- **Bitrate**: 2.5 Mbps
- **Profile**: H.264 Baseline Level 3.1 (42e01f)
- **Keyframe Interval**: Every 2 seconds (60 frames @ 30fps)
- **Low Latency**: Enabled (no B-frames)

### Audio Encoding
- **Sample Rate**: 48 kHz
- **Channels**: Stereo (2)
- **Bitrate**: 128 kbps
- **Codec**: Opus
- **Frame Duration**: 20ms (WebRTC standard)
- **Complexity**: 10 (maximum quality)

### WHIP Configuration
- **Endpoint**: Cloudflare Stream WHIP URL
- **Authentication**: None (endpoint configured without auth)
- **Timeout**: 10 seconds

## How It Works

### 1. Encoder Initialization
The `WebRtcWhipProcessor` initializes:
- VideoToolbox H.264 encoder (GPU-accelerated on macOS)
- Opus audio encoder (libopus)
- RTP timestamp calculators for A/V sync

### 2. WebRTC Session Setup
On first video+audio frame:
- Creates `RTCPeerConnection` with H.264 and Opus tracks
- Generates SDP offer
- POSTs offer to WHIP endpoint via HTTPS
- Receives SDP answer from Cloudflare
- Negotiates ICE candidates (PATCH to WHIP session URL)
- Establishes secure DTLS connection

### 3. Streaming Loop
For each frame:
- **Video**: Encode with VideoToolbox → RTP packetization → WebRTC track
- **Audio**: Encode with Opus → RTP packetization → WebRTC track
- Timestamps calculated from `MediaClock` for perfect A/V sync
- RTP packets sent over DTLS/SRTP to Cloudflare

### 4. Graceful Shutdown
On Ctrl+C:
- Closes WebRTC peer connection
- Sends DELETE to WHIP session URL
- Releases encoder resources
- Flushes network buffers

## Performance

Expected performance on Apple Silicon (M1/M2/M3):
- **Encoding Latency**: <5ms (VideoToolbox + Opus)
- **Network Latency**: ~10-30ms (depends on location)
- **Total Glass-to-Glass**: ~50-100ms

## Troubleshooting

### Stream Doesn't Start

1. Check camera/microphone permissions:
   ```
   System Settings → Privacy & Security → Camera/Microphone
   ```

2. Verify network connectivity to Cloudflare:
   ```bash
   curl -v https://customer-5xiy6nkciicmt85v.cloudflarestream.com
   ```

3. Check logs with debug output:
   ```bash
   RUST_LOG=debug cargo run -p webrtc-cloudflare-stream
   ```

### Poor Video Quality

Increase bitrate in `VideoEncoderConfig`:
```rust
bitrate_bps: 5_000_000,  // 5 Mbps instead of 2.5 Mbps
```

### Audio/Video Desync

Check A/V synchronization tolerance (should be <100ms):
```
Audio/Video delta: XXms
```

If consistently high, verify:
- Audio resampling quality (use `ResamplingQuality::High`)
- Network jitter (check ping times to Cloudflare)

## Advanced Usage

### Custom Camera

Specify a specific camera device:
```rust
CameraConfig {
    device_id: Some("0x1424001bcf2284".to_string()),
}
```

List available cameras:
```bash
cargo run -p camera-list  # (example not included, use system tools)
```

### Different Resolution

Modify `VideoEncoderConfig`:
```rust
width: 1920,
height: 1080,
fps: 60,
bitrate_bps: 8_000_000,  // 8 Mbps for 1080p60
```

### Different WHIP Endpoint

For other WHIP-compatible services (LiveKit, Janus, etc.):
```rust
WhipConfig {
    endpoint_url: "https://your-server.com/whip".to_string(),
    auth_token: Some("Bearer your-token".to_string()),
    timeout_ms: 10000,
}
```

## References

- [RFC 9725: WHIP Protocol](https://datatracker.ietf.org/doc/rfc9725/)
- [Cloudflare Stream WebRTC](https://developers.cloudflare.com/stream/webrtc/)
- [WebRTC Spec](https://www.w3.org/TR/webrtc/)
- [H.264 RTP Payload (RFC 6184)](https://datatracker.ietf.org/doc/html/rfc6184)
- [Opus Codec](https://opus-codec.org/)
