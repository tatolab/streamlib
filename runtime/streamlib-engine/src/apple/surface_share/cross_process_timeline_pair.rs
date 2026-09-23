// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine's `produce_done` / `consume_done` timeline pair for one surface
//! a helper process shares, and how it orders on macOS.
//!
//! The pair crosses as two Metal shared events. When either end cannot carry
//! one — the export or the helper's import yields nothing — the same pair
//! orders host-side instead, decided at runtime: the producer host-waits its
//! own GPU work before the hand-off, and the helper reports its completion
//! as a message the service turns into a host signal.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::RwLock;
use streamlib_surface_client::OwnedMachSendRight;

use crate::core::rhi::pool_slot_key_of_surface_id;
use crate::core::{Error, Result};
use crate::vulkan::rhi::HostVulkanTimelineSemaphore;

/// Upper bound on any engine wait for a value a helper process signals.
///
/// IOGPU kills a committed command buffer whose wait on a shared event is not
/// satisfied within about 5 s, and MoltenVK then marks the `VkDevice` lost for
/// good. So the engine never puts a device-side wait on a helper's value; it
/// host-waits at most this long before the submit, and past it signals the
/// value itself.
pub const CROSS_PROCESS_TIMELINE_WAIT_BOUND: Duration = Duration::from_secs(2);

/// How a wait for a helper's release of a frame ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerReleaseOutcome {
    /// The helper signalled the value.
    Released,
    /// The helper did not signal within [`CROSS_PROCESS_TIMELINE_WAIT_BOUND`];
    /// the engine signalled the value itself, and the frame the helper held is
    /// stale.
    ForcedPastAStalledConsumer,
}

/// One surface's engine-owned timeline pair, and whether it orders host-side.
pub struct CrossProcessTimelinePair {
    produce_done: Arc<HostVulkanTimelineSemaphore>,
    consume_done: Arc<HostVulkanTimelineSemaphore>,
    orders_host_side: AtomicBool,
}

impl CrossProcessTimelinePair {
    /// A pair ordering through shared events until an export or an import
    /// refuses.
    pub fn new(
        produce_done: Arc<HostVulkanTimelineSemaphore>,
        consume_done: Arc<HostVulkanTimelineSemaphore>,
    ) -> Self {
        Self {
            produce_done,
            consume_done,
            orders_host_side: AtomicBool::new(false),
        }
    }

    /// The timeline the engine's GPU work signals when a frame is produced.
    pub fn produce_done(&self) -> &Arc<HostVulkanTimelineSemaphore> {
        &self.produce_done
    }

    /// The timeline a helper signals when it has released a frame.
    pub fn consume_done(&self) -> &Arc<HostVulkanTimelineSemaphore> {
        &self.consume_done
    }

    /// Whether this pair orders host-side rather than through shared events.
    pub fn orders_host_side(&self) -> bool {
        self.orders_host_side.load(Ordering::Acquire)
    }

    /// Order host-side from now on. Never reverts: a helper that could not
    /// import stays unable to.
    pub fn fall_back_to_host_side_ordering(&self, reason: &str) {
        if !self.orders_host_side.swap(true, Ordering::AcqRel) {
            tracing::warn!(
                reason,
                "a cross-process timeline pair falls back to host-side ordering"
            );
        }
    }

    /// Send rights to both timelines' shared events, or `None` — having
    /// fallen back to host-side ordering — when either will not export.
    pub fn exported_mach_send_rights_or_host_side_fallback(
        &self,
    ) -> Option<(OwnedMachSendRight, OwnedMachSendRight)> {
        let exported = self
            .produce_done
            .export_metal_shared_event_mach_send_right()
            .and_then(|produce_done| {
                self.consume_done
                    .export_metal_shared_event_mach_send_right()
                    .map(|consume_done| (produce_done, consume_done))
            });
        match exported {
            Ok(send_rights) => Some(send_rights),
            Err(refusal) => {
                self.fall_back_to_host_side_ordering(&refusal.to_string());
                None
            }
        }
    }

    /// Call before handing a helper the frame whose GPU work signals
    /// `produce_done` at `value`. Host-side, the frame is handed off only once
    /// that work is done; through shared events the helper waits on the
    /// timeline itself, and this returns at once.
    pub fn complete_production_before_hand_off(&self, value: u64) -> Result<()> {
        if !self.orders_host_side() {
            return Ok(());
        }
        self.produce_done
            .wait(value, CROSS_PROCESS_TIMELINE_WAIT_BOUND.as_nanos() as u64)
    }

    /// Wait, bounded, for a helper to release the frame it signals
    /// `consume_done` at `value` for. The one sanctioned engine wait on a
    /// helper's value on macOS: host-side, ahead of any submit that reuses
    /// the frame, never a device-side wait.
    pub fn wait_for_consumer_release(&self, value: u64) -> Result<ConsumerReleaseOutcome> {
        if self
            .consume_done
            .wait(value, CROSS_PROCESS_TIMELINE_WAIT_BOUND.as_nanos() as u64)
            .is_ok()
        {
            return Ok(ConsumerReleaseOutcome::Released);
        }
        // A driver error rather than a timeout surfaces here too; either
        // way the counter below `value` is what must not block the engine.
        self.advance_consume_done_to_at_least(value)?;
        tracing::warn!(
            value,
            bound_ms = CROSS_PROCESS_TIMELINE_WAIT_BOUND.as_millis() as u64,
            "a helper did not release a frame within the bound; the engine signalled \
             consume_done itself and the frame is stale"
        );
        Ok(ConsumerReleaseOutcome::ForcedPastAStalledConsumer)
    }

