# Cloudflare Stream WHIP Configuration

This document shows how to configure streamlib to stream to Cloudflare Stream using WHIP.

## Cloudflare Stream Endpoint

**WHIP URL**: `https://customer-5xiy6nkciicmt85v.cloudflarestream.com/4e48912c1e10e84c9bab3777695145dbk0072e99f6ddb152545830a794d165fce/webRTC/publish`

**Authentication**: Not required (endpoint configured without authentication)

**Supported Codec**: H.264 Constrained Baseline Profile Level 3.1 (profile-level-id: `42e01f`)

## Configuration Example

```rust
use streamlib::{WebRtcWhipConfig, WhipConfig, VideoEncoderConfig, AudioEncoderConfig, H264Profile};

let config = WebRtcWhipConfig {
    whip: WhipConfig {
        endpoint_url: "https://customer-5xiy6nkciicmt85v.cloudflarestream.com/4e48912c1e10e84c9bab3777695145dbk0072e99f6ddb152545830a794d165fce/webRTC/publish".to_string(),
        auth_token: None, // No authentication required
        timeout_ms: 10000, // 10 second timeout for HTTP requests
    },
    video: VideoEncoderConfig {
        width: 1280,
        height: 720,
        fps: 30,
        bitrate_bps: 2_500_000, // 2.5 Mbps
        keyframe_interval_frames: 60, // Every 2 seconds @ 30fps
        profile: H264Profile::Baseline, // Constrained Baseline Profile
        low_latency: true,
    },
    audio: AudioEncoderConfig {
        sample_rate: 48000,
        channels: 2, // Stereo
        bitrate_bps: 128_000, // 128 kbps
        frame_duration_ms: 20, // 20ms frames (standard for WebRTC)
        complexity: 10, // Maximum quality
        vbr: false, // Constant bitrate for consistent streaming
    },
};
```

## WHIP Protocol Flow

The implementation follows RFC 9725:

1. **POST** (Offer/Answer Exchange):
   - Client sends SDP offer with H.264 Baseline 3.1 codec
   - Server responds with SDP answer (201 Created)
   - Server provides `Location` header with session URL

2. **PATCH** (Trickle ICE):
   - Client sends ICE candidates as discovered
   - Format: `application/trickle-ice-sdpfrag`
   - Multiple candidates batched per request

3. **DELETE** (Session Termination):
   - Client sends DELETE to session URL
   - Graceful shutdown of WebRTC connection

## Video Codec Configuration

Our encoder is configured to match Cloudflare's requirements:

### SDP Offer (RFC 6184 parameters):
```
a=rtpmap:96 H264/90000
a=fmtp:96 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f
```

- **profile-level-id=42e01f**:
  - `42` = Baseline Profile (66 decimal)
  - `e0` = Constrained Baseline constraint flags
  - `1f` = Level 3.1 (31 decimal)
- **packetization-mode=1**: Non-interleaved mode (WebRTC standard)
- **level-asymmetry-allowed=1**: Receiver can decode higher levels

### VideoToolbox Encoder:
- Profile: `kVTProfileLevel_H264_Baseline_3_1`
- Real-time encoding enabled
- Frame reordering disabled (no B-frames)
- Low latency optimizations

## Testing

Once the WebRTC processor is fully implemented, test with:

```bash
# Build library
cargo build --lib

# Run example (when created)
RUST_LOG=debug cargo run --example webrtc-cloudflare-stream
```

Expected behavior:
1. Establishes HTTPS connection to Cloudflare
2. Negotiates WebRTC session via WHIP
3. Streams H.264 video + Opus audio
4. Viewer can watch at: https://customer-5xiy6nkciicmt85v.cloudflarestream.com/4e48912c1e10e84c9bab3777695145dbk0072e99f6ddb152545830a794d165fce

## Troubleshooting

### Connection Issues
- Check network connectivity to `customer-5xiy6nkciicmt85v.cloudflarestream.com`
- Verify firewall allows HTTPS (443) and WebRTC ports

### Codec Mismatch
- Ensure H.264 profile is exactly `42e01f` in SDP
- Check VideoToolbox encoder logs for profile setting

### ICE Connectivity
- Check ICE candidate logs (`RUST_LOG=debug`)
- Verify STUN/TURN servers if behind restrictive NAT

## Authentication (Other Endpoints)

For WHIP endpoints that require authentication:

```rust
whip: WhipConfig {
    endpoint_url: "https://example.com/whip".to_string(),
    auth_token: Some("your-bearer-token-here".to_string()),
    timeout_ms: 10000,
}
```

The client will automatically add `Authorization: Bearer <token>` header to all requests.

## References

- [RFC 9725: WHIP Protocol](https://datatracker.ietf.org/doc/rfc9725/)
- [RFC 6184: H.264 RTP Payload](https://datatracker.ietf.org/doc/html/rfc6184)
- [RFC 8840: Trickle ICE](https://datatracker.ietf.org/doc/html/rfc8840)
- [Cloudflare Stream WebRTC Docs](https://developers.cloudflare.com/stream/webrtc-beta/)
