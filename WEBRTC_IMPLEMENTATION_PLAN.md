# WebRTC WHIP Streaming Implementation Plan

## Overview

Implement WebRTC streaming with WHIP (WebRTC-HTTP Ingestion Protocol) support for streaming to Cloudflare. This implementation uses a **monolithic processor with trait-based modularity** approach - all functionality lives in a single `WebRtcWhipProcessor`, but internal components are abstracted behind traits for testability and future flexibility.

**Related Issue:** [#7 - Implement WebRTC transport with WHIP/WHEP](https://github.com/tato123/streamlib/issues/7)

**Key Design Decisions:**
- Single processor initially (avoid premature optimization)
- Trait-based internal components (modularity + testability)
- One PR per trait/phase (incremental progress)
- Apple platform (VideoToolbox) first, other platforms later
- Test against Cloudflare Stream via WHIP

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│         WebRtcWhipProcessor (Monolithic)                │
│                                                         │
│  ┌──────────────────┐  ┌──────────────────┐           │
│  │ VideoEncoderH264 │  │ AudioEncoderOpus │           │
│  │     (trait)      │  │     (trait)      │           │
│  └──────────────────┘  └──────────────────┘           │
│           │                     │                      │
│           ▼                     ▼                      │
│  ┌────────────────────────────────────┐               │
│  │   RtpTimestampCalculator (util)    │               │
│  └────────────────────────────────────┘               │
│           │                     │                      │
│           ▼                     ▼                      │
│  ┌──────────────────┐  ┌──────────────────┐           │
│  │  WebRtcSession   │  │  WhipSignaling   │           │
│  │     (trait)      │  │     (trait)      │           │
│  └──────────────────┘  └──────────────────┘           │
│           │                     │                      │
│           └──────────┬──────────┘                      │
│                      ▼                                 │
│              Cloudflare Stream                         │
└─────────────────────────────────────────────────────────┘

Inputs: VideoFrame (raw from camera)
        AudioFrame (raw PCM)

Output: Live stream via WHIP to Cloudflare
```

## Phases & Pull Requests

Each phase corresponds to **one trait** with its implementation and tests. Each phase should be a **separate PR**.

---

## Phase 1: Video Encoding (VideoToolbox H.264)

**Goal:** Implement H.264 video encoding using Apple's VideoToolbox framework.

**Files to create/modify:**
- `libs/streamlib/src/encoding/video_encoder.rs` - Trait definition
- `libs/streamlib/src/apple/encoding/videotoolbox_h264.rs` - VideoToolbox implementation
- `libs/streamlib/src/encoding/mod.rs` - Module exports

**Trait Definition:**
```rust
pub trait VideoEncoderH264: Send {
    fn encode(&mut self, frame: &VideoFrame) -> Result<EncodedVideoFrame>;
    fn force_keyframe(&mut self);
    fn config(&self) -> &VideoEncoderConfig;
    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()>;
}
```

**Key Types:**
```rust
pub struct VideoEncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
    pub keyframe_interval_frames: u32,
    pub profile: H264Profile,
    pub low_latency: bool,
}

pub struct EncodedVideoFrame {
    pub data: Vec<u8>,
    pub timestamp_ns: i64,
    pub is_keyframe: bool,
    pub presentation_timestamp: i64,
    pub decode_timestamp: i64,
}

pub enum H264Profile {
    Baseline,
    Main,
    High,
}
```

**Implementation Requirements:**
- Use VideoToolbox `VTCompressionSession` API
- Configure for low-latency streaming (no B-frames)
- Support CBR (constant bitrate) for predictable streaming
- Preserve MediaClock timestamps from input frames
- Output H.264 in Annex B format (start codes)
- First frame must be keyframe
- Respect keyframe interval configuration

**Testing:**
- Unit test: Encode synthetic/test frame, verify output
- Test: Force keyframe works
- Test: Timestamp preservation
- Test: Bitrate changes apply
- Integration test: Encode 30 frames, verify keyframes appear at interval

**Success Criteria:**
- ✅ Can encode VideoFrame to H.264
- ✅ First frame is keyframe
- ✅ Timestamps preserved
- ✅ Keyframe forcing works
- ✅ All tests pass

**Dependencies:**
- `core-video` crate for VideoToolbox APIs
- `core-foundation` crate

**References:**
- VideoToolbox documentation: https://developer.apple.com/documentation/videotoolbox
- Existing `CameraProcessor` for VideoToolbox usage patterns

---

## Phase 2: Audio Encoding (Opus)

**Goal:** Implement Opus audio encoding for low-latency streaming.

**Files to create/modify:**
- `libs/streamlib/src/encoding/audio_encoder.rs` - Trait definition
- `libs/streamlib/src/encoding/opus_encoder.rs` - libopus implementation
- Add `opus` crate dependency to `Cargo.toml`

**Trait Definition:**
```rust
pub trait AudioEncoderOpus: Send {
    fn encode(&mut self, frame: &AudioFrame) -> Result<EncodedAudioFrame>;
    fn config(&self) -> &AudioEncoderConfig;
    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()>;
}
```

**Key Types:**
```rust
pub struct AudioEncoderConfig {
    pub sample_rate: u32,      // 48000 for Opus
    pub channels: u16,         // 1 or 2
    pub bitrate_bps: u32,
    pub frame_duration_ms: u32, // 20ms typical
    pub complexity: u32,       // 0-10
    pub vbr: bool,
}

