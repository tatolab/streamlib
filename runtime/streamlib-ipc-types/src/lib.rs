// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared iceoryx2 payload types for cross-process IPC communication.
//!
//! A [`DataChannelSampleUserHeader`] rides every channel sample in its
//! iceoryx2 user header — numbering, stamp, length, group flag and a
//! [`DataChannelPayloadDescriptor`] — and the payload area carries nothing but
//! payload. Nothing on this wire names a message type, so a consumer handed a
//! payload it cannot read discovers that as a decode failure of the payload
//! itself, at its own read.
//!
//! Alongside the payload types, this crate owns the channel rules the host
//! and every language native must agree on byte-for-byte or silently drift:
//! [`decide_channel_egress_admission`] with its
//! [`emit_channel_egress_admission_tracing`] diagnostics (so the crate does
//! emit `tracing`).

use iceoryx2::prelude::*;

mod gpu_pool_claim_table;
pub use gpu_pool_claim_table::{
    GPU_POOL_CLAIM_TABLE_BYTES, GPU_POOL_CLAIM_TABLE_CLAIMANT_COLUMNS,
    GPU_POOL_CLAIM_TABLE_RETIRING_GENERATION_BIT, GPU_POOL_CLAIM_TABLE_ROW_BYTES,
    GPU_POOL_CLAIM_TABLE_ROW_CLAIM_COUNTS_OFFSET,
    GPU_POOL_CLAIM_TABLE_ROW_PUBLISHED_GENERATION_OFFSET, GPU_POOL_CLAIM_TABLE_ROWS,
    GpuPoolClaimTableRow, GpuPoolClaimantColumn, GpuPoolFrameRecycled, GpuPoolSlotRehandTest,
};

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

/// Per-channel shared-memory chunk ceiling for a trusted (in-process host) data
/// channel — the graceful, observable layer in front of the subprocess cgroup
/// `memory.max` hard backstop. A frame whose sample is above this is refused
/// with a named `PayloadExceedsChannelCeiling` error, counted, and the stream
/// continues; the process never dies.
///
/// A power of two, because iceoryx2's pool allocator buckets a data segment at
/// `next_power_of_two` of the sample layout: at a power-of-two ceiling the chunk
/// a ceiling-sized bag takes is the ceiling itself, which is what
/// [`largest_channel_frame_bytes_under_a_chunk_ceiling`] subtracts the sample
/// overhead to reach. An operator override that is not a power of two still
/// bounds each bag; its chunk rounds up past the ceiling, and the engine says so
/// where the override is read.
pub const TRUSTED_CHANNEL_CHUNK_CEILING_BYTES: usize = 64 * 1024 * 1024;

/// Per-channel shared-memory chunk ceiling for an untrusted-session (subprocess)
/// data channel. Tighter than the trusted tier because a subprocess payload
/// crosses a trust boundary and a runaway producer must be bounded well below
/// host RAM.
pub const UNTRUSTED_SESSION_CHANNEL_CHUNK_CEILING_BYTES: usize = 16 * 1024 * 1024;

/// Bytes iceoryx2 lays out ahead of a channel frame in the shared-memory sample
/// that carries it: its own publish-subscribe header, this crate's
/// [`DataChannelSampleUserHeader`], and the padding that starts the payload at
/// the alignment of a [`DataChannelPayloadCacheLine`].
///
/// Mirrors `MessageTypeDetails::all_headers_len`, which is crate-private
/// upstream: the header, then the user header at its alignment, then the
/// payload at its. Composed from the real types rather than written out, so a
/// size change upstream moves this with it.
const ICEORYX2_SAMPLE_BYTES_AHEAD_OF_A_CHANNEL_FRAME: usize =
    (size_of::<iceoryx2::service::header::publish_subscribe::Header>()
        .next_multiple_of(align_of::<DataChannelSampleUserHeader>())
        + size_of::<DataChannelSampleUserHeader>())
    .next_multiple_of(align_of::<DataChannelPayloadCacheLine>());

/// The alignment iceoryx2 rounds a whole channel sample up to. Mirrors
/// `MessageTypeDetails::chunk_layout`, which takes the largest of the header's,
/// the user header's and the payload's.
const ICEORYX2_SAMPLE_ALIGNMENT_BYTES: usize = {
    let header_alignment = align_of::<iceoryx2::service::header::publish_subscribe::Header>();
    let user_header_alignment = align_of::<DataChannelSampleUserHeader>();
    let largest_header_alignment = if header_alignment > user_header_alignment {
        header_alignment
    } else {
        user_header_alignment
    };
    let payload_alignment = align_of::<DataChannelPayloadCacheLine>();
    if largest_header_alignment > payload_alignment {
        largest_header_alignment
    } else {
        payload_alignment
    }
};

/// The shared-memory sample a channel frame of `frame_total_bytes` occupies —
/// the layout iceoryx2's pool allocator buckets, never the frame alone.
///
/// The distinction is load-bearing at the ceiling: under
/// [`iceoryx2::prelude::AllocationStrategy::PowerOfTwo`] a segment grows to
/// `next_power_of_two` of THIS, so a frame sized to a power-of-two ceiling
/// exactly would take a bucket twice that size.
pub const fn iceoryx2_sample_bytes_for_a_channel_frame(frame_total_bytes: usize) -> usize {
    let unaligned = ICEORYX2_SAMPLE_BYTES_AHEAD_OF_A_CHANNEL_FRAME
        + data_channel_payload_cache_lines_for(frame_total_bytes)
            * DATA_CHANNEL_PAYLOAD_ALIGNMENT_BYTES;
    unaligned.next_multiple_of(ICEORYX2_SAMPLE_ALIGNMENT_BYTES)
}

/// The largest channel frame whose sample still fits inside `chunk_ceiling_bytes`,
/// or `0` where the ceiling cannot hold even an empty frame's sample headers.
///
/// What an output port admits, and the number a refusal reports: a producer can
/// act on "your bag must be at most N bytes", where the chunk ceiling itself
/// would leave it wondering why a bag of exactly that size was refused. The `0`
/// arm is reachable only through a per-tier env override set below the sample
/// headers, and it admits only an empty frame.
pub const fn largest_channel_frame_bytes_under_a_chunk_ceiling(
    chunk_ceiling_bytes: usize,
) -> usize {
    // Round the ceiling down to a whole sample alignment first: a frame is
    // admitted when its aligned-up sample fits, so the last admitted frame sits
    // at the largest aligned size at or below the ceiling.
    let largest_whole_sample =
        chunk_ceiling_bytes - (chunk_ceiling_bytes % ICEORYX2_SAMPLE_ALIGNMENT_BYTES);
    largest_whole_sample.saturating_sub(ICEORYX2_SAMPLE_BYTES_AHEAD_OF_A_CHANNEL_FRAME)
}

/// The shared-memory chunk iceoryx2 really gives a bag sized to the most
/// `chunk_ceiling_bytes` admits — the ceiling itself where that is a power of
/// two, and the next power of two above it where it is not.
///
/// `None` where the arithmetic would overflow, which only a ceiling within one
/// sample layout of `usize::MAX` reaches; a caller reports the chunk as unknown
/// rather than taking a panic from inside a diagnostic.
pub const fn chunk_bytes_a_ceiling_sized_bag_takes(chunk_ceiling_bytes: usize) -> Option<usize> {
    iceoryx2_sample_bytes_for_a_channel_frame(largest_channel_frame_bytes_under_a_chunk_ceiling(
        chunk_ceiling_bytes,
    ))
    .checked_next_power_of_two()
}

