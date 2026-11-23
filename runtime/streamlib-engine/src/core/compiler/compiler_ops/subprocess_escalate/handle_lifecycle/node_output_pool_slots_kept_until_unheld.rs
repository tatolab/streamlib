// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::node_output_pools_of_one_helper::{
    NodeOutputPoolSlot, ReleasedNodeOutputPoolSlot, released_slot,
};
use super::release_node_output_pool_slot;
use crate::core::context::texture_pools_and_rings::lease_aware_pool_slot_ring::{
    LeaseAwarePoolMintedFrameGenerations, LeaseAwarePoolSlot, LeaseAwarePoolSlotResource,
};
use crate::core::context::texture_pools_and_rings::slots_outliving_their_producers::SlotsAwaitingTheirReaders;
use crate::core::context::{GpuContextLimitedAccess, SurfaceCheckOutLeaseRegistry, SurfaceStore};
use streamlib_ipc_types::GpuPoolSlotRehandTest;

/// Node output pool slots no pool hands out any longer, kept until nothing
/// holds them: releasing a held slot would hand its resource back to its
/// allocator, where another acquire could rewrite it under its reader, and
/// would take its registration from a reader that binds it again.
#[derive(Default)]
pub(crate) struct NodeOutputPoolSlotsKeptUntilUnheld {
    slots: Vec<LeaseAwarePoolSlot<NodeOutputPoolSlot>>,
}

impl NodeOutputPoolSlotsKeptUntilUnheld {
    pub(crate) fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Flag each slot's shared pool entry freed once nothing holds it, rather
    /// than handed back for reuse: its producer is gone.
    pub(crate) fn flag_each_shared_pool_entry_freed_as_soon_as_unheld(
        &self,
        sandbox: &GpuContextLimitedAccess,
    ) {
        for slot in &self.slots {
            slot.resource()
                .registered_handle()
                .flag_its_shared_pool_entry_freed_as_soon_as_unheld(sandbox);
        }
    }

    pub(crate) fn keep(
        &mut self,
        slots: impl IntoIterator<Item = LeaseAwarePoolSlot<NodeOutputPoolSlot>>,
    ) {
        self.slots.extend(slots);
    }

    /// Every slot kept here that nobody holds any longer, with every frame it
    /// published retired first — under the lease hand-off guard, so a checkout
    /// of one of its ids lands strictly before the test (and keeps the slot) or
    /// strictly after the retire (and is refused as recycled).
    pub(crate) fn take_each_slot_nobody_holds(
        &mut self,
        check_out_leases: Option<&SurfaceCheckOutLeaseRegistry>,
        minted_frame_generations: &LeaseAwarePoolMintedFrameGenerations,
    ) -> Vec<ReleasedNodeOutputPoolSlot> {
        if self.slots.is_empty() {
            return Vec::new();
        }
        let mut lease_hand_off =
            match check_out_leases.map(|leases| leases.hold_for_pool_slot_hand_off()) {
                None => None,
                Some(Some(hand_off)) => Some(hand_off),
                // An unreadable lease table proves no slot free; keep them all.
                Some(None) => return Vec::new(),
            };
        let mut minted_frame_generations_held = minted_frame_generations.held_for_a_hand_off();
        let (unheld_slots, still_held_slots): (Vec<_>, Vec<_>) = std::mem::take(&mut self.slots)
            .into_iter()
            .partition(|slot| {
                !slot.resource().is_held_in_this_process()
                    && lease_hand_off.as_ref().is_none_or(|hand_off| {
                        !hand_off.is_checked_out_by_any_holder(slot.pool_slot_key())
                    })
                    && slot
                        .resource()
                        .gpu_pool_claim_table_row()
                        .is_none_or(|claim_table_row| {
                            claim_table_row
                                .row()
                                .retire_for_a_rehand(slot.published_frame_generation())
                                == GpuPoolSlotRehandTest::RetiredForTheRehand
                        })
            });
        self.slots = still_held_slots;
        for slot in &unheld_slots {
            let generation_past_every_published_frame = slot.published_frame_generation() + 1;
            if let Some(hand_off) = lease_hand_off.as_mut() {
                hand_off.publish_frame_generation(
                    slot.pool_slot_key(),
                    generation_past_every_published_frame,
                );
            }
            minted_frame_generations_held.retire_every_published_frame_of_slot(
                slot.pool_slot_key(),
                generation_past_every_published_frame,
            );
        }
        unheld_slots.into_iter().map(released_slot).collect()
    }
}

/// The slots a torn-down helper left behind, released through its stream's
/// sandbox as their readers let go.
impl SlotsAwaitingTheirReaders for NodeOutputPoolSlotsKeptUntilUnheld {
    fn release_each_one_nobody_holds(&mut self, sandbox: &GpuContextLimitedAccess) -> bool {
        let host = sandbox.host_inner();
        let surface_store = host.surface_store();
        let unheld_slots = self.take_each_slot_nobody_holds(
            surface_store
                .as_ref()
                .and_then(SurfaceStore::check_out_lease_registry),
            host.lease_aware_pool_minted_frame_generations(),
        );
        for unheld_slot in unheld_slots {
            release_node_output_pool_slot(sandbox, unheld_slot);
        }
        !self.is_empty()
    }

    fn release_every_one(self: Box<Self>, sandbox: &GpuContextLimitedAccess) {
        for slot in self.slots {
            release_node_output_pool_slot(sandbox, released_slot(slot));
        }
    }
}
