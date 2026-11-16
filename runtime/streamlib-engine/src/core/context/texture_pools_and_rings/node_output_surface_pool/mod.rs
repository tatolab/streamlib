// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::context::SurfaceCheckOutLeaseRegistry;
use crate::core::context::texture_pools_and_rings::lease_aware_pool_slot_ring::{
    ANY_ONE_UNHELD_SLOT_IS_REUSABLE, LeaseAwarePoolMintedFrameGenerations, LeaseAwarePoolSlot,
    LeaseAwarePoolSlotResource, LeaseAwarePoolSlotRing,
};
use std::collections::HashMap;

use parking_lot::Mutex;

use crate::core::{Error, Result};

pub(crate) const NODE_OUTPUT_SURFACE_POOL_CAPACITY: usize = 16;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct OutputFramesKeptByOutputLinks {
    pub(crate) queued_in_their_mailboxes: usize,
    pub(crate) borrowed_past_their_mailboxes: usize,
}

#[derive(Debug, Default)]
pub(crate) struct OutputFramesKeptByTheOutputLinksOfOneNode {
    kept_by_output_link_id: Mutex<HashMap<String, OutputFramesKeptByOutputLinks>>,
}

impl OutputFramesKeptByTheOutputLinksOfOneNode {
    /// Record a wired link whose reader's mailbox holds `mailbox_depth` frames
    /// and who may borrow `borrow_quota` past it; rewiring it replaces what it
    /// kept.
    pub(crate) fn note_a_wired_output_link(
        &self,
        link_id: &str,
        mailbox_depth: usize,
        borrow_quota: usize,
    ) {
        self.kept_by_output_link_id.lock().insert(
            link_id.to_string(),
            OutputFramesKeptByOutputLinks {
                queued_in_their_mailboxes: mailbox_depth,
                borrowed_past_their_mailboxes: borrow_quota,
            },
        );
    }

    /// Forget an unwired link; the pools shrink to the rest lazily.
    pub(crate) fn forget_an_unwired_output_link(&self, link_id: &str) {
        self.kept_by_output_link_id.lock().remove(link_id);
    }

    pub(crate) fn kept_by_every_output_link(&self) -> OutputFramesKeptByOutputLinks {
        self.kept_by_output_link_id.lock().values().fold(
            OutputFramesKeptByOutputLinks::default(),
            |sum, kept| OutputFramesKeptByOutputLinks {
                queued_in_their_mailboxes: sum.queued_in_their_mailboxes
                    + kept.queued_in_their_mailboxes,
                borrowed_past_their_mailboxes: sum.borrowed_past_their_mailboxes
                    + kept.borrowed_past_their_mailboxes,
            },
        )
    }
}

pub(crate) enum NodeOutputSurfacePoolHandOff<'pool, Resource> {
    /// An unheld slot, republished under a freshly minted generation.
    ReusedSlot(&'pool LeaseAwarePoolSlot<Resource>),
    NeedsAFreshSlot,
}

/// A fresh slot the pool refused at its capacity, handed back to the caller
/// that allocated it.
pub(crate) struct NodeOutputSurfacePoolRefusedFreshSlot<Resource> {
    pub(crate) refusal: Error,
    pub(crate) pool_slot_key: String,
    pub(crate) resource: Resource,
}

/// A lent slot the pool refused at its capacity, handed back to its lender.
pub(crate) struct NodeOutputSurfacePoolRefusedLentSlot<Resource> {
    pub(crate) refusal: Error,
    pub(crate) slot: Box<LeaseAwarePoolSlot<Resource>>,
}

pub(crate) struct NodeOutputSurfacePool<Resource> {
    pool_key: String,
    ring: LeaseAwarePoolSlotRing<Resource>,
    kept_by_its_output_links: OutputFramesKeptByOutputLinks,
    slot_capacity: usize,
}

impl<Resource: LeaseAwarePoolSlotResource> NodeOutputSurfacePool<Resource> {
    /// An empty pool; its slots are allocated by the hand-offs that need them.
    pub(crate) fn new(pool_key: String) -> Self {
        Self {
            pool_key,
            ring: LeaseAwarePoolSlotRing::default(),
            kept_by_its_output_links: OutputFramesKeptByOutputLinks::default(),
            slot_capacity: NODE_OUTPUT_SURFACE_POOL_CAPACITY,
        }
    }

    /// Size the next hand-offs for what the links on the producer's outputs
    /// keep of its frames.
    pub(crate) fn keep_room_for_what_its_output_links_keep(
        &mut self,
        kept_by_its_output_links: OutputFramesKeptByOutputLinks,
    ) {
        self.kept_by_its_output_links = kept_by_its_output_links;
    }