/// Trust tier of an iceoryx2 data channel, selecting the default per-channel
/// shared-memory chunk ceiling.
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
    /// The default per-channel shared-memory chunk ceiling in bytes for this
    /// tier. The frames it admits are bounded by
    /// [`largest_channel_frame_bytes_under_a_chunk_ceiling`].
    pub const fn default_chunk_ceiling_bytes(self) -> usize {
        match self {
            Self::Trusted => TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
            Self::UntrustedSession => UNTRUSTED_SESSION_CHANNEL_CHUNK_CEILING_BYTES,
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

/// A PowerOfTwo growth event a channel publisher observed while admitting a
/// frame: the tracked per-slot capacity crossed the frame's sample size and was
/// advanced from `old_segment_bytes` to `new_segment_bytes`
/// (`next_power_of_two`). Both are one slot's bucket in sample bytes — the
/// layout iceoryx2 buckets, [`iceoryx2_sample_bytes_for_a_channel_frame`] —
/// never the frame alone, and never the whole data segment, which holds one
/// bucket per subscriber slot.
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
    /// The frame's sample is above the channel's chunk ceiling and the frame was
    /// refused. The caller drops it, surfaces the refusal as a typed
    /// `PayloadExceedsChannelCeiling` error and logs it; `refused_count` is the
    /// running total the caller's counter reported after recording this
    /// refusal.
    RefusedOverCeiling { refused_count: u64 },
    /// The frame fits under the ceiling; the caller publishes it. When `grew_to`
    /// is `Some(growth)` the tracked data-segment capacity crossed the frame's
    /// sample size and was advanced — a PowerOfTwo growth the caller logs,
    /// additionally raising a `warn` when
    /// [`ChannelSegmentGrowth::crossed_quarter_ceiling`].
    Admitted {
        grew_to: Option<ChannelSegmentGrowth>,
    },
}

/// Single authority for the per-channel-egress ceiling refusal + PowerOfTwo
/// growth-observability bookkeeping every channel publisher runs before loaning
/// a frame.
///
/// Refusing above `chunk_ceiling_bytes` is the graceful, observable layer in
/// front of the subprocess cgroup `memory.max` backstop. The engine's output
/// writer is the one caller, in the app process and in every helper alike: this
/// records a refusal through `record_one_refused_frame`, which hands back the
/// caller's running total, advances `current_slot_capacity_bytes` to the next
/// power of two on a growth (in place), and reports whether that growth first
/// crossed a quarter of the ceiling. The shared diagnostics live in
/// [`emit_channel_egress_admission_tracing`] beside this decision so they cannot
/// drift from it.
///
/// Both the refusal and the growth are measured on the frame's shared-memory
/// SAMPLE, never the frame alone: the sample is what iceoryx2's pool buckets, so
/// measuring the frame would admit a ceiling-sized bag into a bucket twice the
/// ceiling and report a segment half its real size.
pub fn decide_channel_egress_admission(
    frame_total_bytes: usize,
    chunk_ceiling_bytes: usize,
    current_slot_capacity_bytes: &mut usize,
    record_one_refused_frame: impl FnOnce() -> u64,
) -> ChannelEgressAdmission {
    if frame_total_bytes > largest_channel_frame_bytes_under_a_chunk_ceiling(chunk_ceiling_bytes) {
        return ChannelEgressAdmission::RefusedOverCeiling {
            refused_count: record_one_refused_frame(),
        };
    }
    let frame_sample_bytes = iceoryx2_sample_bytes_for_a_channel_frame(frame_total_bytes);
    let grew_to = if frame_sample_bytes > *current_slot_capacity_bytes {
        let old_segment_bytes = *current_slot_capacity_bytes;
        let new_segment_bytes = frame_sample_bytes.next_power_of_two();
        *current_slot_capacity_bytes = new_segment_bytes;
        let quarter_ceiling_bytes = chunk_ceiling_bytes / 4;
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

/// Emit the channel-egress admission tracing for one
/// [`decide_channel_egress_admission`] decision, so the refusal /
/// segment-growth / quarter-of-ceiling diagnostics stay lock-step with it.
/// `trust_tier` labels each line; a `log_prefix` of
/// `Some((runtime_tag, processor_id))` scopes the message with a `[tag:id] `
/// prefix. The caller still maps [`ChannelEgressAdmission::RefusedOverCeiling`]
/// to its own typed error.
pub fn emit_channel_egress_admission_tracing(
    log_prefix: Option<(&str, &str)>,
    trust_tier: ChannelTrustTier,
    channel_service_name: &str,
    chunk_ceiling_bytes: usize,
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
                largest_admitted_frame_bytes =
                    largest_channel_frame_bytes_under_a_chunk_ceiling(chunk_ceiling_bytes),
                chunk_ceiling_bytes,
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
                        chunk_ceiling_bytes,
                        tier = trust_tier.as_str(),
                        "{}iceoryx2 publisher segment crossed a quarter of the channel ceiling",
                        prefix,
                    );
                }
            }
        }
    }
}

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
/// destination slots.
///
/// A channel's data service is created with `max_subscribers =
/// MAX_DESTINATIONS_PER_CHANNEL + this`. The reserved slot is the one the
/// runtime's publisher reads an exposed port through, a pure subscriber-add with
/// no service re-open — iceoryx2 fixes `max_subscribers` at create time, so the
/// headroom must exist up front. iceoryx2 sizes each publisher's shared-memory data
/// segment as `max_subscribers × (subscriber_max_buffer_size + borrowed) + …`,
/// so this is deliberately 1 (not the iceoryx2 default of 8).
pub const RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL: usize = 1;

/// Destinations one channel — one source output port — may feed at once.
///
/// iceoryx2 pins `max_subscribers` when the data service is created and
/// verifies it on every reopen, so the slot count cannot follow the graph: a
/// link connected to a running source must fit a slot that existed when the
/// channel opened. Every opener requests this same fixed count, and a source
/// output port refuses a link past it by name.
pub const MAX_DESTINATIONS_PER_CHANNEL: usize = 32;

/// Inbound links one destination processor may hold at once — the
/// `max_notifiers` its destination-keyed notify service is created with.
///
/// Pinned for the same reason as [`MAX_DESTINATIONS_PER_CHANNEL`]: the notify
/// service is created with the first inbound link and iceoryx2 verifies the
/// count on every reopen, so a link connected later must fit a notifier slot
/// that already exists.
pub const MAX_INBOUND_LINKS_PER_DESTINATION: usize = 256;

/// Whether a message opens a group or continues the one before it, carried in
/// [`DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageGroupBoundary {
    /// The message opens a group — a point a relay or a late joiner can start from.
    StartsGroup,
    /// The message continues the group the message before it opened.
    ContinuesGroup,
}

