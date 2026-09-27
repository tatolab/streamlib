// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The processor output pools one helper process publishes kernel outputs
//! from — textures and tensor storage buffers alike — each under the pool key
//! its ring minted.

use std::collections::HashMap;

use super::RegisteredHandle;
use crate::core::context::lease_aware_pool_slot_ring::{
    LeaseAwarePoolMintedFrameGenerations, LeaseAwarePoolSlot, LeaseAwarePoolSlotResource,
};
use crate::core::context::processor_output_surface_pool::{
    ProcessorOutputSurfacePool, ProcessorOutputSurfacePoolHandOff,
    ProcessorOutputSurfacePoolRefusedFreshSlot,
};
use crate::core::context::{GpuContext, SurfaceCheckOutLeaseRegistry, SurfaceStore};
use crate::core::rhi::{TensorStorageBufferLayout, TextureFormat, TextureUsages};
use crate::core::{Error, Result};

/// The allocation every slot of one processor output texture pool shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessorOutputTextureDescriptor {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: TextureFormat,
    pub(crate) usage: TextureUsages,
}

/// The allocation every slot of one processor output pool shares: a texture,
/// or a tensor storage buffer of one shape and element type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProcessorOutputSlotDescriptor {
    Texture(ProcessorOutputTextureDescriptor),
    StorageBuffer(TensorStorageBufferLayout),
}

/// One slot of a processor output pool: the texture or storage buffer,
/// registered with the surface-share service and the parent's texture cache or
/// storage buffer map under the slot key.
pub(crate) struct ProcessorOutputPoolSlot {
    registered_handle: RegisteredHandle,
    /// The resource's handle count once allocation and registration settled —
    /// every share the engine itself keeps. A count above it is a reader.
    strong_count_with_no_holder: usize,
}

/// The handle count of a pooled resource's host `Arc`; `None` for any kind of
/// registered handle no processor output pool may hold.
fn pooled_resource_strong_count(registered_handle: &RegisteredHandle) -> Option<usize> {
    match registered_handle {
        RegisteredHandle::Texture { texture, .. } => Some(texture.texture().strong_count()),
        #[cfg(target_os = "linux")]
        RegisteredHandle::StorageBuffer { buffer } => Some(buffer.strong_count()),
        _ => None,
    }
}

impl LeaseAwarePoolSlotResource for ProcessorOutputPoolSlot {
    fn is_held_in_this_process(&self) -> bool {
        pooled_resource_strong_count(&self.registered_handle)
            .is_some_and(|strong_count| strong_count > self.strong_count_with_no_holder)
    }
}

type ProcessorOutputPool = ProcessorOutputSurfacePool<ProcessorOutputPoolSlot>;

/// Pool key → the descriptor its slots were allocated for, and the pool.
#[derive(Default)]
pub(crate) struct ProcessorOutputPoolsOfOneHelper {
    pools_by_key: HashMap<String, (ProcessorOutputSlotDescriptor, ProcessorOutputPool)>,
    /// Slots of pools a descriptor change replaced, kept until nothing holds
    /// them: releasing a held slot would hand its resource back to its
    /// allocator, and another acquire could rewrite it under its reader.
    retiring_slots: Vec<LeaseAwarePoolSlot<ProcessorOutputPoolSlot>>,
    /// Set by the helper's teardown drain: nothing drains the pools again, so
    /// a slot handed in after it would never be released.
    drained_at_the_helpers_teardown: bool,
}

/// What one helper's pools answer when asked for a pool's next frame without
/// allocating.
pub(crate) enum ProcessorOutputFrameHandOff {
    /// A reused slot's frame, under its published id.
    Published(String),
    /// The pool must grow: allocate a slot and hand it to
    /// [`ProcessorOutputPoolsOfOneHelper::hand_off_a_fresh_slot`].
    NeedsAFreshSlot,
}