    pub(crate) fn hand_off_a_reusable_frame(
        &mut self,
        rotation_depth: usize,
        check_out_leases: Option<&SurfaceCheckOutLeaseRegistry>,
        minted_frame_generations: &LeaseAwarePoolMintedFrameGenerations,
    ) -> Result<NodeOutputSurfacePoolHandOff<'_, Resource>> {
        if rotation_depth == 0 || rotation_depth > NODE_OUTPUT_SURFACE_POOL_CAPACITY {
            return Err(Error::Configuration(format!(
                "node output pool '{}' was asked for a rotation depth of {rotation_depth}; \
                 it must be at least 1 and at most the pool's capacity of \
                 {NODE_OUTPUT_SURFACE_POOL_CAPACITY}",
                self.pool_key
            )));
        }
        let unheld_slot_reserve = rotation_depth
            + self.kept_by_its_output_links.queued_in_their_mailboxes
            + self.kept_by_its_output_links.borrowed_past_their_mailboxes;
        self.slot_capacity = NODE_OUTPUT_SURFACE_POOL_CAPACITY
            .max(unheld_slot_reserve + self.kept_by_its_output_links.borrowed_past_their_mailboxes);
        let slot_count = self.ring.slot_count();
        let pool_is_at_its_cap = slot_count >= self.slot_capacity;
        let unheld_slots_a_reuse_needs = if pool_is_at_its_cap {
            ANY_ONE_UNHELD_SLOT_IS_REUSABLE
        } else {
            unheld_slot_reserve
        };
        if slot_count >= unheld_slot_reserve
            && let Some(slot_index) = self.ring.hand_off_a_reusable_slot(
                unheld_slots_a_reuse_needs,
                check_out_leases,
                minted_frame_generations,
            )
        {
            return Ok(NodeOutputSurfacePoolHandOff::ReusedSlot(
                self.ring.slot(slot_index),
            ));
        }
        if pool_is_at_its_cap {
            return Err(self.every_slot_in_use_refusal());
        }
        Ok(NodeOutputSurfacePoolHandOff::NeedsAFreshSlot)
    }

    /// Add a slot allocated after [`Self::hand_off_a_reusable_frame`] asked
    /// for one, and hand it out under its first generation; at the pool's
    /// capacity the slot is handed back.
    pub(crate) fn hand_off_a_fresh_slot(
        &mut self,
        pool_slot_key: String,
        resource: Resource,
        check_out_leases: Option<&SurfaceCheckOutLeaseRegistry>,
        minted_frame_generations: &LeaseAwarePoolMintedFrameGenerations,
    ) -> std::result::Result<
        &LeaseAwarePoolSlot<Resource>,
        NodeOutputSurfacePoolRefusedFreshSlot<Resource>,
    > {
        if self.ring.slot_count() >= self.slot_capacity {
            return Err(NodeOutputSurfacePoolRefusedFreshSlot {
                refusal: self.every_slot_in_use_refusal(),
                pool_slot_key,
                resource,
            });
        }
        let slot_index = self.ring.push_fresh_slot(pool_slot_key, resource);
        self.ring
            .hand_off_fresh_slot(slot_index, check_out_leases, minted_frame_generations);
        Ok(self.ring.slot(slot_index))
    }

    /// Add a slot its lender already republished under a fresh generation,
    /// after [`Self::hand_off_a_reusable_frame`] asked for one, and hand it
    /// out under that generation; at the pool's capacity the slot is handed
    /// back.
    pub(crate) fn hand_off_a_slot_its_lender_already_republished(
        &mut self,
        slot: LeaseAwarePoolSlot<Resource>,
    ) -> std::result::Result<
        &LeaseAwarePoolSlot<Resource>,
        NodeOutputSurfacePoolRefusedLentSlot<Resource>,
    > {
        if self.ring.slot_count() >= self.slot_capacity {
            return Err(NodeOutputSurfacePoolRefusedLentSlot {
                refusal: self.every_slot_in_use_refusal(),
                slot: Box::new(slot),
            });
        }
        let slot_index = self
            .ring
            .push_a_slot_keeping_its_published_frame_generation(slot);
        Ok(self.ring.slot(slot_index))
    }

    fn every_slot_in_use_refusal(&self) -> Error {
        tracing::warn!(
            "node output pool '{}': all {} slots are held by consumers; the producer \
             drops this frame",
            self.pool_key,
            self.ring.slot_count()
        );
        Error::EverySlotInTheNodeOutputPoolIsInUse {
            pool_key: self.pool_key.clone(),
            pool_capacity: self.slot_capacity,
        }
    }

    /// How many slots the pool holds.
    #[cfg(test)]
    pub(crate) fn slot_count(&self) -> usize {
        self.ring.slot_count()
    }

    /// Every slot, for teardown.
    pub(crate) fn into_slots(self) -> impl Iterator<Item = LeaseAwarePoolSlot<Resource>> {
        self.ring.into_slots()
    }
}

#[cfg(test)]
mod tests;