/// Error constructing a [`PortKey`] from a name that overflows the fixed
/// wire capacity.
///
/// A port / channel name longer than [`MAX_PORT_KEY_SIZE`] `- 1` bytes is refused
/// rather than clipped: a clipped name would route frames to a different port
/// than the one the author named. Names crossing this
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

/// The alignment every channel sample's payload starts at, offset 0 of the
/// sample's payload area: a cache line, and what a SIMD load or a DLPack
/// consumer asks of a CPU tensor.
pub const DATA_CHANNEL_PAYLOAD_ALIGNMENT_BYTES: usize = 64;

/// One cache line of a channel sample's payload, the element type of every
/// channel data service's slice.
///
/// The slice is of these rather than of `u8` under iceoryx2's
/// `payload_alignment`, because iceoryx2 strides every slice element at its
/// aligned size, which for a 64-byte-aligned `u8` would be 64 bytes a byte. A
/// payload of `n` bytes takes `n.div_ceil(64)` lines, and the user header's
/// [`DataChannelSampleUserHeader::payload_byte_len`] says where it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ZeroCopySend)]
#[type_name("DataChannelPayloadCacheLine")]
#[repr(C)]
#[repr(align(64))]
pub struct DataChannelPayloadCacheLine {
    /// The line's bytes.
    pub bytes: [u8; DATA_CHANNEL_PAYLOAD_ALIGNMENT_BYTES],
}

/// The cache lines a `payload_byte_len`-byte payload takes.
pub const fn data_channel_payload_cache_lines_for(payload_byte_len: usize) -> usize {
    payload_byte_len.div_ceil(DATA_CHANNEL_PAYLOAD_ALIGNMENT_BYTES)
}

/// Every byte of `cache_lines`, in order.
pub fn data_channel_payload_bytes_of(cache_lines: &[DataChannelPayloadCacheLine]) -> &[u8] {
    // SAFETY: `DataChannelPayloadCacheLine` is `repr(C)` over a byte array with
    // no padding (its size is its alignment), so a slice of them is that many
    // contiguous initialized bytes.
    unsafe {
        std::slice::from_raw_parts(
            cache_lines.as_ptr().cast::<u8>(),
            std::mem::size_of_val(cache_lines),
        )
    }
}

/// The [`DataChannelSampleUserHeader::flags`] bit set when the message opens a
/// group — a point a relay or a late joiner can start from. Bits 1..31 are
/// reserved: a writer writes them zero and a reader ignores them.
pub const DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP: u32 = 1 << 0;

/// Bytes in a [`DataChannelPayloadDescriptor`]'s kind-specific body.
pub const DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES: usize = 60;

/// The iceoryx2 user header every channel data sample carries: the sample's
/// place in its publisher's sends, what its writer declared lost upstream, its
/// stamp, its payload's length and group flag, and what the payload is.
///
/// The payload area carries nothing but payload. The iceoryx2 type name is
/// pinned because the default one carries the module path, and every opener
/// of a data service must present the same name or be refused
/// `IncompatibleTypes`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ZeroCopySend)]
#[type_name("DataChannelSampleUserHeader")]
#[repr(C)]
pub struct DataChannelSampleUserHeader {
    /// Counted from zero per publisher; consumed by every send that may have
    /// delivered.
    pub sequence_number: u64,
    /// Messages the writer declared lost upstream of it since its previous send
    /// that may have delivered — a loss no link of this channel caused.
    pub messages_lost_upstream_before_this_one: u64,
    /// The publish stamp, monotonic nanoseconds.
    pub timestamp_ns: i64,
    /// Bytes of payload the writer wrote; a reader never reads past it or past
    /// the sample's own slice.
    pub payload_byte_len: u32,
    /// [`DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP`]; every other bit is reserved zero.
    pub flags: u32,
    /// What the payload is.
    pub payload_descriptor: DataChannelPayloadDescriptor,
}

impl DataChannelSampleUserHeader {
    /// A header for a `payload_byte_len`-byte message of `payload_descriptor`,
    /// stamped `timestamp_ns` and `group_boundary`, before its numbering is set.
    pub fn new(
        timestamp_ns: i64,
        payload_byte_len: u32,
        group_boundary: MessageGroupBoundary,
        payload_descriptor: DataChannelPayloadDescriptor,
    ) -> Self {
        let flags = match group_boundary {
            MessageGroupBoundary::StartsGroup => DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP,
            MessageGroupBoundary::ContinuesGroup => 0,
        };
        Self {
            sequence_number: 0,
            messages_lost_upstream_before_this_one: 0,
            timestamp_ns,
            payload_byte_len,
            flags,
            payload_descriptor,
        }
    }

    /// Whether the message opens a group, read from
    /// [`DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP`].
    pub fn group_boundary(&self) -> MessageGroupBoundary {
        if self.flags & DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP != 0 {
            MessageGroupBoundary::StartsGroup
        } else {
            MessageGroupBoundary::ContinuesGroup
        }
    }

    /// The payload this header stamps, out of the sample's `sample_payload`
    /// slice, refused where the stamped length runs past it.
    ///
    /// The stamped length is a `u32` off the wire, so it bounds the read rather
    /// than being trusted by it.
    pub fn payload_within<'sample>(
        &self,
        sample_payload: &'sample [u8],
    ) -> Result<&'sample [u8], DataChannelSamplePayloadRefusal> {
        sample_payload.get(..self.payload_byte_len as usize).ok_or(
            DataChannelSamplePayloadRefusal::StampedLengthPastTheSample {
                stamped_payload_byte_len: self.payload_byte_len,
                sample_payload_byte_len: sample_payload.len(),
            },
        )
    }
}

/// Why a channel sample holds no payload a reader may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataChannelSamplePayloadRefusal {
    /// The user header stamps a longer payload than the sample carries.
    StampedLengthPastTheSample {
        stamped_payload_byte_len: u32,
        sample_payload_byte_len: usize,
    },
    /// The descriptor names a kind this reader does not know.
    UnknownPayloadDescriptorKind { kind: u32 },
}

impl std::fmt::Display for DataChannelSamplePayloadRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StampedLengthPastTheSample {
                stamped_payload_byte_len,
                sample_payload_byte_len,
            } => write!(
                f,
                "the sample's user header declares a {stamped_payload_byte_len}-byte payload but \
                 the sample carries only {sample_payload_byte_len} bytes"
            ),
            Self::UnknownPayloadDescriptorKind { kind } => write!(
                f,
                "the sample's payload descriptor names kind {kind}, which this reader does not \
                 know"
            ),
        }
    }
}

impl std::error::Error for DataChannelSamplePayloadRefusal {}

/// The kinds a [`DataChannelPayloadDescriptor`] names, by their wire value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum DataChannelPayloadDescriptorKind {
    /// The payload is the sample's bytes; the body is zero.
    Bytes = 0,
    /// The payload names a slot of a GPU pool; the body is a
    /// [`GpuPoolSlotFrameDescriptorBody`].
    GpuPoolSlotFrame = 1,
}

impl DataChannelPayloadDescriptorKind {
    /// The kind `wire_kind` names, or `None` for one this build does not know.
    pub fn from_wire(wire_kind: u32) -> Option<Self> {
        match wire_kind {
            0 => Some(Self::Bytes),
            1 => Some(Self::GpuPoolSlotFrame),
            _ => None,
        }
    }
}

