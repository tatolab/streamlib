// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The processor output texture pools one helper process publishes kernel
//! outputs from, each under the pool key its ring minted.

use std::collections::HashMap;

use super::RegisteredHandle;
use crate::core::context::GpuContextFullAccess;
use crate::core::context::SurfaceCheckOutLeaseRegistry;
use crate::core::context::lease_aware_pool_slot_ring::{
    LeaseAwarePoolMintedFrameGenerations, LeaseAwarePoolSlot, LeaseAwarePoolSlotResource,
};
use crate::core::context::processor_output_surface_pool::ProcessorOutputSurfacePool;
use crate::core::rhi::{TextureFormat, TextureUsages};
use crate::core::{Error, Result};

/// The allocation every slot of one processor output texture pool shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessorOutputTextureDescriptor {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: TextureFormat,
    pub(crate) usage: TextureUsages,
}

/// One slot of a processor output texture pool: the texture, registered with
/// the surface-share service and the parent's texture cache under the slot key.
pub(crate) struct ProcessorOutputTextureSlot {
    registered_texture: RegisteredHandle,
    /// The texture's handle count once allocation and registration settled —
    /// every share the engine itself keeps. A count above it is a reader.
    texture_strong_count_with_no_holder: usize,
}

/// The handle count of a pooled texture's host `Arc`; `None` for any other
/// kind of registered handle, which no processor output pool may hold.
fn pooled_texture_strong_count(registered_texture: &RegisteredHandle) -> Option<usize> {
    match registered_texture {
        RegisteredHandle::Texture { texture, .. } => Some(texture.texture().strong_count()),
        _ => None,
    }
}

impl LeaseAwarePoolSlotResource for ProcessorOutputTextureSlot {
    fn is_held_in_this_process(&self) -> bool {
        pooled_texture_strong_count(&self.registered_texture)
            .is_some_and(|strong_count| strong_count > self.texture_strong_count_with_no_holder)
    }
}

type ProcessorOutputTexturePool = ProcessorOutputSurfacePool<ProcessorOutputTextureSlot>;

/// Pool key → the descriptor its slots were allocated for, and the pool.
#[derive(Default)]
pub(crate) struct ProcessorOutputTexturePoolsOfOneHelper {
    pools_by_key: HashMap<String, (ProcessorOutputTextureDescriptor, ProcessorOutputTexturePool)>,
    /// Slots of pools a descriptor change replaced, kept until nothing holds
    /// them: releasing a held slot would hand its texture back to the texture
    /// pool, and another acquire could rewrite it under its reader.
    retiring_slots: Vec<LeaseAwarePoolSlot<ProcessorOutputTextureSlot>>,
}

/// A slot this helper's pools no longer hold, owed the release every
/// registered texture is owed.
pub(crate) struct ReleasedProcessorOutputTextureSlot {
    /// The key the slot's texture is registered under — never a frame id.
    pub(crate) pool_slot_key: String,
    pub(crate) registered_texture: RegisteredHandle,
}

