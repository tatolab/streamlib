// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A native consumer's claim on a published frame: a share of the frame's
//! pooled backing, which its producer's pool counts as a holder.

use super::surface_backing_resolution::ResolvedSurfaceBacking;
use super::{GpuContext, GpuContextLimitedAccess};
use crate::core::error::Result;

/// A published frame held against its producer's reuse: while this lives, the
/// pool that published the frame never rehands its slot.
#[must_use = "a claim holds the frame only while it is kept"]
pub struct SurfaceClaimedAgainstProducerReuse {
    _pooled_backing_share: Option<ResolvedSurfaceBacking>,
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
        let pooled_backing_share = self.same_process_backing_of_surface(surface_id);
        self.refuse_a_retired_frame_id(surface_id)?;
        Ok(SurfaceClaimedAgainstProducerReuse {
            _pooled_backing_share: pooled_backing_share,
        })
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
