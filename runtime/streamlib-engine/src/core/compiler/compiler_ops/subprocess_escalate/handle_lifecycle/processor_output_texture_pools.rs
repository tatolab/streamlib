// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The processor output texture pools one helper process publishes kernel
//! outputs from, each under the pool key its ring minted.

use std::collections::HashMap;

use super::RegisteredHandle;
use crate::core::context::GpuContextFullAccess;
use crate::core::context::lease_aware_pool_slot_ring::LeaseAwarePoolSlotResource;
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

impl ProcessorOutputTextureSlot {
    fn texture_strong_count(registered_texture: &RegisteredHandle) -> usize {
        match registered_texture {
            RegisteredHandle::Texture { texture, .. } => texture.texture().strong_count(),
            _ => 0,
        }
    }
}

impl LeaseAwarePoolSlotResource for ProcessorOutputTextureSlot {
    fn is_held_in_this_process(&self) -> bool {
        Self::texture_strong_count(&self.registered_texture)
            > self.texture_strong_count_with_no_holder
    }
}

type ProcessorOutputTexturePool = ProcessorOutputSurfacePool<ProcessorOutputTextureSlot>;

/// Pool key → the descriptor its slots were allocated for, and the pool.
#[derive(Default)]
pub(crate) struct ProcessorOutputTexturePoolsOfOneHelper {
    pools_by_key: HashMap<String, (ProcessorOutputTextureDescriptor, ProcessorOutputTexturePool)>,
}

/// A slot this helper's pools no longer hold, owed the release every
/// registered texture is owed.
pub(crate) type ReleasedProcessorOutputTextureSlot = (String, RegisteredHandle);

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
        let mut released_slots = Vec::new();
        if let Some((existing_descriptor, _)) = self.pools_by_key.get(pool_key)
            && *existing_descriptor != descriptor
            && let Some((_, replaced_pool)) = self.pools_by_key.remove(pool_key)
        {
            released_slots.extend(released_slots_of(replaced_pool));
        }
        let (_, pool) = self
            .pools_by_key
            .entry(pool_key.to_string())
            .or_insert_with(|| {
                (
                    descriptor,
                    ProcessorOutputSurfacePool::new(pool_key.to_string()),
                )
            });
        let host = full.host_inner();
        let surface_store = host.surface_store();
        let published = pool
            .hand_off_next_frame(
                rotation_depth,
                surface_store
                    .as_ref()
                    .and_then(|store| store.check_out_leases())
                    .map(|leases| leases.as_ref()),
                host.lease_aware_pool_minted_frame_generations(),
                || {
                    let (pool_slot_key, registered_texture) = allocate_fresh_slot()?;
                    if !registered_texture.is_texture_backed() {
                        return Err(Error::GpuError(format!(
                            "processor output pool '{pool_key}' was handed a slot that is not \
                             a texture"
                        )));
                    }
                    let texture_strong_count_with_no_holder =
                        ProcessorOutputTextureSlot::texture_strong_count(&registered_texture);
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

    /// Every slot of every pool, for teardown.
    pub(crate) fn drain_slots(&mut self) -> Vec<ReleasedProcessorOutputTextureSlot> {
        self.pools_by_key
            .drain()
            .flat_map(|(_, (_, pool))| released_slots_of(pool))
            .collect()
    }
}

fn released_slots_of(
    pool: ProcessorOutputTexturePool,
) -> impl Iterator<Item = ReleasedProcessorOutputTextureSlot> {
    pool.into_slots().map(|slot| {
        let pool_slot_key = slot.pool_slot_key().to_string();
        (pool_slot_key, slot.into_resource().registered_texture)
    })
}
