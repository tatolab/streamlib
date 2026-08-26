// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared iceoryx2 payload types for cross-process IPC communication.
//!
//! A [`FrameHeader`] carries routing and framing only — port key, timestamp,
//! payload length. Nothing on this wire names a type, so no read path can
//! compare one: a consumer handed a payload it cannot read discovers that as
//! a decode failure of the payload itself, at its own read.
//!
//! Alongside the payload types, this crate owns the channel rules the host
//! and every language native must agree on byte-for-byte or silently drift:
//! [`decide_channel_egress_admission`] with its
//! [`emit_channel_egress_admission_tracing`] diagnostics (so the crate does
//! emit `tracing`), and [`next_read_required_len`], the peek rule over a
//! native's local receive queue.

use iceoryx2::prelude::*;

/// The initial iceoryx2 slot capacity every channel publisher is primed at.
///
/// This is a HINT, never a cap. Publishers open under
/// [`iceoryx2::prelude::AllocationStrategy::PowerOfTwo`]; the first loan larger
/// than the primed capacity grows the shared-memory segment and subscribers
/// remap transparently. Sizing the hint to the common-case payload keeps the
/// steady state at a single segment while leaving oversized frames (a first
/// multi-MB keyframe) free to grow rather than crash.
pub const DEFAULT_EXPECTED_PAYLOAD_BYTES: usize = 65536;
pub const MAX_PORT_KEY_SIZE: usize = 64;
pub const MAX_EVENT_PAYLOAD_SIZE: usize = 8192;
pub const MAX_TOPIC_KEY_SIZE: usize = 128;

/// Per-channel payload ceiling for a trusted (in-process host) data channel —
/// the graceful, observable layer in front of the subprocess cgroup
/// `memory.max` hard backstop. A payload above this is refused with a named
/// `PayloadExceedsChannelCeiling` error, counted, and the stream continues;
/// the process never dies.
pub const TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES: usize = 64 * 1024 * 1024;

/// Per-channel payload ceiling for an untrusted-session (subprocess) data
/// channel. Tighter than the trusted tier because a subprocess payload crosses
/// a trust boundary and a runaway producer must be bounded well below host RAM.
pub const UNTRUSTED_SESSION_CHANNEL_PAYLOAD_CEILING_BYTES: usize = 16 * 1024 * 1024;

/// Trust tier of an iceoryx2 data channel, selecting the default per-channel
/// payload ceiling.
///
/// Determined structurally by the process boundary at wire time: an in-process
/// host link is [`ChannelTrustTier::Trusted`]; a link crossing a subprocess
/// boundary is [`ChannelTrustTier::UntrustedSession`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelTrustTier {
    /// In-process host-to-host channel.
    Trusted,
    /// Channel with a subprocess (Python / Deno) on either end.
    UntrustedSession,
}

impl ChannelTrustTier {
    /// The default per-channel payload ceiling in bytes for this tier.
    pub const fn default_ceiling_bytes(self) -> usize {
        match self {
            Self::Trusted => TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
            Self::UntrustedSession => UNTRUSTED_SESSION_CHANNEL_PAYLOAD_CEILING_BYTES,
        }
    }

    /// Stable lowercase label used in the channel-egress tracing fields.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::UntrustedSession => "untrusted-session",
        }
    }
}

/// A PowerOfTwo data-segment growth event a channel publisher observed while
/// admitting a frame: the tracked slot capacity crossed the frame size and was
/// advanced from `old_segment_bytes` to `new_segment_bytes` (`next_power_of_two`).
///
/// `crossed_quarter_ceiling` is `true` when this growth is the one that first
/// pushed the segment past a quarter of the channel's ceiling (`old <= ceiling/4
/// < new`) — the early-warning threshold every runtime raises a `tracing::warn`
/// on. The threshold lives here, alongside the growth bookkeeping, so the host
/// writer and the Python / Deno subprocess natives cannot drift on where it sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelSegmentGrowth {
    /// Tracked slot capacity before this growth, in bytes.
    pub old_segment_bytes: usize,
    /// Tracked slot capacity after this growth (`next_power_of_two`), in bytes.
    pub new_segment_bytes: usize,
    /// Whether this growth first crossed a quarter of the channel ceiling.
    pub crossed_quarter_ceiling: bool,
}

/// Outcome of [`decide_channel_egress_admission`]: whether the frame a channel
/// publisher is about to loan should be published or dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelEgressAdmission {
    /// The frame is above the channel's per-channel payload ceiling and was
    /// refused. The caller drops it — surfacing the refusal in its own way (a
    /// typed `PayloadExceedsChannelCeiling` error in the host, a refuse return
    /// code in a subprocess native) — and logs it; `refused_count` is the
    /// running total after this refusal.
    RefusedOverCeiling { refused_count: u64 },
    /// The frame fits under the ceiling; the caller publishes it. When `grew_to`
    /// is `Some(growth)` the tracked data-segment capacity crossed the frame size
    /// and was advanced — a PowerOfTwo growth the caller logs, additionally
    /// raising a `warn` when [`ChannelSegmentGrowth::crossed_quarter_ceiling`].
    Admitted {
        grew_to: Option<ChannelSegmentGrowth>,
    },
}