/// What a channel sample's payload is: a kind and a fixed-size body the kind
/// gives a layout to. A reader refuses a kind it does not know, by kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ZeroCopySend)]
#[type_name("DataChannelPayloadDescriptor")]
#[repr(C)]
pub struct DataChannelPayloadDescriptor {
    /// A [`DataChannelPayloadDescriptorKind`] wire value.
    pub kind: u32,
    /// The kind's body, little-endian; reserved bytes are zero.
    pub body: [u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES],
}

impl DataChannelPayloadDescriptor {
    /// The descriptor of a payload that is the sample's bytes.
    pub const fn bytes() -> Self {
        Self {
            kind: DataChannelPayloadDescriptorKind::Bytes as u32,
            body: [0u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES],
        }
    }

    /// The descriptor of a payload naming one GPU pool slot.
    pub fn gpu_pool_slot_frame(body: GpuPoolSlotFrameDescriptorBody) -> Self {
        Self {
            kind: DataChannelPayloadDescriptorKind::GpuPoolSlotFrame as u32,
            body: body.to_descriptor_body(),
        }
    }

    /// The kind this descriptor names, refused by kind when this build does not
    /// know it.
    pub fn known_kind(
        &self,
    ) -> Result<DataChannelPayloadDescriptorKind, DataChannelSamplePayloadRefusal> {
        DataChannelPayloadDescriptorKind::from_wire(self.kind).ok_or(
            DataChannelSamplePayloadRefusal::UnknownPayloadDescriptorKind { kind: self.kind },
        )
    }
}

impl Default for DataChannelPayloadDescriptor {
    fn default() -> Self {
        Self::bytes()
    }
}

/// Byte offset of [`GpuPoolSlotFrameDescriptorBody::pool`] in a descriptor body.
pub const GPU_POOL_SLOT_FRAME_BODY_POOL_OFFSET: usize = 0;
/// Byte offset of [`GpuPoolSlotFrameDescriptorBody::slot`] in a descriptor body.
pub const GPU_POOL_SLOT_FRAME_BODY_SLOT_OFFSET: usize = 16;
/// Byte offset of [`GpuPoolSlotFrameDescriptorBody::generation`] in a descriptor body.
pub const GPU_POOL_SLOT_FRAME_BODY_GENERATION_OFFSET: usize = 20;
/// Byte offset of [`GpuPoolSlotFrameDescriptorBody::timeline_value`] in a descriptor body.
pub const GPU_POOL_SLOT_FRAME_BODY_TIMELINE_VALUE_OFFSET: usize = 28;
/// Bytes of a descriptor body the GPU pool slot frame uses; the rest are
/// reserved zero.
pub const GPU_POOL_SLOT_FRAME_BODY_USED_BYTES: usize = 36;

/// The body of a [`DataChannelPayloadDescriptorKind::GpuPoolSlotFrame`]
/// descriptor: which pool, which slot, which hand-out of it, and the timeline
/// value its writes signal. Packed little-endian into the descriptor body at
/// the `GPU_POOL_SLOT_FRAME_BODY_*_OFFSET` offsets, because the body is a byte
/// array at a 4-byte offset and cannot hold an 8-byte-aligned field in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuPoolSlotFrameDescriptorBody {
    /// The pool's UUID.
    pub pool: [u8; 16],
    /// The slot's index in the pool.
    pub slot: u32,
    /// Which hand-out of the slot the frame is.
    pub generation: u64,
    /// The timeline semaphore value the frame's writes signal.
    pub timeline_value: u64,
}

impl GpuPoolSlotFrameDescriptorBody {
    /// This body as descriptor body bytes, the reserved tail zero.
    pub fn to_descriptor_body(&self) -> [u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES] {
        let mut body = [0u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES];
        body[GPU_POOL_SLOT_FRAME_BODY_POOL_OFFSET..GPU_POOL_SLOT_FRAME_BODY_SLOT_OFFSET]
            .copy_from_slice(&self.pool);
        body[GPU_POOL_SLOT_FRAME_BODY_SLOT_OFFSET..GPU_POOL_SLOT_FRAME_BODY_GENERATION_OFFSET]
            .copy_from_slice(&self.slot.to_le_bytes());
        body[GPU_POOL_SLOT_FRAME_BODY_GENERATION_OFFSET
            ..GPU_POOL_SLOT_FRAME_BODY_TIMELINE_VALUE_OFFSET]
            .copy_from_slice(&self.generation.to_le_bytes());
        body[GPU_POOL_SLOT_FRAME_BODY_TIMELINE_VALUE_OFFSET..GPU_POOL_SLOT_FRAME_BODY_USED_BYTES]
            .copy_from_slice(&self.timeline_value.to_le_bytes());
        body
    }

    /// The body `descriptor_body` packs.
    pub fn from_descriptor_body(
        descriptor_body: &[u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES],
    ) -> Self {
        let field = |start: usize, end: usize| &descriptor_body[start..end];
        let mut pool = [0u8; 16];
        pool.copy_from_slice(field(
            GPU_POOL_SLOT_FRAME_BODY_POOL_OFFSET,
            GPU_POOL_SLOT_FRAME_BODY_SLOT_OFFSET,
        ));
        let mut slot = [0u8; 4];
        slot.copy_from_slice(field(
            GPU_POOL_SLOT_FRAME_BODY_SLOT_OFFSET,
            GPU_POOL_SLOT_FRAME_BODY_GENERATION_OFFSET,
        ));
        let mut generation = [0u8; 8];
        generation.copy_from_slice(field(
            GPU_POOL_SLOT_FRAME_BODY_GENERATION_OFFSET,
            GPU_POOL_SLOT_FRAME_BODY_TIMELINE_VALUE_OFFSET,
        ));
        let mut timeline_value = [0u8; 8];
        timeline_value.copy_from_slice(field(
            GPU_POOL_SLOT_FRAME_BODY_TIMELINE_VALUE_OFFSET,
            GPU_POOL_SLOT_FRAME_BODY_USED_BYTES,
        ));
        Self {
            pool,
            slot: u32::from_le_bytes(slot),
            generation: u64::from_le_bytes(generation),
            timeline_value: u64::from_le_bytes(timeline_value),
        }
    }
}

/// The board section a [`HelperProcessLossCountBoardKey`] names an inbound-link slot in.
const HELPER_PROCESS_LOSS_COUNT_BOARD_INBOUND_LINK_SLOT_SECTION: u32 = 0;

/// The board section a [`HelperProcessLossCountBoardKey`] names an output port in.
const HELPER_PROCESS_LOSS_COUNT_BOARD_OUTPUT_PORT_SECTION: u32 = 1;

/// The board section holding the helper's one step-progress entry.
const HELPER_PROCESS_LOSS_COUNT_BOARD_STEP_PROGRESS_SECTION: u32 = 2;

/// The key of one entry on a helper process's loss-count blackboard: an
/// inbound-link slot, a declared output port by its index, or the helper's step
/// progress.
///
/// The iceoryx2 type name is pinned for the reason
/// [`DataChannelSampleUserHeader`]'s is: the parent creates the board
/// and the helper opens it, and both must present the same key type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, ZeroCopySend)]
#[type_name("HelperProcessLossCountBoardKey")]
#[repr(C)]
pub struct HelperProcessLossCountBoardKey {
    section: u32,
    index_within_section: u32,
}

