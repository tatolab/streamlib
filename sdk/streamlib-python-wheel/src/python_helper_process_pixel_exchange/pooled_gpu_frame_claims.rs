// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The pooled GPU frames a helper reads and acquires, held against producer
//! reuse by claims in their pools' claim tables rather than a surface-share
//! request per frame.

use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib::sdk::context::GpuPoolSlotFrameDescriptorBody;
use streamlib_surface_client::{
    GpuPoolClaimTableHandOver, SURFACE_SHARE_CLAIM_TABLE_OF_GPU_POOL_OP,
    SURFACE_SHARE_GPU_POOL_FIELD, gpu_pool_uuid_of_its_wire_name, gpu_pool_uuid_wire_name,
    published_frame_id_of_pool_slot, refusal_the_surface_share_service_answered,
};

use super::HelperProcessGpuExchangeClient;
use super::gpu_pool_claim_tables_of_this_helper::{
    HelperGpuPoolFrameClaimAttempt, HelperGpuPoolSlotFrameClaim,
};

impl HelperProcessGpuExchangeClient {
    /// Claim the frame `surface_id` names in its pool's claim table, with no
    /// surface-share request; `None` for a frame no table this helper holds
    /// covers, which a checkout holds instead.
    pub(crate) fn claim_in_its_pool_claim_table(
        &self,
        surface_id: &str,
    ) -> PyResult<Option<Arc<HelperGpuPoolSlotFrameClaim>>> {
        self.claim_in_its_pool_claim_table_refusing_by_message(surface_id)
            .map_err(PyRuntimeError::new_err)
    }

    /// [`Self::claim_in_its_pool_claim_table`], refusing with a message rather
    /// than a Python exception, for a caller holding no interpreter.
    pub(crate) fn claim_in_its_pool_claim_table_refusing_by_message(
        &self,
        surface_id: &str,
    ) -> Result<Option<Arc<HelperGpuPoolSlotFrameClaim>>, String> {
        match self.gpu_pool_claim_tables.claim_the_frame_named(surface_id) {
            HelperGpuPoolFrameClaimAttempt::Claimed(claimed) => Ok(Some(claimed)),
            HelperGpuPoolFrameClaimAttempt::NotInAClaimTable => Ok(None),
            HelperGpuPoolFrameClaimAttempt::Recycled(recycled) => Err(format!(
                "frame {surface_id:?} was recycled: its slot publishes generation {}{}, and a \
                 published frame is never answered with newer pixels",
                recycled.published_generation,
                if recycled.slot_was_retiring {
                    " and is being handed out again"
                } else {
                    ""
                }
            )),
        }
    }

    /// Take in the pooled frame a read carried and claim it, so the slot is
    /// never rehanded while the read's payload is held: the first frame of a
    /// pool, or of a slot this helper does not know yet, fetches the pool's
    /// claim table and its slot keys once. A pool whose table the service
    /// refuses, such as one past its last claimant column, is read through
    /// checkouts, and answers no claim.
    pub(crate) fn note_and_claim_the_pooled_gpu_frame_a_read_carries(
        &self,
        pooled_frame: GpuPoolSlotFrameDescriptorBody,
    ) -> PyResult<Option<Arc<HelperGpuPoolSlotFrameClaim>>> {
        self.note_and_claim_the_pooled_gpu_frame_a_read_carries_refusing_by_message(pooled_frame)
            .map_err(PyRuntimeError::new_err)
    }

    /// [`Self::note_and_claim_the_pooled_gpu_frame_a_read_carries`], refusing
    /// with a message rather than a Python exception, for a caller holding no
    /// interpreter.
    pub(crate) fn note_and_claim_the_pooled_gpu_frame_a_read_carries_refusing_by_message(
        &self,
        pooled_frame: GpuPoolSlotFrameDescriptorBody,
    ) -> Result<Option<Arc<HelperGpuPoolSlotFrameClaim>>, String> {
        if self
            .gpu_pool_claim_tables
            .the_hand_over_was_refused(&pooled_frame.pool)
        {
            return Ok(None);
        }
        if !self.gpu_pool_claim_tables.knows_the_slot_of(&pooled_frame) {
            match self.fetch_the_claim_table_of_gpu_pool(pooled_frame.pool) {
                Ok(()) => {}
                Err(GpuPoolClaimTableHandOverFailure::RequestFailed(request_failure)) => {
                    tracing::warn!(
                        "GPU pool {}: the claim table request failed ({request_failure}); \
                         this frame is read through a checkout and the next frame asks again",
                        gpu_pool_uuid_wire_name(&pooled_frame.pool)
                    );
                    return Ok(None);
                }
                Err(GpuPoolClaimTableHandOverFailure::Refused(refusal)) => {
                    tracing::warn!(
                        "GPU pool {}: {refusal}; this helper reads the pool's frames through \
                         checkouts",
                        gpu_pool_uuid_wire_name(&pooled_frame.pool)
                    );
                    self.gpu_pool_claim_tables
                        .note_a_refused_hand_over(pooled_frame.pool);
                    return Ok(None);
                }
            }
        }
        let Some(pool_slot_key) = self.gpu_pool_claim_tables.note_a_pooled_frame(pooled_frame)
        else {
            return Err(format!(
                "a frame of GPU pool {} names claim table row {}, which the pool's claim table \
                 hand-over leases to no slot",
                gpu_pool_uuid_wire_name(&pooled_frame.pool),
                pooled_frame.slot
            ));
        };
        self.claim_in_its_pool_claim_table_refusing_by_message(&published_frame_id_of_pool_slot(
            &pool_slot_key,
            pooled_frame.generation,
        ))
    }