/// Single authority for the per-channel-egress ceiling refusal + PowerOfTwo
/// growth-observability bookkeeping every channel publisher runs before loaning
/// a frame.
///
/// Refusing above `channel_ceiling_bytes` is the graceful, observable layer in
/// front of the subprocess cgroup `memory.max` backstop. This crate owns the
/// thresholds so the host writer and the Python / Deno subprocess natives cannot
/// drift: it increments `refused_over_ceiling_count` on a refusal, advances
/// `current_slot_capacity_bytes` to the next power of two on a growth (both in
/// place), and reports whether that growth first crossed a quarter of the
/// ceiling. The caller owns the refusal surface (typed error vs. refuse return
/// code); the shared diagnostics live in
/// [`emit_channel_egress_admission_tracing`] beside this decision so they cannot
/// drift from it.
pub fn decide_channel_egress_admission(
    frame_total_bytes: usize,
    channel_ceiling_bytes: usize,
    refused_over_ceiling_count: &mut u64,
    current_slot_capacity_bytes: &mut usize,
) -> ChannelEgressAdmission {
    if frame_total_bytes > channel_ceiling_bytes {
        *refused_over_ceiling_count += 1;
        return ChannelEgressAdmission::RefusedOverCeiling {
            refused_count: *refused_over_ceiling_count,
        };
    }
    let grew_to = if frame_total_bytes > *current_slot_capacity_bytes {
        let old_segment_bytes = *current_slot_capacity_bytes;
        let new_segment_bytes = frame_total_bytes.next_power_of_two();
        *current_slot_capacity_bytes = new_segment_bytes;
        let quarter_ceiling_bytes = channel_ceiling_bytes / 4;
        Some(ChannelSegmentGrowth {
            old_segment_bytes,
            new_segment_bytes,
            crossed_quarter_ceiling: new_segment_bytes > quarter_ceiling_bytes
                && old_segment_bytes <= quarter_ceiling_bytes,
        })
    } else {
        None
    };
    ChannelEgressAdmission::Admitted { grew_to }
}

/// Emit the channel-egress admission tracing shared by the host writer and the
/// helper-process SDK natives' output-write path, so every writer stays
/// lock-step on the refusal / segment-growth / quarter-of-ceiling diagnostics
/// off the same [`decide_channel_egress_admission`] decision. `trust_tier`
/// labels each line; `log_prefix` is `None` for the host and
/// `Some((runtime_tag, processor_id))` for a native (its runtime tag plus its
/// processor id) to scope the message with a `[tag:id] ` prefix. The caller
/// still maps [`ChannelEgressAdmission::RefusedOverCeiling`] to its own refuse
/// return code or typed error.
pub fn emit_channel_egress_admission_tracing(
    log_prefix: Option<(&str, &str)>,
    trust_tier: ChannelTrustTier,
    channel_service_name: &str,
    channel_ceiling_bytes: usize,
    payload_total_bytes: usize,
    admission: &ChannelEgressAdmission,
) {
    let prefix = match log_prefix {
        Some((runtime_tag, processor_id)) => format!("[{}:{}] ", runtime_tag, processor_id),
        None => String::new(),
    };

    match admission {
        ChannelEgressAdmission::RefusedOverCeiling { refused_count } => {
            tracing::warn!(
                channel = channel_service_name,
                payload_bytes = payload_total_bytes,
                ceiling_bytes = channel_ceiling_bytes,
                tier = trust_tier.as_str(),
                refused_count = *refused_count,
                "{}output channel refused a payload above its per-channel ceiling",
                prefix,
            );
        }
        ChannelEgressAdmission::Admitted { grew_to } => {
            if let Some(growth) = grew_to {
                tracing::info!(
                    channel = channel_service_name,
                    old_segment_bytes = growth.old_segment_bytes,
                    new_segment_bytes = growth.new_segment_bytes,
                    tier = trust_tier.as_str(),
                    "{}iceoryx2 publisher data segment grew (PowerOfTwo)",
                    prefix,
                );
                if growth.crossed_quarter_ceiling {
                    tracing::warn!(
                        channel = channel_service_name,
                        segment_bytes = growth.new_segment_bytes,
                        ceiling_bytes = channel_ceiling_bytes,
                        tier = trust_tier.as_str(),
                        "{}iceoryx2 publisher segment crossed a quarter of the channel ceiling",
                        prefix,
                    );
                }
            }
        }
    }
}