impl HelperProcessLossCountBoardKey {
    /// The key of inbound-link slot `slot`.
    pub const fn inbound_link_slot(slot: u32) -> Self {
        Self {
            section: HELPER_PROCESS_LOSS_COUNT_BOARD_INBOUND_LINK_SLOT_SECTION,
            index_within_section: slot,
        }
    }

    /// The key of the declared output port at `output_port_index`.
    pub const fn output_port(output_port_index: u32) -> Self {
        Self {
            section: HELPER_PROCESS_LOSS_COUNT_BOARD_OUTPUT_PORT_SECTION,
            index_within_section: output_port_index,
        }
    }

    /// The key of the helper's step-progress entry.
    pub const STEP_PROGRESS: Self = Self {
        section: HELPER_PROCESS_LOSS_COUNT_BOARD_STEP_PROGRESS_SECTION,
        index_within_section: 0,
    };
}

/// One inbound link's loss counts as a helper process last wrote them, beside
/// the wiring generation the parent assigned that link.
///
/// The parent reads a slot as its link's only while the generation matches, so
/// a write for a link since unwired never lands on the link that reused the slot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ZeroCopySend)]
#[type_name("InboundLinkLossCountBoardSlot")]
#[repr(C)]
pub struct InboundLinkLossCountBoardSlot {
    /// The generation of the wiring these counts belong to; zero before any
    /// write.
    pub wiring_generation: u64,
    /// Bags lost on the link before anything read them.
    pub dropped_bags: u64,
    /// Samples a windowed port's flushes discarded on the link.
    pub discarded_samples: u64,
    /// Bags that left the link's mailbox for its reader: read, passed over by
    /// a `newest` read, or evicted from a port that may pass bags over.
    pub bags_off_the_mailbox_for_its_reader: u64,
    /// Of those, the bags the reader counted lost after taking them, which
    /// `dropped_bags` already holds.
    pub bags_its_reader_counted_lost_after_taking_them: u64,
}

/// One output port's counts of bags refused at its channel ceiling and bags
/// written onto its channel, as a helper process last wrote them, beside the
/// wiring generation the parent assigned the port's channel.
///
/// Read as the port's only while the generation matches, so a reopened
/// channel never shows the totals of the one before it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ZeroCopySend)]
#[type_name("OutputPortBagCountBoardEntry")]
#[repr(C)]
pub struct OutputPortBagCountBoardEntry {
    /// The generation of the channel these counts belong to; zero before any
    /// write.
    pub wiring_generation: u64,
    /// Bags refused since the port's channel was opened.
    pub refused_bags: u64,
    /// Bags sent on the port's channel since it was opened.
    pub written_bags: u64,
}

/// A helper's callbacks in its stream's steps, as it last wrote them: what the
/// step barrier reads before it closes a step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ZeroCopySend)]
#[type_name("HelperProcessStepProgressBoardEntry")]
#[repr(C)]
pub struct HelperProcessStepProgressBoardEntry {
    /// One past the last step the helper's node finished; step `k` is finished
    /// once this exceeds `k`.
    pub steps_finished: u64,
    /// Callbacks of the node's started and not yet returned.
    pub callbacks_in_flight: u32,
    /// Always zero; keeps the entry a multiple of 8 bytes.
    pub reserved: u32,
}

/// The key of the one entry on a stream's clock blackboard.
///
/// Pinned under its own iceoryx2 type name: the engine creates the board and
/// every helper of the stream opens it, and both must present the same key type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, ZeroCopySend)]
#[type_name("StreamClockBoardKey")]
#[repr(C)]
pub struct StreamClockBoardKey {
    entry_index: u32,
}

impl StreamClockBoardKey {
    /// The key of the board's one entry, the stream clock.
    pub const STREAM_CLOCK: Self = Self { entry_index: 0 };
}

/// [`StreamClockBoardEntry::mode`] for a stream on the machine's monotonic clock.
pub const STREAM_CLOCK_BOARD_MODE_REAL: u32 = 0;

/// [`StreamClockBoardEntry::mode`] for a stream whose time runs at a scale of
/// the machine's monotonic clock.
pub const STREAM_CLOCK_BOARD_MODE_SCALED: u32 = 1;

/// [`StreamClockBoardEntry::mode`] for a stream whose time advances in steps.
pub const STREAM_CLOCK_BOARD_MODE_STEPPED: u32 = 2;

/// A stream's clock as its engine last wrote it, copied whole by every reader.
///
/// Stream time is `base_ns + scale × (machine monotonic now − anchor_ns)` when
/// scaled, and `base_ns + step_index × step_ns` when stepped; both are in the
/// machine's monotonic epoch. The two `f64` fields travel as their IEEE-754 bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ZeroCopySend)]
#[type_name("StreamClockBoardEntry")]
#[repr(C)]
pub struct StreamClockBoardEntry {
    /// One of the `STREAM_CLOCK_BOARD_MODE_*` values.
    pub mode: u32,
    /// One once a stepped clock opened its first step, zero before and unless stepped.
    pub a_step_is_open: u32,
    /// The step open now; zero unless a step is open.
    pub step_index: u64,
    /// Stream nanoseconds per step; zero unless the clock steps.
    pub step_ns: u64,
    /// Stream time at the anchor, in nanoseconds.
    pub base_ns: i64,
    /// The machine monotonic reading `base_ns` was taken at, in nanoseconds.
    pub anchor_ns: i64,
    /// Stream seconds per machine second, as `f64::to_bits`.
    pub scale_f64_bits: u64,
    /// Steps paced to this multiple of real time, as `f64::to_bits`; zero when unpaced.
    pub pace_f64_bits: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stream_clock_board_key_is_one_u32_under_its_pinned_name() {
        assert_eq!(std::mem::size_of::<StreamClockBoardKey>(), 4);
        assert_eq!(std::mem::align_of::<StreamClockBoardKey>(), 4);
        assert_eq!(std::mem::offset_of!(StreamClockBoardKey, entry_index), 0);
        assert_eq!(
            // SAFETY: reads the pinned name; the layout it vouches for is asserted above.
            unsafe { <StreamClockBoardKey as ZeroCopySend>::type_name() },
            "StreamClockBoardKey"
        );
    }

    #[test]
    fn a_stream_clock_board_entry_is_two_u32s_and_six_eight_byte_fields_under_its_pinned_name() {
        assert_eq!(std::mem::size_of::<StreamClockBoardEntry>(), 56);
        assert_eq!(std::mem::align_of::<StreamClockBoardEntry>(), 8);
        assert_eq!(std::mem::offset_of!(StreamClockBoardEntry, mode), 0);
        assert_eq!(
            std::mem::offset_of!(StreamClockBoardEntry, a_step_is_open),
            4
        );
        assert_eq!(std::mem::offset_of!(StreamClockBoardEntry, step_index), 8);
        assert_eq!(std::mem::offset_of!(StreamClockBoardEntry, step_ns), 16);
        assert_eq!(std::mem::offset_of!(StreamClockBoardEntry, base_ns), 24);
        assert_eq!(std::mem::offset_of!(StreamClockBoardEntry, anchor_ns), 32);
        assert_eq!(
            std::mem::offset_of!(StreamClockBoardEntry, scale_f64_bits),
            40
        );
        assert_eq!(
            std::mem::offset_of!(StreamClockBoardEntry, pace_f64_bits),
            48
        );
        assert_eq!(
            // SAFETY: reads the pinned name; the layout it vouches for is asserted above.
            unsafe { <StreamClockBoardEntry as ZeroCopySend>::type_name() },
            "StreamClockBoardEntry"
        );
    }

