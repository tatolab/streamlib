# WebRTC Streaming Considerations

This document outlines important considerations for implementing WebRTC/RTP streaming in streamlib, particularly how it differs from MP4 file writing and what needs to be handled differently.

## Background: MP4 vs WebRTC

### What AVAssetWriter Handles (MP4 File Writing)

AVAssetWriter (used in `Mp4WriterProcessor`) handles:

1. **Container format** (MP4/MOV atoms/boxes)
2. **Interleaving** audio and video samples in the file
3. **Index creation** (moov atom) for seekability
4. **Timestamp to presentation time conversion** (relative to session start)

**Key characteristics**:
- Writes to a **file** where entire content is available after writing
- Player can seek/buffer as needed
- Timestamps are relative to session start (t=0)
- Missing frames just create gaps in playback
- Both audio and video streams are tightly synchronized in the container

### What WebRTC/RTP Requires (Real-time Streaming)

WebRTC is fundamentally different - it's **real-time, low-latency streaming** over unreliable networks:

- Audio and video sent as **separate RTP streams**
- Each stream has its own sequence numbers and timestamps
- Receiver uses **RTP timestamps** and **RTCP Sender Reports** to synchronize
- No file container - just raw packetized streams
- Must handle packet loss, jitter, and network congestion

## Critical Differences

### 1. Synchronization Approach

**MP4Writer (file-based)**:
```rust
// Both streams have timestamps relative to session start
// File format keeps them interleaved
// Player handles sync during playback
audio_timestamp_relative = audio.timestamp_ns - start_time_ns;
video_timestamp_relative = video.timestamp_ns - start_time_ns;
```

**WebRTC (streaming)**:
```rust
// Audio RTP packets have audio RTP timestamps (increments by sample count)
// Video RTP packets have video RTP timestamps (increments by 90kHz clock)
// Receiver uses RTCP to align the two clocks
audio_rtp_ts += samples_in_packet;  // e.g., +960 for 20ms @ 48kHz
video_rtp_ts += 90000 / fps;        // e.g., +3000 for 30fps
```

### 2. RTP Timestamp Calculation

RTP timestamps must be **monotonic** and **continuous**. We can leverage our existing `MediaClock` timestamps:

**Audio RTP Timestamps** (sample rate clock, e.g., 48kHz):
```rust
// Convert MediaClock nanoseconds to RTP timestamp units (sample rate)
let audio_rtp_ts = self.audio_rtp_base +
    ((audio_frame.timestamp_ns - self.start_time_ns) * AUDIO_SAMPLE_RATE / 1_000_000_000) as u32;
```

**Video RTP Timestamps** (90kHz clock - standard for video):
```rust
// Convert MediaClock nanoseconds to 90kHz RTP timestamp units
let video_rtp_ts = self.video_rtp_base +
    ((video_frame.timestamp_ns - self.start_time_ns) * 90000 / 1_000_000_000) as u32;
```

**Important**: The `rtp_base` values should be randomized at session start for security (prevents RTP timestamp prediction).

### 3. RTCP Sender Reports (SR)

Send periodic RTCP Sender Reports to allow receiver-side synchronization:

```rust
// Send RTCP SR for audio stream (every ~5 seconds)
let ntp_timestamp = MediaClock::now().as_nanos();
let rtcp_sr = RtcpSenderReport {
    ssrc: audio_ssrc,
    ntp_timestamp,           // MediaClock timestamp (wall time reference)
    rtp_timestamp: audio_rtp_ts,  // Current audio RTP timestamp
    packet_count: audio_packet_count,
    octet_count: audio_byte_count,
};

// Send RTCP SR for video stream
let rtcp_sr = RtcpSenderReport {
    ssrc: video_ssrc,
    ntp_timestamp,           // Same MediaClock reference
    rtp_timestamp: video_rtp_ts,  // Current video RTP timestamp
    packet_count: video_packet_count,
    octet_count: video_byte_count,
};
```

The receiver uses these RTCP SRs to:
- Map RTP timestamps to wall clock time (NTP)
- Calculate inter-stream timing (audio vs video offset)
- Synchronize playback of both streams

### 4. Audio Frame Dropping - Do We Need It?

**MP4Writer**: Yes, we drop audio frames that arrive before the first video frame
- Camera initialization delay (~800ms) meant audio buffered while waiting
- We synchronized session start to first video frame
- Had to drop early audio frames to align streams at t=0 in the file

**WebRTC**: Probably not, for different reasons
- Start streaming when both audio and video are ready
- Receiver handles slight timing differences via jitter buffer
- Both streams should start near-simultaneously in real-time
- If there's a delay, it's communicated via RTP timestamps and RTCP SRs

**However**, you still need to handle:
- Initial buffering until both streams are available
- Timestamp alignment (both streams use same `start_time_ns` reference)
- Graceful handling if one stream starts before the other

## What We Can Reuse from MP4Writer