/// Byte length of the frame a read would return next from a native SDK's local
/// `pending` receive queue, so every native shares one peek rule.
/// `read_next_in_order` selects the FIFO front; otherwise the SkipToLatest
/// newest. `None` when the queue is empty. The caller compares the returned
/// length against its receive buffer to decide whether to grow before
/// consuming the frame.
pub fn next_read_required_len(queue: &[(Vec<u8>, i64)], read_next_in_order: bool) -> Option<usize> {
    let next = if read_next_in_order {
        queue.first()
    } else {
        queue.last()
    };
    next.map(|(frame, _)| frame.len())
}

/// Default iceoryx2 ring depth (slot count, not bytes) for the data
/// pub/sub channel between two processors.
///
/// iceoryx2 pre-allocates `DEFAULT_MAX_QUEUED_MESSAGES * (primed slot bytes)`
/// of shared memory per publisher, so this value is a per-publisher memory
/// commitment too. The slot bytes are primed from
/// [`DEFAULT_EXPECTED_PAYLOAD_BYTES`] and grow on demand.
pub const DEFAULT_MAX_QUEUED_MESSAGES: usize = 16;

/// Publishers on a channel-centric iceoryx2 pub/sub data service.
///
/// A channel is keyed on its **source output port** — one source port publishes
/// to exactly one channel — so every data service carries exactly ONE publisher
/// and fans a single zero-copy loan out to its N compile-time-known subscribers.
/// iceoryx2 verifies the publisher count on `open`, so this is pinned in lockstep
/// on the host service builder and both subprocess SDK builders (Python, Deno);
/// a per-runtime divergence would itself break cross-language wiring.
pub const MAX_PUBLISHERS_PER_CHANNEL: usize = 1;

/// Subscriber slots reserved on every channel data service beyond its
/// compile-time-known destination count.
///
/// A channel's data service is created with `max_subscribers = N + this`, where
/// `N` is the number of destinations wired to the source output port at
/// graph-compile time. The reserved slot lets the phase-3.5 `tap` op attach a
/// broadcast consumer as a pure subscriber-add with no service re-open — iceoryx2
/// fixes `max_subscribers` at create time, so the headroom must exist up front.
/// iceoryx2 sizes each publisher's shared-memory data segment as
/// `max_subscribers × (subscriber_max_buffer_size + borrowed) + …`, so this is
/// deliberately 1 (not the iceoryx2 default of 8) to keep the per-channel segment
/// sized to its true consumer count plus one tap.
pub const RESERVED_TAP_SUBSCRIBER_SLOTS_PER_CHANNEL: usize = 1;

/// Size of the frame header in the `[u8]` slice wire format.
pub const FRAME_HEADER_SIZE: usize = MAX_PORT_KEY_SIZE + 8 + 4; // 76 bytes

/// Error constructing a [`PortKey`] from a name that overflows the fixed
/// wire capacity.
///
/// This is the engine-layer replacement for the pre-#1416 silent truncation:
/// a port / channel name longer than [`MAX_PORT_KEY_SIZE`] `- 1` bytes used to
/// be quietly clipped, routing frames to a different (truncated) port than the
/// one the author named. Over-length is now a hard, named error the caller
/// must handle rather than a data-corruption surface. Names crossing this
/// boundary have already passed the charset + length grammar in
/// the engine's `iceoryx2::validate_channel_name`; this guard is the wire-level
/// backstop that makes truncation unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortKeyError {
    /// The UTF-8 name is `len` bytes, past the fixed `max`-byte capacity.
    TooLong { len: usize, max: usize },
}

impl std::fmt::Display for PortKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLong { len, max } => write!(
                f,
                "port key name is {len} bytes, exceeding the fixed wire capacity of {max} bytes"
            ),
        }
    }
}

impl std::error::Error for PortKeyError {}

/// Fixed-size port name for zero-copy IPC.
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug, ZeroCopySend)]
#[repr(C)]
pub struct PortKey {
    len: u8,
    name: [u8; MAX_PORT_KEY_SIZE - 1],
}

// The `len` field is one wire byte, so the name capacity must stay addressable
// by it: widening `MAX_PORT_KEY_SIZE` past 256 would silently wrap the lengths
// this type narrows to `u8` rather than fail to compile.
const _: () = assert!(PortKey::MAX_NAME_BYTES <= u8::MAX as usize);

impl PortKey {
    /// Maximum UTF-8 byte length a port / channel name may occupy on the wire
    /// (the fixed `name` field is [`MAX_PORT_KEY_SIZE`] `- 1` bytes).
    pub const MAX_NAME_BYTES: usize = MAX_PORT_KEY_SIZE - 1;