    #[test]
    fn the_channel_sample_user_header_sits_at_its_documented_offsets_under_its_pinned_name() {
        assert_eq!(size_of::<DataChannelSampleUserHeader>(), 96);
        assert_eq!(align_of::<DataChannelSampleUserHeader>(), 8);
        assert_eq!(
            [
                std::mem::offset_of!(DataChannelSampleUserHeader, sequence_number),
                std::mem::offset_of!(
                    DataChannelSampleUserHeader,
                    messages_lost_upstream_before_this_one
                ),
                std::mem::offset_of!(DataChannelSampleUserHeader, timestamp_ns),
                std::mem::offset_of!(DataChannelSampleUserHeader, payload_byte_len),
                std::mem::offset_of!(DataChannelSampleUserHeader, flags),
                std::mem::offset_of!(DataChannelSampleUserHeader, payload_descriptor),
            ],
            [0, 8, 16, 24, 28, 32]
        );
        assert_eq!(
            unsafe { <DataChannelSampleUserHeader as ZeroCopySend>::type_name() },
            "DataChannelSampleUserHeader",
        );
    }

    #[test]
    fn the_payload_descriptor_is_a_kind_and_a_sixty_byte_body_under_its_pinned_name() {
        assert_eq!(size_of::<DataChannelPayloadDescriptor>(), 64);
        assert_eq!(align_of::<DataChannelPayloadDescriptor>(), 4);
        assert_eq!(std::mem::offset_of!(DataChannelPayloadDescriptor, kind), 0);
        assert_eq!(std::mem::offset_of!(DataChannelPayloadDescriptor, body), 4);
        assert_eq!(
            unsafe { <DataChannelPayloadDescriptor as ZeroCopySend>::type_name() },
            "DataChannelPayloadDescriptor",
        );
        assert_eq!(DataChannelPayloadDescriptorKind::Bytes as u32, 0);
        assert_eq!(DataChannelPayloadDescriptorKind::GpuPoolSlotFrame as u32, 1);
        assert_eq!(DataChannelPayloadDescriptor::bytes().body, [0u8; 60]);
    }

    #[test]
    fn a_payload_cache_line_is_sixty_four_bytes_at_sixty_four_byte_alignment() {
        assert_eq!(size_of::<DataChannelPayloadCacheLine>(), 64);
        assert_eq!(align_of::<DataChannelPayloadCacheLine>(), 64);
        assert_eq!(
            unsafe { <DataChannelPayloadCacheLine as ZeroCopySend>::type_name() },
            "DataChannelPayloadCacheLine",
        );
        assert_eq!(data_channel_payload_cache_lines_for(0), 0);
        assert_eq!(data_channel_payload_cache_lines_for(1), 1);
        assert_eq!(data_channel_payload_cache_lines_for(64), 1);
        assert_eq!(data_channel_payload_cache_lines_for(65), 2);
    }

    #[test]
    fn the_gpu_pool_slot_frame_body_packs_every_field_at_its_documented_offset() {
        assert_eq!(
            [
                GPU_POOL_SLOT_FRAME_BODY_POOL_OFFSET,
                GPU_POOL_SLOT_FRAME_BODY_SLOT_OFFSET,
                GPU_POOL_SLOT_FRAME_BODY_GENERATION_OFFSET,
                GPU_POOL_SLOT_FRAME_BODY_TIMELINE_VALUE_OFFSET,
                GPU_POOL_SLOT_FRAME_BODY_USED_BYTES,
            ],
            [0, 16, 20, 28, 36]
        );
        let frame = GpuPoolSlotFrameDescriptorBody {
            pool: *b"0123456789abcdef",
            slot: 0x0403_0201,
            generation: 0x0c0b_0a09_0807_0605,
            timeline_value: u64::MAX - 1,
        };
        let descriptor = DataChannelPayloadDescriptor::gpu_pool_slot_frame(frame);
        assert_eq!(
            descriptor.known_kind(),
            Ok(DataChannelPayloadDescriptorKind::GpuPoolSlotFrame)
        );
        assert_eq!(&descriptor.body[0..16], b"0123456789abcdef");
        assert_eq!(&descriptor.body[16..20], &[1, 2, 3, 4]);
        assert_eq!(&descriptor.body[20..28], &[5, 6, 7, 8, 9, 10, 11, 12]);
        assert_eq!(&descriptor.body[28..36], &(u64::MAX - 1).to_le_bytes());
        assert!(descriptor.body[36..].iter().all(|&reserved| reserved == 0));
        assert_eq!(
            GpuPoolSlotFrameDescriptorBody::from_descriptor_body(&descriptor.body),
            frame
        );
    }

    #[test]
    fn a_descriptor_of_an_unknown_kind_is_refused_by_kind() {
        let unknown = DataChannelPayloadDescriptor {
            kind: 7,
            body: [0u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES],
        };
        assert_eq!(
            unknown.known_kind(),
            Err(DataChannelSamplePayloadRefusal::UnknownPayloadDescriptorKind { kind: 7 })
        );
    }

    #[test]
    fn the_starts_group_flag_is_bit_zero_and_round_trips_through_the_header() {
        assert_eq!(DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP, 0x0000_0001);
        for group_boundary in [
            MessageGroupBoundary::StartsGroup,
            MessageGroupBoundary::ContinuesGroup,
        ] {
            let header = DataChannelSampleUserHeader::new(
                -5,
                3,
                group_boundary,
                DataChannelPayloadDescriptor::bytes(),
            );
            assert_eq!(header.flags & !DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP, 0);
            assert_eq!(header.group_boundary(), group_boundary);
        }
    }

    #[test]
    fn a_header_bounds_its_payload_and_refuses_a_stamp_past_the_sample() {
        let header = DataChannelSampleUserHeader::new(
            0,
            3,
            MessageGroupBoundary::StartsGroup,
            DataChannelPayloadDescriptor::bytes(),
        );
        assert_eq!(header.payload_within(b"abcdef"), Ok(&b"abc"[..]));
        assert_eq!(
            header.payload_within(b"ab"),
            Err(
                DataChannelSamplePayloadRefusal::StampedLengthPastTheSample {
                    stamped_payload_byte_len: 3,
                    sample_payload_byte_len: 2,
                }
            )
        );
    }