pub struct EncodedAudioFrame {
    pub data: Vec<u8>,
    pub timestamp_ns: i64,
    pub sample_count: usize,
    pub duration_ns: i64,
}
```

**Implementation Requirements:**
- Use `opus` crate (bindings to libopus)
- Default: 48kHz, stereo, 128kbps, 20ms frames
- Handle input that may not be exactly 20ms (buffer/rechunk)
- Preserve MediaClock timestamps from input frames
- Support VBR and CBR modes
- Complexity 10 for best quality (CPU permitting)

**Testing:**
- Unit test: Encode test PCM data, verify Opus output
- Test: Timestamp preservation
- Test: Bitrate changes apply
- Test: Handle various input buffer sizes
- Integration test: Encode 1 second of audio, verify packet count

**Success Criteria:**
- ✅ Can encode AudioFrame to Opus
- ✅ Timestamps preserved
- ✅ Handles 20ms frame chunking
- ✅ Bitrate configuration works
- ✅ All tests pass

**Dependencies:**
- `opus` crate (https://crates.io/crates/opus)
- May need `libopus-sys` for native linking

**References:**
- Opus codec: https://opus-codec.org/
- RFC 7587: RTP Payload Format for Opus
- Existing `AudioResamplerProcessor` for audio handling patterns

---

## Phase 3: RTP Timestamp Calculation

**Goal:** Implement utility for converting MediaClock timestamps to RTP timestamps.

**Files to create/modify:**
- `libs/streamlib/src/webrtc/rtp_timestamp.rs` - Calculator implementation
- `libs/streamlib/src/webrtc/mod.rs` - Module exports

**Implementation:**
```rust
pub struct RtpTimestampCalculator {
    start_time_ns: i64,
    rtp_base: u32,
    clock_rate: u32,
}

impl RtpTimestampCalculator {
    pub fn new(start_time_ns: i64, clock_rate: u32) -> Self;
    pub fn calculate(&self, timestamp_ns: i64) -> u32;
}
```

**Requirements:**
- Random `rtp_base` for security (prevents timestamp prediction)
- Handle u32 wrapping correctly
- Support 90kHz clock (video) and 48kHz clock (audio)
- Precision: nanosecond MediaClock → RTP units conversion

**Testing:**
- Test: Zero elapsed time → rtp_base
- Test: Known elapsed time → correct RTP offset
- Test: Wrapping around u32::MAX
- Test: Different clock rates (90kHz, 48kHz)

**Success Criteria:**
- ✅ Accurate timestamp conversion
- ✅ Handles wrapping
- ✅ Works for both audio and video clock rates
- ✅ All tests pass

**Dependencies:**
- `rand` crate for random `rtp_base`

**References:**
- RFC 3550 (RTP): Section 5.1 (RTP Fixed Header Fields)
- `docs/webrtc_considerations.md`: Section 2 (RTP Timestamp Calculation)

---

## Phase 4: WHIP Signaling Client

**Goal:** Implement WHIP protocol for HTTP-based WebRTC signaling.

**Files to create/modify:**
- `libs/streamlib/src/webrtc/whip.rs` - Trait definition + HTTP implementation
- Add `reqwest` crate dependency to `Cargo.toml`

**Trait Definition:**
```rust
pub trait WhipSignaling: Send {
    fn post_offer(&mut self, offer: &SessionDescription) -> Result<SessionDescription>;
    fn send_ice_candidate(&mut self, candidate: &IceCandidate) -> Result<()>;
    fn terminate(&mut self) -> Result<()>;
    fn session_info(&self) -> Option<&WhipSessionInfo>;
}
```

**Key Types:**
```rust
pub struct WhipConfig {
    pub endpoint_url: String,
    pub auth_token: String,
    pub timeout_ms: u64,
}