    /// The wire name-length prefix, or `None` when it exceeds the capacity of
    /// the fixed `name` field it indexes.
    ///
    /// Rejecting rather than saturating. A clamp is safe to *slice* but not safe
    /// to *route*: it reconstructs exactly [`PortKey::MAX_NAME_BYTES`] bytes of
    /// name, which is the name of a port declared at the full width — so a
    /// corrupt frame aimed at one would land in its mailbox rather than miss.
    /// A prefix past capacity cannot have come from [`PortKey::new`], so the
    /// frame is malformed and its port is not recoverable.
    fn wire_name_len_prefix_within_capacity(len_prefix: u8) -> Option<u8> {
        (usize::from(len_prefix) <= Self::MAX_NAME_BYTES).then_some(len_prefix)
    }

    /// Construct a [`PortKey`], rejecting an over-length name.
    ///
    /// A name past [`PortKey::MAX_NAME_BYTES`] is a [`PortKeyError::TooLong`]
    /// rather than a silent truncation — see [`PortKeyError`].
    pub fn new(name: &str) -> Result<Self, PortKeyError> {
        let bytes = name.as_bytes();
        if bytes.len() > Self::MAX_NAME_BYTES {
            return Err(PortKeyError::TooLong {
                len: bytes.len(),
                max: Self::MAX_NAME_BYTES,
            });
        }
        let mut key = Self {
            len: bytes.len() as u8,
            name: [0u8; MAX_PORT_KEY_SIZE - 1],
        };
        key.name[..bytes.len()].copy_from_slice(bytes);
        Ok(key)
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.name[..self.len as usize]).unwrap_or("")
    }
}

impl Default for PortKey {
    fn default() -> Self {
        Self {
            len: 0,
            name: [0u8; MAX_PORT_KEY_SIZE - 1],
        }
    }
}

/// Header for slice-based iceoryx2 frame transport.
///
/// Wire format in a `[u8]` slice (little-endian for multi-byte fields):
/// `[port_key: 64][timestamp_ns: 8][len: 4][data: len]`
///
/// Routing and framing only — the header names no type, so a subscriber can
/// route a frame and bound its payload without knowing what is in it.
pub struct FrameHeader {
    pub port_key: PortKey,
    pub timestamp_ns: i64,
    pub len: u32,
}

impl FrameHeader {
    /// Create a new frame header for `port`.
    ///
    /// Fails with [`PortKeyError`] if `port` overflows the fixed wire capacity —
    /// see [`PortKey::new`].
    pub fn new(port: &str, timestamp_ns: i64, data_len: u32) -> Result<Self, PortKeyError> {
        Ok(Self {
            port_key: PortKey::new(port)?,
            timestamp_ns,
            len: data_len,
        })
    }

    /// Write the header to the first [`FRAME_HEADER_SIZE`] bytes of `buf`.
    pub fn write_to_slice(&self, buf: &mut [u8]) {
        debug_assert!(buf.len() >= FRAME_HEADER_SIZE);
        // port_key: [len: 1][name: 63] = 64 bytes
        buf[0] = self.port_key.len;
        buf[1..MAX_PORT_KEY_SIZE].copy_from_slice(&self.port_key.name);
        // timestamp_ns: 8 bytes little-endian
        let t = MAX_PORT_KEY_SIZE;
        buf[t..t + 8].copy_from_slice(&self.timestamp_ns.to_le_bytes());
        // len: 4 bytes little-endian
        buf[t + 8..t + 12].copy_from_slice(&self.len.to_le_bytes());
    }

    /// Read a header from the first [`FRAME_HEADER_SIZE`] bytes of `buf`.
    ///
    /// A name-length prefix past [`PortKey::MAX_NAME_BYTES`] reads as the empty
    /// port, which matches no mailbox.
    pub fn read_from_slice(buf: &[u8]) -> Self {
        debug_assert!(buf.len() >= FRAME_HEADER_SIZE);
        let mut port_key = PortKey {
            len: PortKey::wire_name_len_prefix_within_capacity(buf[0]).unwrap_or(0),
            ..Default::default()
        };
        port_key.name.copy_from_slice(&buf[1..MAX_PORT_KEY_SIZE]);

        let t = MAX_PORT_KEY_SIZE;
        let timestamp_ns = i64::from_le_bytes(buf[t..t + 8].try_into().unwrap());
        let len = u32::from_le_bytes(buf[t + 8..t + 12].try_into().unwrap());

        Self {
            port_key,
            timestamp_ns,
            len,
        }
    }

    /// Read the port key string from a raw slice without parsing the full header.
    ///
    /// Checked on both wire-derived lengths — the prefix against
    /// [`PortKey::MAX_NAME_BYTES`], the resulting span against `buf` — so a
    /// malformed frame reads as the empty port, which matches no mailbox,
    /// instead of panicking or naming a port it was not stamped for.
    pub fn read_port_from_slice(buf: &[u8]) -> &str {
        let Some(&len_prefix) = buf.first() else {
            return "";
        };
        let Some(len) = PortKey::wire_name_len_prefix_within_capacity(len_prefix) else {
            return "";
        };
        buf.get(1..1 + usize::from(len))
            .and_then(|name| std::str::from_utf8(name).ok())
            .unwrap_or("")
    }

