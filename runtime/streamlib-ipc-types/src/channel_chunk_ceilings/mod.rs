// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    DATA_CHANNEL_PAYLOAD_ALIGNMENT_BYTES, DataChannelPayloadCacheLine, DataChannelSampleUserHeader,
    data_channel_payload_cache_lines_for,
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

#[cfg(test)]
mod tests;
