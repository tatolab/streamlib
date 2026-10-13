// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The output pool a native built-in node publishes its per-frame pixel
//! buffers from: the node's own count, under the processor output pool's cap,
//! over slots it borrows from the runtime's shared pixel-buffer pool. Memory
//! stays the runtime's; only the count is the node's. At the pool's teardown
//! the memory goes back: each slot nothing holds is freed, and each held one
//! returns to the shared ring to be freed once let go.

use std::sync::{Arc, OnceLock};

use super::SurfaceStore;
use super::gpu_context::{
    PixelBufferPoolKey, PixelBufferPoolManager, PixelBufferPoolSlot,
    published_pixel_buffer_frame_id,
};
use super::processor_output_surface_pool::{
    ProcessorOutputSurfacePool, ProcessorOutputSurfacePoolHandOff,
    ProcessorOutputSurfacePoolRefusedLentSlot,
};
use crate::core::Result;
use crate::core::rhi::{PixelBuffer, PixelFormat, PublishedPixelBufferFrameId};
use crate::core::runtime::LoadedStreamTag;

/// How many frames a native node's output pool keeps in rotation before it
/// reuses one.
pub(super) const NATIVE_NODE_OUTPUT_PIXEL_BUFFER_POOL_ROTATION_DEPTH: usize = 4;

/// The pixel buffers one native built-in node publishes its frames in,
/// borrowed from the runtime's shared pixel-buffer pool, handed back to it
/// when the node's frame shape changes, and freed or handed back when this
/// drops.
pub(crate) struct ProcessorOutputPixelBufferPoolOfOneNativeNode {
    processor_id_of_the_owning_node: String,
    /// The stream the node runs in, which every slot it borrows is attributed
    /// to; `None` for a node the engine's own context serves.
    loaded_stream_the_owning_node_runs_in: Option<LoadedStreamTag>,
    output_pool_of_the_current_shape: Option<(
        PixelBufferPoolKey,
        ProcessorOutputSurfacePool<PixelBufferPoolSlot>,
    )>,
    shared_pixel_buffer_pool_manager: Arc<PixelBufferPoolManager>,
    engines_own_surface_store: Arc<OnceLock<SurfaceStore>>,
}

impl ProcessorOutputPixelBufferPoolOfOneNativeNode {
    /// An empty pool for the node `processor_id_of_the_owning_node` running
    /// in `loaded_stream_the_owning_node_runs_in`, borrowing from
    /// `shared_pixel_buffer_pool_manager`.
    pub(super) fn new(
        processor_id_of_the_owning_node: &str,
        loaded_stream_the_owning_node_runs_in: Option<LoadedStreamTag>,
        shared_pixel_buffer_pool_manager: Arc<PixelBufferPoolManager>,
        engines_own_surface_store: Arc<OnceLock<SurfaceStore>>,
    ) -> Self {
        Self {
            processor_id_of_the_owning_node: processor_id_of_the_owning_node.to_string(),
            loaded_stream_the_owning_node_runs_in,
            output_pool_of_the_current_shape: None,
            shared_pixel_buffer_pool_manager,
            engines_own_surface_store,
        }
    }

    /// The pixel buffer this node's next frame publishes into, under a freshly
    /// minted id; refused by name once the pool's capacity of this node's
    /// frames is held, or past the device's memory budget. Never waits. Frees
    /// at most one shared slot due to be freed.
    pub(crate) fn acquire_the_next_output_pixel_buffer(
        &mut self,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<(PublishedPixelBufferFrameId, PixelBuffer)> {
        let shape = PixelBufferPoolKey {
            width,
            height,
            format,
        };
        self.hand_back_the_output_pool_of_another_shape(shape);
        let Self {
            processor_id_of_the_owning_node,
            loaded_stream_the_owning_node_runs_in,
            output_pool_of_the_current_shape,
            shared_pixel_buffer_pool_manager,
            engines_own_surface_store,
        } = self;
        let check_out_leases =
            SurfaceStore::check_out_lease_registry_of_the_engines_own(engines_own_surface_store);
        let (_, output_pool) = output_pool_of_the_current_shape.get_or_insert_with(|| {
            (
                shape,
                ProcessorOutputSurfacePool::new(native_node_output_pool_key(
                    processor_id_of_the_owning_node,
                    shape,
                )),
            )
        });

        if let ProcessorOutputSurfacePoolHandOff::ReusedSlot(reused_slot) = output_pool
            .hand_off_a_reusable_frame(
                NATIVE_NODE_OUTPUT_PIXEL_BUFFER_POOL_ROTATION_DEPTH,
                check_out_leases,
                shared_pixel_buffer_pool_manager.minted_frame_generations(),
            )?
        {
            let handed_off = shared_pixel_buffer_pool_manager
                .publish_the_handed_off_frame_in_the_buffer_cache(reused_slot);
            shared_pixel_buffer_pool_manager.free_at_most_one_slot_due_to_be_freed();
            return Ok(handed_off);
        }

        let lent_slot = shared_pixel_buffer_pool_manager.lend_a_slot_to_a_native_node_output_pool(
            shape,
            *loaded_stream_the_owning_node_runs_in,
        )?;
        match output_pool.hand_off_a_slot_its_lender_already_republished(lent_slot) {
            Ok(handed_off_slot) => Ok((
                published_pixel_buffer_frame_id(handed_off_slot),
                handed_off_slot.resource().buffer.clone(),
            )),
            Err(ProcessorOutputSurfacePoolRefusedLentSlot { refusal, slot }) => {
                shared_pixel_buffer_pool_manager
                    .take_back_slots_lent_to_a_native_node_output_pool(shape, [*slot]);
                Err(refusal)
            }
        }
    }

    /// How many slots the pool holds of its current shape.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn slot_count(&self) -> usize {
        self.output_pool_of_the_current_shape
            .as_ref()
            .map_or(0, |(_, output_pool)| output_pool.slot_count())
    }

    fn hand_back_the_output_pool_of_another_shape(&mut self, shape: PixelBufferPoolKey) {
        if self
            .output_pool_of_the_current_shape
            .as_ref()
            .is_some_and(|(current_shape, _)| *current_shape != shape)
        {
            self.hand_back_every_slot();
        }
    }

    fn hand_back_every_slot(&mut self) {
        if let Some((shape, output_pool)) = self.output_pool_of_the_current_shape.take() {
            self.shared_pixel_buffer_pool_manager
                .take_back_slots_lent_to_a_native_node_output_pool(shape, output_pool.into_slots());
        }
    }
}

impl Drop for ProcessorOutputPixelBufferPoolOfOneNativeNode {
    fn drop(&mut self) {
        if let Some((shape, output_pool)) = self.output_pool_of_the_current_shape.take() {
            self.shared_pixel_buffer_pool_manager
                .free_or_take_back_the_slots_of_a_torn_down_native_node_output_pool(
                    shape,
                    output_pool.into_slots(),
                );
        }
    }
}

/// The key a native node's output pool is named by in its refusal: the node
/// and the shape it publishes.
fn native_node_output_pool_key(
    processor_id_of_the_owning_node: &str,
    shape: PixelBufferPoolKey,
) -> String {
    format!(
        "native-node-output-pixel-buffer-pool-{processor_id_of_the_owning_node}-{}x{}-{}",
        shape.width,
        shape.height,
        shape.format.wire_name()
    )
}