    /// Read the payload a frame stamps, without trusting either length.
    ///
    /// `None` for a slice too short to hold a header at all, and for one whose
    /// stamped length runs past what actually followed it. The second is the
    /// dangerous case: the leading bytes of a truncated frame are a
    /// well-formed shorter message in every self-describing wire format, so a
    /// reader that sliced to the end of the buffer would hand back a payload
    /// the sender never wrote, silently.
    ///
    /// The tail past the stamped length is not padding to trim — a slice off
    /// the wire is fixed-capacity, so it holds whatever an earlier, larger
    /// frame left behind.
    pub fn read_payload_from_slice(buf: &[u8]) -> Option<&[u8]> {
        let body = buf.get(FRAME_HEADER_SIZE..)?;
        body.get(..Self::read_from_slice(buf).len as usize)
    }

    /// Get the port key as a string.
    pub fn port(&self) -> &str {
        self.port_key.as_str()
    }
}

/// Fixed-size topic name for event pub/sub IPC.
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug, ZeroCopySend)]
#[repr(C)]
pub struct TopicKey {
    len: u8,
    name: [u8; MAX_TOPIC_KEY_SIZE - 1],
}

impl TopicKey {
    pub fn new(name: &str) -> Self {
        let bytes = name.as_bytes();
        let len = bytes.len().min(MAX_TOPIC_KEY_SIZE - 1) as u8;
        let mut key = Self {
            len,
            name: [0u8; MAX_TOPIC_KEY_SIZE - 1],
        };
        key.name[..len as usize].copy_from_slice(&bytes[..len as usize]);
        key
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.name[..self.len as usize]).unwrap_or("")
    }
}

impl Default for TopicKey {
    fn default() -> Self {
        Self {
            len: 0,
            name: [0u8; MAX_TOPIC_KEY_SIZE - 1],
        }
    }
}

/// Event payload for iceoryx2 pub/sub communication.
///
/// Carries serialized runtime events (lifecycle, graph changes, compiler, input)
/// between components via iceoryx2 shared memory.
#[derive(Clone, Copy, ZeroCopySend)]
#[type_name("EventPayload")]
#[repr(C)]
pub struct EventPayload {
    pub topic_key: TopicKey,
    pub timestamp_ns: i64,
    pub len: u32,
    pub data: [u8; MAX_EVENT_PAYLOAD_SIZE],
}

impl EventPayload {
    /// Create a new event payload with the given topic and serialized data.
    pub fn new(topic: &str, timestamp_ns: i64, data: &[u8]) -> Self {
        let len = data.len().min(MAX_EVENT_PAYLOAD_SIZE) as u32;
        let mut payload = Self {
            topic_key: TopicKey::new(topic),
            timestamp_ns,
            len,
            data: [0u8; MAX_EVENT_PAYLOAD_SIZE],
        };
        payload.data[..len as usize].copy_from_slice(&data[..len as usize]);
        payload
    }

    /// Get the actual data slice (excluding padding).
    pub fn data(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }

    /// Get the topic key as a string.
    pub fn topic(&self) -> &str {
        self.topic_key.as_str()
    }
}

impl Default for EventPayload {
    fn default() -> Self {
        Self {
            topic_key: TopicKey::default(),
            timestamp_ns: 0,
            len: 0,
            data: [0u8; MAX_EVENT_PAYLOAD_SIZE],
        }
    }
}

