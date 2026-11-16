// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::{Arc, OnceLock};

use crate::core::Result;
use crate::core::context::SurfaceStore;
use crate::core::context::gpu_context::{
    PixelBufferPoolKey, PixelBufferPoolManager, PixelBufferPoolSlot,
    gpu_pool_slot_frame_of_the_pixel_buffer_slot, published_pixel_buffer_frame_id,
};
use crate::core::context::texture_pools_and_rings::node_output_surface_pool::{
    NodeOutputSurfacePool, NodeOutputSurfacePoolHandOff, NodeOutputSurfacePoolRefusedLentSlot,
};
use crate::core::rhi::{PixelBuffer, PixelFormat, PublishedPixelBufferFrameId};
use crate::core::runtime::LoadedStreamTag;
use streamlib_ipc_types::GpuPoolSlotFrameDescriptorBody;

/// How many frames a native node's output pool keeps in rotation before it
/// reuses one.
pub(in crate::core::context) const NATIVE_NODE_OUTPUT_PIXEL_BUFFER_POOL_ROTATION_DEPTH: usize = 4;

/// One frame a native node's output pool hands out.
pub struct NativeNodeOutputPixelBufferFrame {
    /// The id the frame is published under.
    pub published_frame_id: PublishedPixelBufferFrameId,
    /// The buffer the frame's pixels are written into.
    pub pixel_buffer: PixelBuffer,
    /// The descriptor a reader claims the frame by, or `None` when its slot
    /// leases no claim-table row.
    pub gpu_pool_slot_frame: Option<GpuPoolSlotFrameDescriptorBody>,
}

/// The pixel buffers one native built-in node publishes its frames in,
/// borrowed from the runtime's shared pixel-buffer pool, handed back to it
/// when the node's frame shape changes, and freed or handed back when this
/// drops.
pub struct NodeOutputPixelBufferPoolOfOneNativeNode {
    node_run_id_of_the_owning_node: String,
    /// The stream the node runs in, which every slot it borrows is attributed
    /// to; `None` for a node the engine's own context serves.
    loaded_stream_the_owning_node_runs_in: Option<LoadedStreamTag>,
    output_pool_of_the_current_shape: Option<(
        PixelBufferPoolKey,
        NodeOutputSurfacePool<PixelBufferPoolSlot>,
    )>,
    shared_pixel_buffer_pool_manager: Arc<PixelBufferPoolManager>,
    engines_own_surface_store: Arc<OnceLock<SurfaceStore>>,
}

impl NodeOutputPixelBufferPoolOfOneNativeNode {
    pub(in crate::core::context) fn new(
        node_run_id_of_the_owning_node: &str,
        loaded_stream_the_owning_node_runs_in: Option<LoadedStreamTag>,
        shared_pixel_buffer_pool_manager: Arc<PixelBufferPoolManager>,
        engines_own_surface_store: Arc<OnceLock<SurfaceStore>>,
    ) -> Self {
        Self {
            node_run_id_of_the_owning_node: node_run_id_of_the_owning_node.to_string(),
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
    pub fn acquire_the_next_output_pixel_buffer(
        &mut self,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<NativeNodeOutputPixelBufferFrame> {
        let shape = PixelBufferPoolKey {
            width,
            height,
            format,
        };
        self.hand_back_the_output_pool_of_another_shape(shape);
        let Self {
            node_run_id_of_the_owning_node,
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
                NodeOutputSurfacePool::new(native_node_output_pool_key(
                    node_run_id_of_the_owning_node,
                    shape,
                )),
            )
        });

        if let NodeOutputSurfacePoolHandOff::ReusedSlot(reused_slot) = output_pool
            .hand_off_a_reusable_frame(
                NATIVE_NODE_OUTPUT_PIXEL_BUFFER_POOL_ROTATION_DEPTH,
                check_out_leases,
                shared_pixel_buffer_pool_manager.minted_frame_generations(),
            )?
        {
            let (published_frame_id, pixel_buffer) = shared_pixel_buffer_pool_manager
                .publish_the_handed_off_frame_in_the_buffer_cache(reused_slot);
            let gpu_pool_slot_frame = gpu_pool_slot_frame_of_the_pixel_buffer_slot(reused_slot);
            shared_pixel_buffer_pool_manager.free_at_most_one_slot_due_to_be_freed();
            return Ok(NativeNodeOutputPixelBufferFrame {
                published_frame_id,
                pixel_buffer,
                gpu_pool_slot_frame,
            });
        }

        let lent_slot = shared_pixel_buffer_pool_manager.lend_a_slot_to_a_native_node_output_pool(
            shape,
            *loaded_stream_the_owning_node_runs_in,
        )?;
        match output_pool.hand_off_a_slot_its_lender_already_republished(lent_slot) {
            Ok(handed_off_slot) => Ok(NativeNodeOutputPixelBufferFrame {
                published_frame_id: published_pixel_buffer_frame_id(handed_off_slot),
                pixel_buffer: handed_off_slot.resource().buffer.clone(),
                gpu_pool_slot_frame: gpu_pool_slot_frame_of_the_pixel_buffer_slot(handed_off_slot),
            }),
            Err(NodeOutputSurfacePoolRefusedLentSlot { refusal, slot }) => {
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

impl Drop for NodeOutputPixelBufferPoolOfOneNativeNode {
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
    node_run_id_of_the_owning_node: &str,
    shape: PixelBufferPoolKey,
) -> String {
    format!(
        "native-node-output-pixel-buffer-pool-{node_run_id_of_the_owning_node}-{}x{}-{}",
        shape.width,
        shape.height,
        shape.format.wire_name()
    )
}
