// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A native consumer's claim on a published frame: a share of the frame's
//! backing in this process, which its producer's pool counts as a holder.

use super::surface_backing_resolution::ResolvedSurfaceBacking;
use super::{GpuContext, GpuContextLimitedAccess};
use crate::core::error::{Error, Result};

/// A published frame held against its producer's reuse: while this lives, the
/// pool that published the frame never rehands its slot.
#[must_use = "a claim holds the frame only while it is kept"]
pub struct SurfaceClaimedAgainstProducerReuse {
    _same_process_backing_share: Option<ResolvedSurfaceBacking>,
}

impl GpuContext {
    /// Claim a published frame against its producer's reuse.
    ///
    /// The share is taken before the frame's generation is checked, and the
    /// check waits out any hand-off in progress, so a hand-off either sees the
    /// share or retires the id before the check refuses it. A frame no pool in
    /// this process published has no backing here to share, and the claim
    /// holds none.
    pub(crate) fn claim_surface_against_producer_reuse(
        &self,
        surface_id: &str,
    ) -> Result<SurfaceClaimedAgainstProducerReuse> {
        let same_process_backing_share = self.same_process_backing_of_surface(surface_id);
        self.refuse_a_retired_frame_id(surface_id)?;
        Ok(SurfaceClaimedAgainstProducerReuse {
            _same_process_backing_share: same_process_backing_share,
        })
    }
}

impl GpuContext {
    /// The surface id of the frame `gpu_pool_slot_frame` names, read from its
    /// pool's claim table in this process: the slot's key at the frame's
    /// generation. Refused when no claim table here names the pool or leases
    /// the row, as for a frame another runtime published.
    pub(crate) fn published_frame_id_of_the_gpu_pool_slot_frame(
        &self,
        gpu_pool_slot_frame: &streamlib_ipc_types::GpuPoolSlotFrameDescriptorBody,
    ) -> Result<String> {
        let pool_wire_name =
            streamlib_surface_client::gpu_pool_uuid_wire_name(&gpu_pool_slot_frame.pool);
        let surface_store = self.surface_store().ok_or_else(|| {
            Error::GpuError(format!(
                "GPU pool {pool_wire_name}'s frames cannot be read in this process: it shares \
                 no surface store"
            ))
        })?;
        let claim_table = surface_store
            .check_out_leases()
            .and_then(|check_out_leases| {
                check_out_leases
                    .gpu_pool_claim_tables()
                    .table_named(&gpu_pool_slot_frame.pool)
            })
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "no claim table in this process names GPU pool {pool_wire_name}, so its \
                     frames are not this runtime's"
                ))
            })?;
        let pool_slot_key = claim_table
            .rows_leased_to_slots()
            .into_iter()
            .find(|leased_row| leased_row.row_index == gpu_pool_slot_frame.slot)
            .map(|leased_row| leased_row.pool_slot_key)
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "GPU pool {pool_wire_name} leases row {} to no slot, so the frame it named \
                     at generation {} is gone",
                    gpu_pool_slot_frame.slot, gpu_pool_slot_frame.generation
                ))
            })?;
        Ok(crate::core::rhi::published_frame_id_of_pool_slot(
            &pool_slot_key,
            gpu_pool_slot_frame.generation,
        ))
    }
}

impl GpuContextLimitedAccess {
    /// Hold a published frame against its producer's reuse until the claim
    /// drops, refusing a frame id its pool has already retired as recycled.
    pub fn claim_surface_against_producer_reuse(
        &self,
        surface_id: &str,
    ) -> Result<SurfaceClaimedAgainstProducerReuse> {
        self.host_inner()
            .claim_surface_against_producer_reuse(surface_id)
    }
}