impl ProcessorOutputTexturePoolsOfOneHelper {
    /// Hand out the next frame of the pool under `pool_key`, answering its
    /// published id and every slot a descriptor change retired.
    ///
    /// A request whose descriptor differs from the pool's replaces the pool: a
    /// ring whose extent changed publishes into slots the new size, never into
    /// the old ones. `allocate_fresh_slot` allocates and registers one texture
    /// and answers the slot key it is registered under.
    pub(crate) fn hand_off_next_frame(
        &mut self,
        full: &GpuContextFullAccess,
        pool_key: &str,
        rotation_depth: usize,
        descriptor: ProcessorOutputTextureDescriptor,
        allocate_fresh_slot: impl FnOnce() -> Result<(String, RegisteredHandle)>,
    ) -> Result<(String, Vec<ReleasedProcessorOutputTextureSlot>)> {
        let host = full.host_inner();
        let surface_store = host.surface_store();
        let check_out_leases = surface_store
            .as_ref()
            .and_then(|store| store.check_out_leases())
            .map(|leases| leases.as_ref());
        let minted_frame_generations = host.lease_aware_pool_minted_frame_generations();
        if let Some((existing_descriptor, _)) = self.pools_by_key.get(pool_key)
            && *existing_descriptor != descriptor
            && let Some((_, replaced_pool)) = self.pools_by_key.remove(pool_key)
        {
            self.retiring_slots.extend(replaced_pool.into_slots());
        }
        let released_slots =
            self.release_retiring_slots_nobody_holds(check_out_leases, minted_frame_generations);
        let (_, pool) = self
            .pools_by_key
            .entry(pool_key.to_string())
            .or_insert_with(|| {
                (
                    descriptor,
                    ProcessorOutputSurfacePool::new(pool_key.to_string()),
                )
            });
        let published = pool
            .hand_off_next_frame(
                rotation_depth,
                check_out_leases,
                minted_frame_generations,
                || {
                    let (pool_slot_key, registered_texture) = allocate_fresh_slot()?;
                    let Some(texture_strong_count_with_no_holder) =
                        pooled_texture_strong_count(&registered_texture)
                    else {
                        return Err(Error::GpuError(format!(
                            "processor output pool '{pool_key}' was handed a slot that is not \
                             a pooled texture"
                        )));
                    };
                    Ok((
                        pool_slot_key,
                        ProcessorOutputTextureSlot {
                            registered_texture,
                            texture_strong_count_with_no_holder,
                        },
                    ))
                },
            )?
            .currently_published_frame_id();
        Ok((published, released_slots))
    }

    /// Every retiring slot nobody holds any longer, with every frame it
    /// published retired first — under the lease hand-off guard, so a checkout
    /// of one of its ids lands strictly before the test (and keeps the slot) or
    /// strictly after the retire (and is refused as recycled).
    fn release_retiring_slots_nobody_holds(
        &mut self,
        check_out_leases: Option<&SurfaceCheckOutLeaseRegistry>,
        minted_frame_generations: &LeaseAwarePoolMintedFrameGenerations,
    ) -> Vec<ReleasedProcessorOutputTextureSlot> {
        if self.retiring_slots.is_empty() {
            return Vec::new();
        }
        let mut lease_hand_off =
            match check_out_leases.map(|leases| leases.hold_for_pool_slot_hand_off()) {
                None => None,
                Some(Some(hand_off)) => Some(hand_off),
                // An unreadable lease table proves no slot free; keep them all.
                Some(None) => return Vec::new(),
            };
        let (unheld_slots, still_held_slots): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.retiring_slots)
                .into_iter()
                .partition(|slot| {
                    !slot.resource().is_held_in_this_process()
                        && lease_hand_off.as_ref().is_none_or(|hand_off| {
                            !hand_off.is_checked_out_by_any_holder(slot.pool_slot_key())
                        })
                });
        self.retiring_slots = still_held_slots;
        for slot in &unheld_slots {
            let generation_past_every_published_frame = slot.published_frame_generation() + 1;
            if let Some(hand_off) = lease_hand_off.as_mut() {
                hand_off.publish_frame_generation(
                    slot.pool_slot_key(),
                    generation_past_every_published_frame,
                );
            }
            minted_frame_generations.retire_every_published_frame_of_slot(
                slot.pool_slot_key(),
                generation_past_every_published_frame,
            );
        }
        unheld_slots.into_iter().map(released_slot).collect()
    }

    /// Every slot of every pool, retiring ones included, for teardown.
    pub(crate) fn drain_slots(&mut self) -> Vec<ReleasedProcessorOutputTextureSlot> {
        self.pools_by_key
            .drain()
            .flat_map(|(_, (_, pool))| pool.into_slots())
            .chain(std::mem::take(&mut self.retiring_slots))
            .map(released_slot)
            .collect()
    }
}

fn released_slot(
    slot: LeaseAwarePoolSlot<ProcessorOutputTextureSlot>,
) -> ReleasedProcessorOutputTextureSlot {
    ReleasedProcessorOutputTextureSlot {
        pool_slot_key: slot.pool_slot_key().to_string(),
        registered_texture: slot.into_resource().registered_texture,
    }
}