✅ **MediaClock timestamps** - Already monotonic (`mach_absolute_time()`), perfect for RTP
✅ **Resampler logic** - Still need to match audio sample rates (likely to Opus @ 48kHz)
✅ **Timestamp calculation approach** - Convert `MediaClock → RTP timestamps`
✅ **Synchronization understanding** - Same principles apply

❌ **Audio frame dropping logic** - Probably not needed if we start when ready
❌ **"Session start at t=0" concept** - RTP timestamps can start at any random value
❌ **Container/interleaving logic** - No MP4 container, just raw packetized streams

## What WebRTC Libraries Handle

If using a WebRTC library (like `webrtc-rs` or `pion` via FFI):

**Library handles**:
- RTP packetization (splitting into MTU-sized packets)
- RTCP feedback (receiver reports, NACKs for retransmission)
- Jitter buffering on receiver side
- Network congestion control (bandwidth estimation)
- DTLS/SRTP encryption
- ICE/STUN/TURN for NAT traversal

**You still need to provide**:
- Encoded video frames (H.264 NAL units)
- Encoded audio frames (Opus packets, typically 20ms frames)
- Proper RTP timestamps (calculated from MediaClock)
- RTCP Sender Reports for synchronization
- Codec parameters (SDP negotiation)

## Practical Implementation Example

### WebRTC Pipeline Architecture

```rust
// Video path:
CameraProcessor (MediaClock timestamp)
  → VideoEncoderProcessor (H.264)
  → RtpPacketizerProcessor (MediaClock → RTP timestamp @ 90kHz)
  → WebRtcSenderProcessor

// Audio path:
AudioCaptureProcessor (MediaClock timestamp)
  → AudioResamplerProcessor (to 48kHz if needed)
  → AudioEncoderProcessor (Opus)
  → RtpPacketizerProcessor (MediaClock → RTP timestamp @ 48kHz)
  → WebRtcSenderProcessor

// Synchronization:
// Both paths periodically send RTCP SR with:
// - RTP timestamp (their respective clocks: 48kHz for audio, 90kHz for video)
// - NTP timestamp (from MediaClock)
// → Receiver uses this to align audio/video clocks
```

### Example RTP Packetizer Processor

```rust
pub struct RtpVideoPacketizerProcessor {
    video_in: StreamInput<EncodedVideoFrame>,
    rtp_out: Arc<StreamOutput<RtpPacket>>,

    // State
    rtp_base_timestamp: u32,      // Random starting point
    start_time_ns: i64,            // From MediaClock
    sequence_number: u16,          // Increments for each packet
    ssrc: u32,                     // Random stream identifier
}

impl RtpVideoPacketizerProcessor {
    fn process(&mut self) -> Result<()> {
        if let Some(frame) = self.video_in.read() {
            // Convert MediaClock timestamp to RTP timestamp (90kHz)
            let elapsed_ns = frame.timestamp_ns - self.start_time_ns;
            let rtp_timestamp = self.rtp_base_timestamp.wrapping_add(
                ((elapsed_ns * 90000) / 1_000_000_000) as u32
            );

            // Packetize H.264 frame into RTP packets (handle MTU)
            for packet in self.packetize_h264(&frame.data, rtp_timestamp) {
                self.rtp_out.write(packet);
                self.sequence_number = self.sequence_number.wrapping_add(1);
            }

            // Send RTCP SR periodically (every ~5 seconds)
            if self.should_send_rtcp_sr() {
                self.send_rtcp_sr(rtp_timestamp);
            }
        }
        Ok(())
    }
}
```

## Key Takeaways

1. **MediaClock is your foundation** - The work we did to use `MediaClock::now()` for both audio and video provides the monotonic timestamps needed for proper RTP timing

2. **Different clock rates for RTP** - Audio uses sample rate (e.g., 48kHz), video uses 90kHz standard clock

3. **RTCP SR is critical** - This is how receivers synchronize audio and video streams

4. **No frame dropping needed** - Unlike MP4 where we synced to first video frame, WebRTC starts when ready and communicates timing via RTP timestamps

5. **Network awareness** - Unlike file writing, must handle packet loss, jitter, and congestion

6. **Codec selection** - Typical WebRTC stack uses Opus for audio (48kHz) and H.264/VP8/VP9/AV1 for video

## References

- RFC 3550: RTP: A Transport Protocol for Real-Time Applications
- RFC 3551: RTP Profile for Audio and Video Conferences
- RFC 6184: RTP Payload Format for H.264 Video
- RFC 7587: RTP Payload Format for Opus Speech and Audio Codec
- WebRTC specification: https://www.w3.org/TR/webrtc/

## Related Files

- `libs/streamlib/src/apple/processors/mp4_writer.rs` - MP4 writing implementation
- `libs/streamlib/src/apple/processors/camera.rs` - Video capture with MediaClock timestamps
- `libs/streamlib/src/apple/processors/audio_capture.rs` - Audio capture with MediaClock timestamps
- `libs/streamlib/src/apple/media_clock.rs` - Monotonic clock implementation
- `libs/streamlib/src/core/processors/audio_resampler.rs` - Audio resampling (needed for Opus)
