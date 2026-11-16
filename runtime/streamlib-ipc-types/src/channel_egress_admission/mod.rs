// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    TRUSTED_CHANNEL_CHUNK_CEILING_BYTES, UNTRUSTED_SESSION_CHANNEL_CHUNK_CEILING_BYTES,
    iceoryx2_sample_bytes_for_a_channel_frame, largest_channel_frame_bytes_under_a_chunk_ceiling,
};

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

#[cfg(test)]
mod tests;