    #[test]
    fn the_loss_count_board_key_is_two_u32s_under_its_pinned_name() {
        assert_eq!(std::mem::size_of::<HelperProcessLossCountBoardKey>(), 8);
        assert_eq!(std::mem::align_of::<HelperProcessLossCountBoardKey>(), 4);
        assert_eq!(
            std::mem::offset_of!(HelperProcessLossCountBoardKey, section),
            0
        );
        assert_eq!(
            std::mem::offset_of!(HelperProcessLossCountBoardKey, index_within_section),
            4
        );
        assert_ne!(
            HelperProcessLossCountBoardKey::inbound_link_slot(3),
            HelperProcessLossCountBoardKey::output_port(3),
            "a slot and an output port at one index are two entries"
        );
        assert_ne!(
            HelperProcessLossCountBoardKey::STEP_PROGRESS,
            HelperProcessLossCountBoardKey::inbound_link_slot(0),
            "the step-progress entry is in a section of its own"
        );
        assert_eq!(
            // SAFETY: reads the pinned name; the layout it vouches for is asserted above.
            unsafe { <HelperProcessLossCountBoardKey as ZeroCopySend>::type_name() },
            "HelperProcessLossCountBoardKey"
        );
    }

    #[test]
    fn an_inbound_link_slot_is_five_u64s_at_their_documented_offsets_under_its_pinned_name() {
        assert_eq!(std::mem::size_of::<InboundLinkLossCountBoardSlot>(), 40);
        assert_eq!(std::mem::align_of::<InboundLinkLossCountBoardSlot>(), 8);
        assert_eq!(
            std::mem::offset_of!(InboundLinkLossCountBoardSlot, wiring_generation),
            0
        );
        assert_eq!(
            std::mem::offset_of!(InboundLinkLossCountBoardSlot, dropped_bags),
            8
        );
        assert_eq!(
            std::mem::offset_of!(InboundLinkLossCountBoardSlot, discarded_samples),
            16
        );
        assert_eq!(
            std::mem::offset_of!(
                InboundLinkLossCountBoardSlot,
                bags_off_the_mailbox_for_its_reader
            ),
            24
        );
        assert_eq!(
            std::mem::offset_of!(
                InboundLinkLossCountBoardSlot,
                bags_its_reader_counted_lost_after_taking_them
            ),
            32
        );
        assert_eq!(
            // SAFETY: reads the pinned name; the layout it vouches for is asserted above.
            unsafe { <InboundLinkLossCountBoardSlot as ZeroCopySend>::type_name() },
            "InboundLinkLossCountBoardSlot"
        );
    }

    #[test]
    fn an_output_ports_bag_count_entry_is_three_u64s_at_their_documented_offsets_under_its_pinned_name()
     {
        assert_eq!(std::mem::size_of::<OutputPortBagCountBoardEntry>(), 24);
        assert_eq!(std::mem::align_of::<OutputPortBagCountBoardEntry>(), 8);
        assert_eq!(
            std::mem::offset_of!(OutputPortBagCountBoardEntry, wiring_generation),
            0
        );
        assert_eq!(
            std::mem::offset_of!(OutputPortBagCountBoardEntry, refused_bags),
            8
        );
        assert_eq!(
            std::mem::offset_of!(OutputPortBagCountBoardEntry, written_bags),
            16
        );
        assert_eq!(
            // SAFETY: reads the pinned name; the layout it vouches for is asserted above.
            unsafe { <OutputPortBagCountBoardEntry as ZeroCopySend>::type_name() },
            "OutputPortBagCountBoardEntry"
        );
    }

    #[test]
    fn a_helpers_step_progress_entry_is_a_u64_and_two_u32s_under_its_pinned_name() {
        assert_eq!(
            std::mem::size_of::<HelperProcessStepProgressBoardEntry>(),
            16
        );
        assert_eq!(
            std::mem::align_of::<HelperProcessStepProgressBoardEntry>(),
            8
        );
        assert_eq!(
            std::mem::offset_of!(HelperProcessStepProgressBoardEntry, steps_finished),
            0
        );
        assert_eq!(
            std::mem::offset_of!(HelperProcessStepProgressBoardEntry, callbacks_in_flight),
            8
        );
        assert_eq!(
            std::mem::offset_of!(HelperProcessStepProgressBoardEntry, reserved),
            12
        );
        assert_eq!(
            // SAFETY: reads the pinned name; the layout it vouches for is asserted above.
            unsafe { <HelperProcessStepProgressBoardEntry as ZeroCopySend>::type_name() },
            "HelperProcessStepProgressBoardEntry"
        );
    }

