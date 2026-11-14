// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The pooled GPU frames a helper reads and acquires, held against producer
//! reuse by claims in their pools' claim tables rather than a surface-share
//! request per frame.

use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib::sdk::context::GpuPoolSlotFrameDescriptorBody;
use streamlib_surface_client::{gpu_pool_uuid_of_its_wire_name, published_frame_id_of_pool_slot};

use super::HelperProcessGpuExchangeClient;
use streamlib_gpu_frame_import_client::GpuPoolSlotFrameClaimOfThisImporter;

impl HelperProcessGpuExchangeClient {
    /// Claim the frame `surface_id` names in its pool's claim table, with no
    /// surface-share request; `None` for a frame no table this helper holds
    /// covers, which a checkout holds instead.
    pub(crate) fn claim_in_its_pool_claim_table(
        &self,
        surface_id: &str,
    ) -> PyResult<Option<Arc<GpuPoolSlotFrameClaimOfThisImporter>>> {
        self.claim_in_its_pool_claim_table_refusing_by_message(surface_id)
            .map_err(PyRuntimeError::new_err)
    }

    /// [`Self::claim_in_its_pool_claim_table`], refusing with a message rather
    /// than a Python exception, for a caller holding no interpreter.
    pub(crate) fn claim_in_its_pool_claim_table_refusing_by_message(
        &self,
        surface_id: &str,
    ) -> Result<Option<Arc<GpuPoolSlotFrameClaimOfThisImporter>>, String> {
        self.gpu_pool_claim_tables
            .claim_the_frame_named_refusing_a_recycled_one(surface_id)
    }

    /// Take in the pooled frame a read carried and claim it, so the slot is
    /// never rehanded while the read's payload is held; see
    /// [`GpuPoolClaimTablesOfThisImporter::note_and_claim_a_pooled_frame`](streamlib_gpu_frame_import_client::GpuPoolClaimTablesOfThisImporter::note_and_claim_a_pooled_frame).
    pub(crate) fn note_and_claim_the_pooled_gpu_frame_a_read_carries(
        &self,
        pooled_frame: GpuPoolSlotFrameDescriptorBody,
    ) -> PyResult<Option<Arc<GpuPoolSlotFrameClaimOfThisImporter>>> {
        self.note_and_claim_the_pooled_gpu_frame_a_read_carries_refusing_by_message(pooled_frame)
            .map_err(PyRuntimeError::new_err)
    }

    /// [`Self::note_and_claim_the_pooled_gpu_frame_a_read_carries`], refusing
    /// with a message rather than a Python exception, for a caller holding no
    /// interpreter.
    pub(crate) fn note_and_claim_the_pooled_gpu_frame_a_read_carries_refusing_by_message(
        &self,
        pooled_frame: GpuPoolSlotFrameDescriptorBody,
    ) -> Result<Option<Arc<GpuPoolSlotFrameClaimOfThisImporter>>, String> {
        self.gpu_pool_claim_tables
            .note_and_claim_a_pooled_frame(pooled_frame, &self.surface_share_connection)
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
}