/// What one helper's pools answer when handed a freshly allocated slot.
pub(crate) enum ProcessorOutputFreshSlotHandOff {
    /// The fresh slot's first frame, under its published id.
    Published(String),
    /// The pools refused the slot — the helper was torn down while it was
    /// allocated, the pool is at its capacity, or it is not a poolable
    /// resource — and it is owed its release.
    Refused {
        refusal: Error,
        slot_owed_its_release: ReleasedProcessorOutputPoolSlot,
    },
}

/// A slot this helper's pools no longer hold, owed the release every
/// registered handle of its kind is owed.
pub(crate) struct ReleasedProcessorOutputPoolSlot {
    /// The key the slot's resource is registered under — never a frame id.
    pub(crate) pool_slot_key: String,
    pub(crate) registered_handle: RegisteredHandle,
}

impl ProcessorOutputPoolsOfOneHelper {
    /// Hand out the next frame of the pool under `pool_key` when it needs no
    /// fresh slot, answering — refused or not — every slot a descriptor change
    /// retired, each owed its release.
    ///
    /// A request whose descriptor differs from the pool's replaces the pool: a
    /// ring whose extent changed publishes into slots the new size, never into
    /// the old ones. Nothing here calls into Vulkan, so it runs outside the
    /// escalate scope.
    pub(crate) fn hand_off_a_reusable_frame(
        &mut self,
        host: &GpuContext,
        pool_key: &str,
        rotation_depth: usize,
        descriptor: &ProcessorOutputSlotDescriptor,
    ) -> (
        Result<ProcessorOutputFrameHandOff>,
        Vec<ReleasedProcessorOutputPoolSlot>,
    ) {
        if let Err(refusal) = self.refuse_after_the_helpers_teardown(pool_key) {
            return (Err(refusal), Vec::new());
        }
        let surface_store = host.surface_store();
        let check_out_leases = check_out_leases_of(surface_store.as_ref());
        let minted_frame_generations = host.lease_aware_pool_minted_frame_generations();
        self.replace_the_pool_if_its_descriptor_changed(pool_key, descriptor);
        let released_slots =
            self.release_retiring_slots_nobody_holds(check_out_leases, minted_frame_generations);
        let handed_off = self
            .pool_under(pool_key, descriptor)
            .hand_off_a_reusable_frame(rotation_depth, check_out_leases, minted_frame_generations)
            .map(|handed_off| match handed_off {
                ProcessorOutputSurfacePoolHandOff::ReusedSlot(slot) => {
                    ProcessorOutputFrameHandOff::Published(slot.currently_published_frame_id())
                }
                ProcessorOutputSurfacePoolHandOff::NeedsAFreshSlot => {
                    ProcessorOutputFrameHandOff::NeedsAFreshSlot
                }
            });
        (handed_off, released_slots)
    }

    /// Add a slot allocated after [`Self::hand_off_a_reusable_frame`] asked
    /// for one to the pool under `pool_key`, and hand out its first frame.
    pub(crate) fn hand_off_a_fresh_slot(
        &mut self,
        host: &GpuContext,
        pool_key: &str,
        descriptor: &ProcessorOutputSlotDescriptor,
        pool_slot_key: String,
        registered_handle: RegisteredHandle,
    ) -> ProcessorOutputFreshSlotHandOff {
        let refused =
            |refusal, pool_slot_key, registered_handle| ProcessorOutputFreshSlotHandOff::Refused {
                refusal,
                slot_owed_its_release: ReleasedProcessorOutputPoolSlot {
                    pool_slot_key,
                    registered_handle,
                },
            };
        if let Err(refusal) = self.refuse_after_the_helpers_teardown(pool_key) {
            return refused(refusal, pool_slot_key, registered_handle);
        }
        let Some(strong_count_with_no_holder) = pooled_resource_strong_count(&registered_handle)
        else {
            let refusal = Error::GpuError(format!(
                "processor output pool '{pool_key}' was handed a slot that is neither a pooled \
                 texture nor a tensor storage buffer"
            ));
            return refused(refusal, pool_slot_key, registered_handle);
        };
        let surface_store = host.surface_store();
        self.replace_the_pool_if_its_descriptor_changed(pool_key, descriptor);
        match self.pool_under(pool_key, descriptor).hand_off_a_fresh_slot(
            pool_slot_key,
            ProcessorOutputPoolSlot {
                registered_handle,
                strong_count_with_no_holder,
            },
            check_out_leases_of(surface_store.as_ref()),
            host.lease_aware_pool_minted_frame_generations(),
        ) {
            Ok(slot) => {
                ProcessorOutputFreshSlotHandOff::Published(slot.currently_published_frame_id())
            }
            Err(ProcessorOutputSurfacePoolRefusedFreshSlot {
                refusal,
                pool_slot_key,
                resource,
            }) => refused(refusal, pool_slot_key, resource.registered_handle),
        }
    }