    /// Remember the descriptor an acquire from this helper's own output pool
    /// answered, which its writes of the frame carry.
    pub(crate) fn note_the_pooled_frame_an_acquire_answered(
        &self,
        published_frame_id: &str,
        acquire_response: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let Ok(pooled_frame) = acquire_response.get_item("gpu_pool_slot_frame") else {
            return Ok(());
        };
        if pooled_frame.is_none() {
            return Ok(());
        }
        let pool_wire_name: String = pooled_frame.get_item("pool")?.extract()?;
        let pool = gpu_pool_uuid_of_its_wire_name(&pool_wire_name).ok_or_else(|| {
            PyRuntimeError::new_err(format!(
                "the parent's acquire named GPU pool {pool_wire_name:?}, which is not 32 hex \
                 digits"
            ))
        })?;
        let generation_digits: String = pooled_frame.get_item("generation")?.extract()?;
        let generation = generation_digits.parse::<u64>().map_err(|_| {
            PyRuntimeError::new_err(format!(
                "the parent's acquire named frame generation {generation_digits:?}, which is not \
                 a decimal u64"
            ))
        })?;
        self.gpu_pool_claim_tables.note_an_acquired_pooled_frame(
            published_frame_id,
            GpuPoolSlotFrameDescriptorBody {
                pool,
                slot: pooled_frame.get_item("slot")?.extract()?,
                generation,
                timeline_value: 0,
                image_layout: 0,
            },
        );
        Ok(())
    }

    /// Record the layout a kernel run left each texture it bound in.
    pub(crate) fn note_the_texture_layouts_a_run_left(&self, run_response: &Bound<'_, PyAny>) {
        let Ok(layouts) = run_response.get_item("texture_layouts_after_the_run") else {
            return;
        };
        let Ok(layouts) = layouts.try_iter() else {
            return;
        };
        for layout in layouts.flatten() {
            let surface_id = layout
                .get_item("surface_id")
                .and_then(|value| value.extract::<String>());
            let image_layout = layout
                .get_item("image_layout")
                .and_then(|value| value.extract::<i32>());
            if let (Ok(surface_id), Ok(image_layout)) = (surface_id, image_layout) {
                self.gpu_pool_claim_tables
                    .note_the_layout_a_run_left(&surface_id, image_layout);
            }
        }
    }

    /// The descriptor a write of the pooled frame `published_frame_id` names
    /// carries, as this helper last read or acquired it.
    pub(crate) fn descriptor_of_the_pooled_gpu_frame_named(
        &self,
        published_frame_id: &str,
    ) -> Option<GpuPoolSlotFrameDescriptorBody> {
        self.gpu_pool_claim_tables
            .descriptor_of_the_frame_named(published_frame_id)
    }

    /// The id the pooled frame `pooled_frame` is published under, once a read
    /// of it taught this helper its slot's key.
    pub(crate) fn published_frame_id_of_the_pooled_gpu_frame(
        &self,
        pooled_frame: &GpuPoolSlotFrameDescriptorBody,
    ) -> Option<String> {
        self.gpu_pool_claim_tables
            .pool_slot_key_of(pooled_frame)
            .map(|pool_slot_key| {
                published_frame_id_of_pool_slot(&pool_slot_key, pooled_frame.generation)
            })
    }

    fn fetch_the_claim_table_of_gpu_pool(
        &self,
        pool: [u8; 16],
    ) -> Result<(), GpuPoolClaimTableHandOverFailure> {
        let pool_wire_name = gpu_pool_uuid_wire_name(&pool);
        let (reply, received_handles) = self
            .surface_share_request(&serde_json::json!({
                "op": SURFACE_SHARE_CLAIM_TABLE_OF_GPU_POOL_OP,
                SURFACE_SHARE_GPU_POOL_FIELD: pool_wire_name,
            }))
            .map_err(GpuPoolClaimTableHandOverFailure::RequestFailed)?;
        if let Some(refusal) = refusal_the_surface_share_service_answered(&reply) {
            return Err(GpuPoolClaimTableHandOverFailure::Refused(format!(
                "the surface-share service refused the claim table of GPU pool \
                 {pool_wire_name}: {refusal}"
            )));
        }
        let claim_table_shared_rows_handle = received_handles.into_iter().next().ok_or_else(|| {
            GpuPoolClaimTableHandOverFailure::Refused(format!(
                "the claim table hand-over of GPU pool {pool_wire_name} carried no handle to the table's memory"
            ))
        })?;
        let hand_over = GpuPoolClaimTableHandOver::from_reply(&reply)
            .map_err(GpuPoolClaimTableHandOverFailure::Refused)?;
        self.gpu_pool_claim_tables
            .adopt_a_hand_over(pool, claim_table_shared_rows_handle, hand_over)
            .map_err(GpuPoolClaimTableHandOverFailure::Refused)
    }
}

/// Why a pool's claim table hand-over did not land: a request that never got
/// an answer is asked again on the next frame, an answered refusal is final.
pub(super) enum GpuPoolClaimTableHandOverFailure {
    RequestFailed(PyErr),
    Refused(String),
}