    /// A helper's host-side report that it released the frame at `value`.
    /// A value the counter already reached — the engine forced it past a
    /// stall — is not signalled again.
    pub fn record_consumer_release_reported_over_the_channel(&self, value: u64) -> Result<()> {
        self.advance_consume_done_to_at_least(value)
    }

    fn advance_consume_done_to_at_least(&self, value: u64) -> Result<()> {
        if self.consume_done.current_value()? >= value {
            return Ok(());
        }
        self.consume_done.signal_host(value)
    }
}

/// The engine's timeline pairs by surface id, shared between the surface
/// store that registers them and the Mach service that answers helpers'
/// host-side reports against them.
#[derive(Default)]
pub struct CrossProcessTimelinePairsBySurface {
    pairs: RwLock<HashMap<String, Arc<CrossProcessTimelinePair>>>,
}

impl CrossProcessTimelinePairsBySurface {
    /// Record `pair` as `surface_id`'s, replacing any earlier one.
    pub fn insert(&self, surface_id: &str, pair: Arc<CrossProcessTimelinePair>) {
        self.pairs.write().insert(surface_id.to_string(), pair);
    }

    /// The pair behind `surface_id`; a published frame id resolves through
    /// its pool slot.
    pub fn pair_of(&self, surface_id: &str) -> Option<Arc<CrossProcessTimelinePair>> {
        self.pairs
            .read()
            .get(pool_slot_key_of_surface_id(surface_id))
            .cloned()
    }

    /// Forget `surface_id`'s pair.
    pub fn remove(&self, surface_id: &str) {
        self.pairs
            .write()
            .remove(pool_slot_key_of_surface_id(surface_id));
    }

    /// The pair behind `surface_id`, or the refusal a helper's report gets.
    pub fn pair_or_refusal(&self, surface_id: &str) -> Result<Arc<CrossProcessTimelinePair>> {
        self.pair_of(surface_id).ok_or_else(|| {
            Error::GpuError(format!(
                "surface '{surface_id}' has no engine timeline pair to report against"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vulkan::rhi::HostVulkanDevice;

    fn a_pair(device: &HostVulkanDevice) -> CrossProcessTimelinePair {
        CrossProcessTimelinePair::new(
            Arc::new(HostVulkanTimelineSemaphore::new_exportable(device.device(), 0).unwrap()),
            Arc::new(HostVulkanTimelineSemaphore::new_exportable(device.device(), 0).unwrap()),
        )
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_stalled_consumer_is_forced_past_within_the_bound_and_a_late_report_is_harmless() {
        let device = HostVulkanDevice::new().expect("the rig must produce a Vulkan device");
        let pair = a_pair(&device);

        let started = std::time::Instant::now();
        let outcome = pair.wait_for_consumer_release(1).expect("bounded wait");
        let waited = started.elapsed();

        assert_eq!(outcome, ConsumerReleaseOutcome::ForcedPastAStalledConsumer);
        assert!(waited >= CROSS_PROCESS_TIMELINE_WAIT_BOUND);
        assert!(waited < Duration::from_secs(4), "waited {waited:?}");
        assert_eq!(pair.consume_done().current_value().unwrap(), 1);
        pair.record_consumer_release_reported_over_the_channel(1)
            .expect("a late report of a forced value is not an error");
        assert_eq!(pair.consume_done().current_value().unwrap(), 1);
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_reported_release_ends_the_wait_without_forcing() {
        let device = HostVulkanDevice::new().expect("the rig must produce a Vulkan device");
        let pair = a_pair(&device);
        pair.record_consumer_release_reported_over_the_channel(3)
            .expect("the report signals");
        assert_eq!(
            pair.wait_for_consumer_release(3).expect("wait"),
            ConsumerReleaseOutcome::Released
        );
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_timeline_that_will_not_export_falls_the_pair_back_to_host_side() {
        let device = HostVulkanDevice::new().expect("the rig must produce a Vulkan device");
        let exportable = a_pair(&device);
        assert!(
            exportable
                .exported_mach_send_rights_or_host_side_fallback()
                .is_some()
        );
        assert!(!exportable.orders_host_side());

        let unexportable = CrossProcessTimelinePair::new(
            Arc::new(HostVulkanTimelineSemaphore::new(device.device(), 0).unwrap()),
            Arc::new(HostVulkanTimelineSemaphore::new(device.device(), 0).unwrap()),
        );
        assert!(
            unexportable
                .exported_mach_send_rights_or_host_side_fallback()
                .is_none()
        );
        assert!(unexportable.orders_host_side());
    }
}