pub struct WhipSessionInfo {
    pub session_url: String,    // From Location header
    pub endpoint_url: String,
}

// Re-export or wrap webrtc crate types
pub struct SessionDescription { /* SDP */ }
pub struct IceCandidate { /* candidate string */ }
```

**Implementation Requirements:**
- HTTP POST to WHIP endpoint with SDP offer
  - Content-Type: `application/sdp`
  - Authorization: `Bearer {token}`
- Parse response: SDP answer + Location header (session URL)
- HTTP PATCH to session URL for trickle ICE candidates
  - Content-Type: `application/trickle-ice-sdpfrag`
- HTTP DELETE to session URL to terminate
- Proper error handling (network, auth, malformed responses)
- Timeout handling

**Testing:**
- Unit test with mock HTTP server (use `mockito` or `wiremock`)
- Test: POST offer returns answer
- Test: PATCH sends ICE candidate
- Test: DELETE terminates session
- Test: Auth token in headers
- Test: Error handling (404, 401, timeout)

**Success Criteria:**
- ✅ Can POST offer and receive answer
- ✅ Can PATCH ICE candidates
- ✅ Can DELETE session
- ✅ Proper error handling
- ✅ All tests pass

**Dependencies:**
- `reqwest` crate for HTTP client
- `mockito` or `wiremock` for testing

**References:**
- WHIP RFC: https://datatracker.ietf.org/doc/html/draft-ietf-wish-whip
- Cloudflare Stream WHIP: https://developers.cloudflare.com/stream/webrtc/

---

## Phase 5: WebRTC Session Management

**Goal:** Wrap `webrtc` crate v0.14.0 behind a trait for session/track management.

**Files to create/modify:**
- `libs/streamlib/src/webrtc/session.rs` - Trait definition + webrtc-rs wrapper
- Add `webrtc` crate v0.14.0 dependency to `Cargo.toml`

**Trait Definition:**
```rust
pub trait WebRtcSession: Send {
    fn create_offer(&mut self) -> Result<SessionDescription>;
    fn set_local_description(&mut self, desc: SessionDescription) -> Result<()>;
    fn set_remote_description(&mut self, desc: SessionDescription) -> Result<()>;
    fn write_video_sample(&mut self, sample: RtpSample) -> Result<()>;
    fn write_audio_sample(&mut self, sample: RtpSample) -> Result<()>;
    fn connection_state(&self) -> ConnectionState;
    fn poll_events(&mut self) -> Vec<WebRtcEvent>;
}
```

**Key Types:**
```rust
pub struct RtpSample {
    pub data: Vec<u8>,
    pub timestamp: u32,      // RTP timestamp (already converted)
    pub duration: u32,
}

pub enum WebRtcEvent {
    IceCandidate(IceCandidate),
    ConnectionStateChanged(ConnectionState),
    Error(String),
}

pub enum ConnectionState {
    New, Connecting, Connected, Disconnected, Failed, Closed,
}

pub struct WebRtcConfig {
    pub video_codec: VideoCodecParams,
    pub audio_codec: AudioCodecParams,
    pub stun_servers: Vec<String>,
    pub turn_servers: Vec<TurnServer>,
}

pub struct VideoCodecParams {
    pub codec: VideoCodec,
    pub payload_type: u8,
    pub clock_rate: u32,
    pub parameters: HashMap<String, String>,
}

pub struct AudioCodecParams {
    pub codec: AudioCodec,
    pub payload_type: u8,
    pub clock_rate: u32,
    pub channels: u16,
    pub parameters: HashMap<String, String>,
}
```

**Implementation Requirements (`WebRtcRsSession`):**
- Create `RTCPeerConnection` from `webrtc` crate
- Add H.264 video track with proper SDP parameters
  - `profile-level-id=42e01f` (Baseline 3.1)
  - `packetization-mode=1`
  - Payload type 96, clock rate 90000
- Add Opus audio track
  - Payload type 111, clock rate 48000, channels 2
- Handle ICE candidate gathering
- Write samples to tracks via `write_sample()` API
- Poll for connection state changes
- Configure STUN servers (e.g., `stun:stun.l.google.com:19302`)

**Testing:**
- Test: Can create offer with video + audio tracks
- Test: Can set local/remote descriptions
- Test: Can write samples to tracks (may need mock)
- Test: Connection state transitions
- Test: ICE candidate events
- Integration test with loopback (if feasible)

**Success Criteria:**
- ✅ Can create WebRTC peer connection
- ✅ Can add video and audio tracks
- ✅ Can generate SDP offer
- ✅ Can write RTP samples
- ✅ Events poll correctly
- ✅ All tests pass

**Dependencies:**
- `webrtc` crate v0.14.0
- `tokio` for async runtime (webrtc crate is async)

**References:**
- `webrtc` crate docs: https://docs.rs/webrtc/
- `webrtc` examples: https://github.com/webrtc-rs/webrtc/tree/master/examples
- RFC 6184: RTP Payload Format for H.264

---

## Phase 6: WebRtcWhipProcessor Integration

**Goal:** Compose all traits into the final monolithic processor.

**Files to create/modify:**
- `libs/streamlib/src/apple/processors/webrtc_whip.rs` - Main processor
- `examples/webrtc_whip_cloudflare.rs` - Example usage

**Processor Implementation:**
```rust
pub struct WebRtcWhipProcessor {
    // Inputs
    video_in: StreamInput<VideoFrame>,
    audio_in: StreamInput<AudioFrame>,