impl std::fmt::Debug for EventPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventPayload")
            .field("topic_key", &self.topic_key.as_str())
            .field("timestamp_ns", &self.timestamp_ns)
            .field("len", &self.len)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_read_required_len_picks_front_or_back_by_read_mode() {
        let queue: Vec<(Vec<u8>, i64)> =
            vec![(vec![0u8; 4], 1), (vec![0u8; 16], 2), (vec![0u8; 64], 3)];
        assert_eq!(next_read_required_len(&queue, true), Some(4));
        assert_eq!(next_read_required_len(&queue, false), Some(64));
        assert_eq!(next_read_required_len(&[], true), None);
        assert_eq!(next_read_required_len(&[], false), None);
    }

    #[test]
    fn frame_header_round_trip_via_slice() {
        let header = FrameHeader::new("dest_port", 42, 1024).unwrap();
        let mut buf = [0u8; FRAME_HEADER_SIZE];
        header.write_to_slice(&mut buf);
        let back = FrameHeader::read_from_slice(&buf);
        assert_eq!(back.port(), "dest_port");
        assert_eq!(back.timestamp_ns, 42);
        assert_eq!(back.len, 1024);
    }

    #[test]
    fn frame_header_round_trips_the_extremes_of_every_field() {
        // A negative timestamp is representable and must survive: the data
        // plane stamps CLOCK_MONOTONIC, and a reader comparing two stamps can
        // legitimately carry a value below its own epoch.
        let longest_port = "z".repeat(PortKey::MAX_NAME_BYTES);
        for (port, timestamp_ns, len) in [
            ("", i64::MIN, 0u32),
            ("p", -1, 1),
            (longest_port.as_str(), i64::MAX, u32::MAX),
        ] {
            let mut buf = [0u8; FRAME_HEADER_SIZE];
            FrameHeader::new(port, timestamp_ns, len)
                .expect("port fits the wire capacity")
                .write_to_slice(&mut buf);
            let back = FrameHeader::read_from_slice(&buf);
            assert_eq!(back.port(), port);
            assert_eq!(back.timestamp_ns, timestamp_ns);
            assert_eq!(back.len, len);
        }
    }

    #[test]
    fn frame_header_size_matches_constant() {
        // [PortKey: 64][i64: 8][u32: 4] = 76 bytes.
        assert_eq!(FRAME_HEADER_SIZE, 64 + 8 + 4);
        assert_eq!(FRAME_HEADER_SIZE, 76);
    }

    #[test]
    fn frame_header_fields_sit_at_their_documented_wire_offsets() {
        // The layout regression test engine doctrine requires of anything
        // crossing the IPC wire. `FrameHeader` is not `#[repr(C)]` — it is
        // packed field by field by `write_to_slice` — so the contract is the
        // byte offsets in the serialized slice, not the Rust struct layout.
        // Every publisher and subscriber in the graph agrees on these offsets
        // and there is no version field to negotiate a disagreement on: a
        // shift here is silent cross-process corruption, not a compile error.
        let mut buf = [0u8; FRAME_HEADER_SIZE];
        FrameHeader::new("cam", 0x0102_0304_0506_0708, 0x0A0B_0C0D)
            .unwrap()
            .write_to_slice(&mut buf);

        assert_eq!(buf[0], 3, "port_key len prefix at offset 0");
        assert_eq!(&buf[1..4], b"cam", "port_key name bytes start at offset 1");
        assert!(
            buf[4..64].iter().all(|&b| b == 0),
            "the port_key name field is zero-padded through offset 63"
        );
        assert_eq!(
            i64::from_le_bytes(buf[64..72].try_into().unwrap()),
            0x0102_0304_0506_0708,
            "timestamp_ns is 8 bytes little-endian at 64..72"
        );
        assert_eq!(
            u32::from_le_bytes(buf[72..76].try_into().unwrap()),
            0x0A0B_0C0D,
            "len is 4 bytes little-endian at 72..76"
        );
    }

    #[test]
    fn channel_trust_tier_defaults_and_labels() {
        assert_eq!(
            ChannelTrustTier::Trusted.default_ceiling_bytes(),
            TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES
        );
        assert_eq!(
            ChannelTrustTier::UntrustedSession.default_ceiling_bytes(),
            UNTRUSTED_SESSION_CHANNEL_PAYLOAD_CEILING_BYTES
        );
        assert!(
            ChannelTrustTier::UntrustedSession.default_ceiling_bytes()
                < ChannelTrustTier::Trusted.default_ceiling_bytes(),
            "untrusted-session ceiling must be tighter than trusted"
        );
        assert_eq!(ChannelTrustTier::Trusted.as_str(), "trusted");
        assert_eq!(
            ChannelTrustTier::UntrustedSession.as_str(),
            "untrusted-session"
        );
    }

    #[test]
    fn port_key_accepts_max_length_name() {
        // Exact-fit boundary: a name of MAX_NAME_BYTES must construct.
        let name = "a".repeat(PortKey::MAX_NAME_BYTES);
        let key = PortKey::new(&name).expect("max-length name must construct");
        assert_eq!(key.as_str(), name);
    }

    #[test]
    fn port_key_rejects_over_length_name_instead_of_truncating() {
        // Mental-revert guard for the pre-#1416 silent truncation: a name one
        // byte past the wire capacity must be a named error, NOT a clipped key
        // that routes frames to the wrong port. Revert `PortKey::new` to the
        // `.min(MAX_PORT_KEY_SIZE - 1)` truncation and this fails — the
        // construction would succeed and `as_str()` would return the clipped
        // 63-byte prefix.
        let over = "b".repeat(PortKey::MAX_NAME_BYTES + 1);
        assert_eq!(over.len(), 64);
        assert_eq!(
            PortKey::new(&over),
            Err(PortKeyError::TooLong { len: 64, max: 63 })
        );
    }

    /// A header-sized frame carrying `payload_bytes` of a recognizable filler,
    /// stamped for `port` — the shape every read site is handed off the wire.
    fn frame_with_payload_filler(port: &str, payload_bytes: usize, filler: u8) -> Vec<u8> {
        let mut frame = vec![filler; FRAME_HEADER_SIZE + payload_bytes];
        FrameHeader::new(port, 7, payload_bytes as u32)
            .expect("port fits the wire capacity")
            .write_to_slice(&mut frame[..FRAME_HEADER_SIZE]);
        frame
    }

    #[test]
    fn read_payload_from_slice_stops_at_the_stamped_length() {
        // The filler stands in for an earlier, larger frame's leftovers: the
        // slice is longer than the payload, and everything past it is slack.
        let mut frame = frame_with_payload_filler("cam", 8, 0xAB);
        frame.resize(FRAME_HEADER_SIZE + 64, 0xCD);

        assert_eq!(
            FrameHeader::read_payload_from_slice(&frame),
            Some(&[0xABu8; 8][..]),
            "reading past the stamped length appends the slice's slack to the payload"
        );
    }

    #[test]
    fn read_payload_from_slice_refuses_a_frame_cut_short() {
        let frame = frame_with_payload_filler("cam", 32, 0xAB);

        assert_eq!(
            FrameHeader::read_payload_from_slice(&frame[..frame.len() - 1]),
            None,
            "a stamped length past the bytes that followed it is a truncated frame, \
             never a shorter one"
        );
    }

    #[test]
    fn read_payload_from_slice_refuses_bytes_that_cannot_hold_a_header() {
        assert_eq!(FrameHeader::read_payload_from_slice(&[0u8; 8]), None);
        assert_eq!(FrameHeader::read_payload_from_slice(&[]), None);
    }

    /// A zero-length payload is a real frame, not an absent one — `None` is
    /// reserved for a slice that cannot answer the question.
    #[test]
    fn read_payload_from_slice_reads_an_empty_payload_as_empty() {
        let frame = frame_with_payload_filler("cam", 0, 0);

        assert_eq!(FrameHeader::read_payload_from_slice(&frame), Some(&[][..]));
    }

    #[test]
    fn read_from_slice_rejects_an_over_capacity_port_key_len_prefix() {
        // The length prefix is one untrusted byte indexing a 63-byte field, so
        // it reaches 255 while the field cannot. Unclamped it lands in
        // `PortKey::len` and `as_str` slices past the field: "range end index
        // 255 out of range for slice of length 63".
        let mut frame = frame_with_payload_filler("cam", 0, 0);
        frame[0] = 0xFF;

        let header = FrameHeader::read_from_slice(&frame);
        assert_eq!(
            header.port(),
            "",
            "an over-capacity prefix names no port at all — clamping it to the \
             field width would reconstruct the name of a max-width port"
        );

        // Same path, well-formed prefix: routing is untouched.
        let well_formed = frame_with_payload_filler("cam", 0, 0);
        assert_eq!(FrameHeader::read_from_slice(&well_formed).port(), "cam");
    }

    #[test]
    fn read_port_from_slice_rejects_an_over_capacity_len_prefix() {
        // Same prefix, the peek path: on a header-sized frame the unclamped
        // slice runs off the buffer itself — "range end index 256 out of range
        // for slice of length 76".
        let mut frame = frame_with_payload_filler("cam", 0, 0);
        assert_eq!(frame.len(), FRAME_HEADER_SIZE);
        frame[0] = 0xFF;

        assert_eq!(
            FrameHeader::read_port_from_slice(&frame),
            "",
            "the peek path must reject the prefix too"
        );

        let well_formed = frame_with_payload_filler("cam", 0, 0);
        assert_eq!(FrameHeader::read_port_from_slice(&well_formed), "cam");
    }

    #[test]
    fn read_port_from_slice_never_reads_payload_bytes_as_the_port_name() {
        // The harder half of the same defect. Give the over-long prefix a frame
        // big enough to absorb it and there is no panic to notice — the read
        // walks past the 63-byte name field into the payload and returns those
        // bytes as the port name. That is silent misrouting: a frame delivered
        // to a mailbox its header never named.
        //
        // The spanned bytes must stay valid UTF-8 or `unwrap_or("")` masks the
        // defect and this passes vacuously — hence an ASCII filler, a prefix the
        // frame is long enough to absorb, and a payload length whose
        // little-endian header bytes all sit below 0x80.
        const FILLER: u8 = b'X';
        let mut frame = frame_with_payload_filler("cam", 100, FILLER);
        frame[0] = 150;

        let port = FrameHeader::read_port_from_slice(&frame);
        assert!(
            !port.as_bytes().contains(&FILLER),
            "the port name must never be read out of the payload, got {port:?}"
        );
        assert_eq!(port, "", "an over-capacity prefix names no port at all");

        let well_formed = frame_with_payload_filler("cam", 100, FILLER);
        assert_eq!(FrameHeader::read_port_from_slice(&well_formed), "cam");
    }

    #[test]
    fn egress_admission_refuses_over_ceiling_and_counts() {
        let ceiling = 128 * 1024usize;
        let mut refused = 0u64;
        let mut slot = 64usize;
        // First over-ceiling frame: refused, count → 1, slot untouched.
        assert_eq!(
            decide_channel_egress_admission(ceiling + 1, ceiling, &mut refused, &mut slot),
            ChannelEgressAdmission::RefusedOverCeiling { refused_count: 1 }
        );
        assert_eq!(slot, 64, "a refusal must not grow the tracked slot");
        // Second over-ceiling frame: count keeps climbing.
        assert_eq!(
            decide_channel_egress_admission(ceiling + 999, ceiling, &mut refused, &mut slot),
            ChannelEgressAdmission::RefusedOverCeiling { refused_count: 2 }
        );
    }

    #[test]
    fn egress_admission_grows_without_crossing_quarter_ceiling() {
        let ceiling = 128 * 1024usize; // quarter = 32 KiB
        let mut refused = 0u64;
        let mut slot = 4096usize;
        // A frame that grows the slot but stays at or below the quarter ceiling
        // (32 KiB) must NOT flag a crossing. 20_000 → next_pow2 = 32_768 == quarter.
        match decide_channel_egress_admission(20_000, ceiling, &mut refused, &mut slot) {
            ChannelEgressAdmission::Admitted {
                grew_to: Some(growth),
            } => {
                assert_eq!(growth.old_segment_bytes, 4096);
                assert_eq!(growth.new_segment_bytes, 32_768);
                assert!(
                    !growth.crossed_quarter_ceiling,
                    "new == ceiling/4 is not yet past the quarter — must not warn"
                );
            }
            other => panic!("expected an Admitted growth, got {other:?}"),
        }
        assert_eq!(slot, 32_768, "the slot advances to next_power_of_two");
        assert_eq!(refused, 0);
    }

    #[test]
    fn egress_admission_flags_the_growth_that_crosses_quarter_ceiling() {
        // Mental-revert guard for the quarter-ceiling early warning: this is the
        // single authority the host writer + Python/Deno natives all read the
        // `crossed_quarter_ceiling` flag from, so the threshold can't drift across
        // the three call sites. Drop the `> quarter && old <= quarter` computation
        // and this crossing goes unflagged — no runtime raises the warn.
        let ceiling = 128 * 1024usize; // quarter = 32 KiB = 32_768
        let mut refused = 0u64;
        let mut slot = 4096usize;
        // 40_000 → next_pow2 = 65_536, which is past the 32_768 quarter while the
        // old 4096 slot was under it: exactly the first crossing.
        match decide_channel_egress_admission(40_000, ceiling, &mut refused, &mut slot) {
            ChannelEgressAdmission::Admitted {
                grew_to: Some(growth),
            } => {
                assert_eq!(growth.old_segment_bytes, 4096);
                assert_eq!(growth.new_segment_bytes, 65_536);
                assert!(
                    growth.crossed_quarter_ceiling,
                    "old <= ceiling/4 < new must flag the quarter-ceiling crossing"
                );
            }
            other => panic!("expected an Admitted growth, got {other:?}"),
        }

        // A subsequent still-larger growth does NOT re-flag — the segment already
        // sits past the quarter, so only the FIRST crossing warns.
        match decide_channel_egress_admission(100_000, ceiling, &mut refused, &mut slot) {
            ChannelEgressAdmission::Admitted {
                grew_to: Some(growth),
            } => assert!(
                !growth.crossed_quarter_ceiling,
                "a growth already above the quarter must not re-flag"
            ),
            other => panic!("expected an Admitted growth, got {other:?}"),
        }
    }

    #[test]
    fn egress_admission_admits_within_slot_without_growth() {
        let ceiling = 128 * 1024usize;
        let mut refused = 0u64;
        let mut slot = 65_536usize;
        // A frame at or under the tracked slot neither grows nor flags.
        assert_eq!(
            decide_channel_egress_admission(4096, ceiling, &mut refused, &mut slot),
            ChannelEgressAdmission::Admitted { grew_to: None }
        );
        assert_eq!(
            slot, 65_536,
            "an in-slot frame leaves the tracked slot as-is"
        );
        assert_eq!(refused, 0);
    }

    #[test]
    fn frame_header_rejects_over_length_port() {
        // The truncation defect surfaced through FrameHeader::new on the write
        // path — over-length must propagate as the typed error, not silently
        // build a header with a clipped port key.
        let over = "c".repeat(PortKey::MAX_NAME_BYTES + 1);
        assert!(matches!(
            FrameHeader::new(&over, 0, 0),
            Err(PortKeyError::TooLong { .. })
        ));
    }
}
