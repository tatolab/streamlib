// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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

pub const MAX_INBOUND_LINKS_PER_DESTINATION: usize = 256;