    // Components (trait objects)
    video_encoder: Box<dyn VideoEncoderH264>,
    audio_encoder: Box<dyn AudioEncoderOpus>,
    whip_client: Box<dyn WhipSignaling>,
    webrtc_session: Box<dyn WebRtcSession>,

    // RTP calculators
    video_rtp_calc: Option<RtpTimestampCalculator>,
    audio_rtp_calc: Option<RtpTimestampCalculator>,

    // State
    session_started: bool,
    start_time_ns: Option<i64>,
}

impl Processor for WebRtcWhipProcessor {
    fn process(&mut self) -> Result<()> { /* ... */ }
}
```

**Requirements:**
- Implement `Processor` trait
- Wait for both audio and video before starting session
- Initialize RTP timestamp calculators on session start
- Encode video → calculate RTP timestamp → write to WebRTC
- Encode audio → calculate RTP timestamp → write to WebRTC
- Poll WebRTC events, send ICE candidates via WHIP
- Handle connection state changes
- Graceful shutdown (DELETE WHIP session)

**Configuration:**
- Builder pattern for easy construction
- Default configs optimized for low-latency streaming
- Cloudflare endpoint + auth token from env or config

**Testing:**
- Unit test: Mock all traits, verify orchestration
- Integration test: Stream to Cloudflare Stream
  - Verify stream is live
  - Play in browser, verify audio/video sync
  - Measure glass-to-glass latency (<100ms target)

**Success Criteria:**
- ✅ Processor successfully starts WHIP session
- ✅ Video and audio both stream
- ✅ Playback in browser shows synchronized A/V
- ✅ Latency <100ms glass-to-glass
- ✅ No crashes or errors during 1min streaming test

**Example Usage:**
```rust
// examples/webrtc_whip_cloudflare.rs
fn main() -> Result<()> {
    let video_encoder = Box::new(VideoToolboxH264Encoder::new(video_config)?);
    let audio_encoder = Box::new(LibopusEncoder::new(audio_config)?);
    let whip_client = Box::new(HttpWhipClient::new(whip_config)?);
    let webrtc_session = Box::new(WebRtcRsSession::new(webrtc_config)?);

    let processor = WebRtcWhipProcessor::new(
        video_input,
        audio_input,
        video_encoder,
        audio_encoder,
        whip_client,
        webrtc_session,
    );

    pipeline.add_processor(processor);
    pipeline.run()?;

    Ok(())
}
```

**Dependencies:**
- All previous phases

**References:**
- `docs/webrtc_considerations.md` - Full architecture discussion
- Existing processors for `Processor` trait patterns

---

## Phase 7: Documentation & Polish

**Goal:** Document usage, add metrics, improve error handling.

**Tasks:**
- Update README with WebRTC/WHIP example
- Add metrics/observability
  - Encoder latency
  - RTP packet rate
  - RTCP statistics
  - Connection state duration
- Improve error messages
- Add logging (tracing)
- Performance profiling
- Memory profiling

**Success Criteria:**
- ✅ Clear documentation for users
- ✅ Observable metrics for debugging
- ✅ Helpful error messages

---

## Instructions for Claude Planning Agent

When working on a specific phase:

1. **Reference this document:** `/Users/fonta/Repositories/tatolab/streamlib/WEBRTC_IMPLEMENTATION_PLAN.md`

2. **Phase structure:** Each phase is self-contained with:
   - Goal: What to accomplish
   - Files: What to create/modify
   - Requirements: Implementation details
   - Testing: What tests to write
   - Success Criteria: Definition of done
   - Dependencies: What's needed
   - References: Where to look for help

3. **Working on a phase:** When asked to implement "Phase X":
   - Read the phase section in this document
   - Create the files specified
   - Implement the trait and its implementation
   - Write the tests described
   - Verify success criteria are met

4. **Example prompts:**
   - "Implement Phase 1: Video Encoding"
   - "Continue with Phase 2: Audio Encoding"
   - "Review my implementation of Phase 3"

5. **Cross-references:**
   - See `docs/webrtc_considerations.md` for deep-dive on WebRTC vs MP4
   - See existing processors in `libs/streamlib/src/apple/processors/` for patterns
   - See `libs/streamlib/src/core/media_clock.rs` for timestamp handling
   - See issue #7: https://github.com/tato123/streamlib/issues/7

6. **PR workflow:**
   - One PR per phase
   - Each PR should be independently reviewable and testable
   - PR title: "Phase X: [Phase Name]"
   - PR description should reference this plan and the phase number

7. **Testing strategy:**
   - Unit tests for each trait implementation
   - Integration tests where appropriate
   - Final E2E test in Phase 6 (streaming to Cloudflare)

8. **Key principles:**
   - Preserve MediaClock timestamps through the pipeline
   - Low-latency configuration (no B-frames, minimal buffering)
   - Proper error handling and logging
   - Follow existing code patterns in the codebase

---

## Configuration Defaults

### Video Encoding (VideoToolbox H.264)
```rust
VideoEncoderConfig {
    width: 1280,
    height: 720,
    fps: 30,
    bitrate_bps: 2_500_000,        // 2.5 Mbps
    keyframe_interval_frames: 60,  // 2 seconds at 30fps
    profile: H264Profile::Main,
    low_latency: true,             // No B-frames
}
```

### Audio Encoding (Opus)
```rust
AudioEncoderConfig {
    sample_rate: 48000,
    channels: 2,
    bitrate_bps: 128_000,          // 128 kbps
    frame_duration_ms: 20,         // Standard low-latency
    complexity: 10,                // Max quality
    vbr: true,                     // Variable bitrate
}
```

### WHIP Configuration
```rust
WhipConfig {
    endpoint_url: "https://customer-<id>.cloudflarestream.com/<stream-id>/webrtc/publish".to_string(),
    auth_token: env::var("CLOUDFLARE_STREAM_TOKEN")?,
    timeout_ms: 10000,
}
```

### WebRTC Configuration
```rust
WebRtcConfig {
    video_codec: VideoCodecParams {
        codec: VideoCodec::H264,
        payload_type: 96,
        clock_rate: 90000,
        parameters: hashmap! {
            "profile-level-id" => "42e01f",  // Baseline 3.1
            "packetization-mode" => "1",
        },
    },
    audio_codec: AudioCodecParams {
        codec: AudioCodec::Opus,
        payload_type: 111,
        clock_rate: 48000,
        channels: 2,
        parameters: hashmap! {
            "minptime" => "10",
            "useinbandfec" => "1",
        },
    },
    stun_servers: vec![
        "stun:stun.l.google.com:19302".to_string(),
    ],
    turn_servers: vec![],  // Not needed for WHIP typically
}
```

---

## Timeline Estimate

Assuming one phase per PR, with review time:

- Phase 1 (VideoToolbox): 2-3 days
- Phase 2 (Opus): 1-2 days
- Phase 3 (RTP Timestamps): 0.5-1 day
- Phase 4 (WHIP): 1-2 days
- Phase 5 (WebRTC Session): 2-3 days
- Phase 6 (Integration): 2-3 days
- Phase 7 (Polish): 1-2 days

**Total: ~2-3 weeks** for full implementation

---

## Future Extensions (Out of Scope)

- WHEP (receiving streams)
- Dynamic bitrate adaptation (congestion control)
- Android platform support (MediaCodec)
- Software encoder fallbacks (OpenH264, libvpx)
- Recording while streaming
- Multiple simultaneous streams
- SFU/MCU server support

---

## Questions & Decisions Log

Track key decisions and open questions here as implementation progresses.

### Q1: Should we use Annex B or AVCC format for H.264?
**Decision:** Annex B (start codes) - more compatible with RTP packetization in webrtc-rs.

### Q2: Do we need trickle ICE or can we skip it?
**Decision:** Implement it, but it may not be critical for WHIP (server typically doesn't send candidates). Start with POST-only, add PATCH if needed.

### Q3: Async vs sync processor implementation?
**Decision:** TBD based on webrtc-rs API requirements (it's async). May need to spawn tokio runtime within processor or make processor async.

### Q4: How to handle encoder initialization latency?
**Decision:** TBD - may need to buffer initial frames or signal when encoder is ready.

---

**Document Version:** 1.0
**Last Updated:** 2025-11-16
**Owner:** @tato123
**Status:** Planning → Ready for Implementation