    fn refuse_after_the_helpers_teardown(&self, pool_key: &str) -> Result<()> {
        if self.drained_at_the_helpers_teardown {
            return Err(Error::Runtime(format!(
                "processor output pool '{pool_key}' was asked for a frame after its helper \
                 process was torn down"
            )));
        }
        Ok(())
    }

    fn replace_the_pool_if_its_descriptor_changed(
        &mut self,
        pool_key: &str,
        descriptor: &ProcessorOutputSlotDescriptor,
    ) {
        if let Some((existing_descriptor, _)) = self.pools_by_key.get(pool_key)
            && existing_descriptor != descriptor
            && let Some((_, replaced_pool)) = self.pools_by_key.remove(pool_key)
        {
            self.retiring_slots.extend(replaced_pool.into_slots());
        }
    }

    fn pool_under(
        &mut self,
        pool_key: &str,
        descriptor: &ProcessorOutputSlotDescriptor,
    ) -> &mut ProcessorOutputPool {
        let (_, pool) = self
            .pools_by_key
            .entry(pool_key.to_string())
            .or_insert_with(|| {
                (
                    descriptor.clone(),
                    ProcessorOutputSurfacePool::new(pool_key.to_string()),
                )
            });
        pool
    }

    /// Every retiring slot nobody holds any longer, with every frame it
    /// published retired first — under the lease hand-off guard, so a checkout
    /// of one of its ids lands strictly before the test (and keeps the slot) or
    /// strictly after the retire (and is refused as recycled).
    fn release_retiring_slots_nobody_holds(
        &mut self,
        check_out_leases: Option<&SurfaceCheckOutLeaseRegistry>,
        minted_frame_generations: &LeaseAwarePoolMintedFrameGenerations,
    ) -> Vec<ReleasedProcessorOutputPoolSlot> {
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

    /// Every slot of every pool, retiring ones included, for teardown; every
    /// hand-off after it is refused.
    pub(crate) fn drain_slots(&mut self) -> Vec<ReleasedProcessorOutputPoolSlot> {
        self.drained_at_the_helpers_teardown = true;
        self.pools_by_key
            .drain()
            .flat_map(|(_, (_, pool))| pool.into_slots())
            .chain(std::mem::take(&mut self.retiring_slots))
            .map(released_slot)
            .collect()
    }
}

fn check_out_leases_of(
    surface_store: Option<&SurfaceStore>,
) -> Option<&SurfaceCheckOutLeaseRegistry> {
    surface_store
        .and_then(|store| store.check_out_leases())
        .map(|leases| leases.as_ref())
}

fn released_slot(
    slot: LeaseAwarePoolSlot<ProcessorOutputPoolSlot>,
) -> ReleasedProcessorOutputPoolSlot {
    ReleasedProcessorOutputPoolSlot {
        pool_slot_key: slot.pool_slot_key().to_string(),
        registered_handle: slot.into_resource().registered_handle,
    }
}