    #[test]
    fn channel_trust_tier_defaults_and_labels() {
        assert_eq!(
            ChannelTrustTier::Trusted.default_chunk_ceiling_bytes(),
            TRUSTED_CHANNEL_CHUNK_CEILING_BYTES
        );
        assert_eq!(
            ChannelTrustTier::UntrustedSession.default_chunk_ceiling_bytes(),
            UNTRUSTED_SESSION_CHANNEL_CHUNK_CEILING_BYTES
        );
        assert!(
            ChannelTrustTier::UntrustedSession.default_chunk_ceiling_bytes()
                < ChannelTrustTier::Trusted.default_chunk_ceiling_bytes(),
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
        // A name one byte past the wire capacity must be a named error, NOT a clipped key
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

    /// The shape of `MessageTypeDetails::chunk_layout`, spelled out here so a
    /// one-sided edit of the constants reddens. Both sides read the same upstream
    /// types, so this deliberately does NOT catch an upstream size change — that
    /// moves the constants and the formula together, which is the point of
    /// composing them from the types rather than writing a number. What catches
    /// an upstream change to the formula itself is the engine's
    /// `a_ceiling_sized_bag_fills_the_ceilings_chunk_and_one_byte_more_takes_the_next`,
    /// which loans a real chunk.
    #[test]
    fn a_channel_samples_layout_is_iceoryx2s_own_over_the_two_headers_and_the_payload() {
        type Iceoryx2PublishSubscribeHeader = iceoryx2::service::header::publish_subscribe::Header;
        type Line = DataChannelPayloadCacheLine;
        let all_headers_len = (size_of::<Iceoryx2PublishSubscribeHeader>()
            .next_multiple_of(align_of::<DataChannelSampleUserHeader>())
            + size_of::<DataChannelSampleUserHeader>())
        .next_multiple_of(align_of::<Line>());
        let max_alignment = align_of::<Iceoryx2PublishSubscribeHeader>()
            .max(align_of::<DataChannelSampleUserHeader>())
            .max(align_of::<Line>());
        assert_eq!(
            all_headers_len % 64,
            0,
            "the payload must start 64-byte aligned"
        );
        for frame_total_bytes in [0usize, 1, 7, 8, 9, 64, 65, 4096, 65_536, 1_000_003] {
            let upstream_formula = (all_headers_len
                + size_of::<Line>().next_multiple_of(align_of::<Line>())
                    * frame_total_bytes.div_ceil(size_of::<Line>()))
            .next_multiple_of(max_alignment);
            assert_eq!(
                iceoryx2_sample_bytes_for_a_channel_frame(frame_total_bytes),
                upstream_formula,
                "the sample size for a {frame_total_bytes}-byte frame must be iceoryx2's own"
            );
        }
    }

    /// The ceiling arithmetic is exact for any ceiling an operator's per-tier
    /// override can set, not only for the two powers of two the tiers default
    /// to: the admitted frame's sample fits and the next byte's does not.
    #[test]
    fn the_admitted_frame_is_exact_at_every_ceiling_an_override_can_set() {
        for chunk_ceiling_bytes in [
            1usize,
            55,
            56,
            57,
            64,
            100,
            4095,
            4096,
            100_003,
            TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
            TRUSTED_CHANNEL_CHUNK_CEILING_BYTES + 1,
        ] {
            let largest_admitted_frame_bytes =
                largest_channel_frame_bytes_under_a_chunk_ceiling(chunk_ceiling_bytes);
            if largest_admitted_frame_bytes == 0 {
                // The degenerate arm: a ceiling under the sample headers and
                // one payload line admits no byte of payload.
                assert!(iceoryx2_sample_bytes_for_a_channel_frame(1) > chunk_ceiling_bytes);
                continue;
            }
            assert!(
                iceoryx2_sample_bytes_for_a_channel_frame(largest_admitted_frame_bytes)
                    <= chunk_ceiling_bytes,
                "the admitted frame's sample must fit a {chunk_ceiling_bytes}-byte ceiling"
            );
            assert!(
                iceoryx2_sample_bytes_for_a_channel_frame(largest_admitted_frame_bytes + 1)
                    > chunk_ceiling_bytes,
                "one byte more must not fit a {chunk_ceiling_bytes}-byte ceiling"
            );
        }
    }

    /// Mental-revert guard for the ceiling half of the shared-memory accounting:
    /// compare the FRAME against the ceiling instead of its sample, and a
    /// ceiling-sized bag is admitted into a chunk twice the ceiling — the
    /// container `/dev/shm` SIGBUS this subtraction exists to keep away from.
    #[test]
    fn a_frame_at_the_admitted_ceiling_keeps_its_sample_inside_one_ceiling_sized_chunk() {
        for chunk_ceiling_bytes in [
            TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
            UNTRUSTED_SESSION_CHANNEL_CHUNK_CEILING_BYTES,
        ] {
            let largest_admitted_frame_bytes =
                largest_channel_frame_bytes_under_a_chunk_ceiling(chunk_ceiling_bytes);
            assert!(
                largest_admitted_frame_bytes < chunk_ceiling_bytes,
                "the admitted frame must leave room for iceoryx2's sample headers"
            );
            assert_eq!(
                chunk_bytes_a_ceiling_sized_bag_takes(chunk_ceiling_bytes),
                Some(chunk_ceiling_bytes),
                "the largest admitted frame must bucket at exactly the ceiling"
            );
            assert_eq!(
                iceoryx2_sample_bytes_for_a_channel_frame(chunk_ceiling_bytes).next_power_of_two(),
                chunk_ceiling_bytes * 2,
                "a frame sized to the ceiling itself is what takes a double-sized chunk"
            );
        }
    }

    /// The composite never panics on a ceiling an operator's env override can
    /// set, however absurd — it is read from inside a warning, and a diagnostic
    /// that kills the engine is worse than the misconfiguration it describes.
    #[test]
    fn the_chunk_a_ceiling_sized_bag_takes_is_unknown_rather_than_a_panic_at_the_extremes() {
        assert_eq!(chunk_bytes_a_ceiling_sized_bag_takes(usize::MAX), None);
        assert_eq!(
            chunk_bytes_a_ceiling_sized_bag_takes(TRUSTED_CHANNEL_CHUNK_CEILING_BYTES),
            Some(TRUSTED_CHANNEL_CHUNK_CEILING_BYTES)
        );
        // A ceiling under the sample headers admits no frame at all; the chunk an
        // empty one would take is still a real, small number.
        assert!(chunk_bytes_a_ceiling_sized_bag_takes(32).is_some());
    }

    #[test]
    fn a_frame_one_byte_past_the_admitted_ceiling_is_refused_and_the_admitted_one_is_not() {
        let chunk_ceiling_bytes = TRUSTED_CHANNEL_CHUNK_CEILING_BYTES;
        let largest_admitted_frame_bytes =
            largest_channel_frame_bytes_under_a_chunk_ceiling(chunk_ceiling_bytes);
        let mut slot = DEFAULT_EXPECTED_PAYLOAD_BYTES;

        assert!(
            matches!(
                decide_channel_egress_admission(
                    largest_admitted_frame_bytes,
                    chunk_ceiling_bytes,
                    &mut slot,
                    || panic!("the largest admitted frame must not be refused"),
                ),
                ChannelEgressAdmission::Admitted { .. }
            ),
            "the largest frame whose sample fits the chunk must be admitted"
        );
        assert_eq!(
            slot, chunk_ceiling_bytes,
            "the tracked slot is the sample's bucket, which is the ceiling itself"
        );

        assert_eq!(
            decide_channel_egress_admission(
                largest_admitted_frame_bytes + 1,
                chunk_ceiling_bytes,
                &mut slot,
                || 1,
            ),
            ChannelEgressAdmission::RefusedOverCeiling { refused_count: 1 },
            "one byte more would need a chunk twice the ceiling"
        );
    }

    /// The logging half of the same accounting: a frame that fits a power of two
    /// exactly still needs a sample larger than it, so the segment it grows to is
    /// the next bucket up. Measuring the frame alone under-reports by 2× here.
    #[test]
    fn a_growth_reports_the_bucket_the_sample_takes_and_not_the_frames_own_rounding() {
        let mut slot = 4096usize;
        match decide_channel_egress_admission(65_536, 128 * 1024, &mut slot, || {
            panic!("an admitted frame records no refusal")
        }) {
            ChannelEgressAdmission::Admitted {
                grew_to: Some(growth),
            } => assert_eq!(
                growth.new_segment_bytes, 131_072,
                "a 64 KiB frame's sample is past 64 KiB, so its bucket is 128 KiB"
            ),
            other => panic!("expected an Admitted growth, got {other:?}"),
        }
    }

    #[test]
    fn egress_admission_refuses_over_ceiling_and_counts() {
        let ceiling = 128 * 1024usize;
        let mut refused = 0u64;
        let mut slot = 64usize;
        // First over-ceiling frame: refused, count → 1, slot untouched.
        assert_eq!(
            decide_channel_egress_admission(ceiling + 1, ceiling, &mut slot, || {
                refused += 1;
                refused
            }),
            ChannelEgressAdmission::RefusedOverCeiling { refused_count: 1 }
        );
        assert_eq!(slot, 64, "a refusal must not grow the tracked slot");
        // Second over-ceiling frame: count keeps climbing.
        assert_eq!(
            decide_channel_egress_admission(ceiling + 999, ceiling, &mut slot, || {
                refused += 1;
                refused
            }),
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
        match decide_channel_egress_admission(20_000, ceiling, &mut slot, || {
            refused += 1;
            refused
        }) {
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
        assert_eq!(refused, 0, "an admitted frame records no refusal");
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
        match decide_channel_egress_admission(40_000, ceiling, &mut slot, || {
            refused += 1;
            refused
        }) {
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
        match decide_channel_egress_admission(100_000, ceiling, &mut slot, || {
            refused += 1;
            refused
        }) {
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
            decide_channel_egress_admission(4096, ceiling, &mut slot, || {
                refused += 1;
                refused
            }),
            ChannelEgressAdmission::Admitted { grew_to: None }
        );
        assert_eq!(
            slot, 65_536,
            "an in-slot frame leaves the tracked slot as-is"
        );
        assert_eq!(refused, 0, "an admitted frame records no refusal");
    }
}
