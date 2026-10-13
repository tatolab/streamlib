// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one-off acquisitions one node holds, counted per shape and refused by
//! name once the node holds as many of a shape as one node may.

#[cfg(test)]
mod tests;

/// One-off shapes the counting tests acquire under.
#[cfg(test)]
pub(crate) mod one_off_acquisition_shapes_for_tests {
    use super::OneOffAcquisitionShape;
    use crate::core::rhi::{PixelFormat, TextureFormat, TextureUsages};

    /// A sampled 1280x720 `Rgba8Unorm` texture.
    pub(crate) fn a_1280x720_rgba8_unorm_texture() -> OneOffAcquisitionShape {
        OneOffAcquisitionShape::Texture {
            width: 1280,
            height: 720,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::TEXTURE_BINDING,
        }
    }

    /// A 640x480 `Bgra32` pixel buffer.
    pub(crate) fn a_640x480_bgra32_pixel_buffer() -> OneOffAcquisitionShape {
        OneOffAcquisitionShape::PixelBuffer {
            width: 640,
            height: 480,
            format: PixelFormat::Bgra32,
        }
    }
}

use std::collections::HashMap;

use super::processor_output_surface_pool::PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY;
use super::texture_pool::TexturePoolDescriptor;
use crate::core::rhi::{
    PixelFormat, TensorStorageBufferLayout, TextureFormat, TextureUsages, texture_usages_to_wire,
};
use crate::core::{Error, Result};

/// The most one-off acquisitions of one shape a node may hold at once.
pub(crate) const ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE: usize =
    PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY;

/// The shape a node's one-off acquisition is counted under.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum OneOffAcquisitionShape {
    Texture {
        width: u32,
        height: u32,
        format: TextureFormat,
        usage: TextureUsages,
    },
    PixelBuffer {
        width: u32,
        height: u32,
        format: PixelFormat,
    },
    StorageBuffer {
        tensor_layout: TensorStorageBufferLayout,
    },
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    RenderTargetImage {
        width: u32,
        height: u32,
        format: TextureFormat,
    },
}

impl OneOffAcquisitionShape {
    /// The shape a texture acquired from the pool by `descriptor` is counted under.
    pub(crate) fn of_texture_pool_descriptor(descriptor: &TexturePoolDescriptor) -> Self {
        Self::Texture {
            width: descriptor.width,
            height: descriptor.height,
            format: descriptor.format,
            usage: descriptor.usage,
        }
    }
}

impl std::fmt::Display for OneOffAcquisitionShape {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Texture {
                width,
                height,
                format,
                usage,
            } => write!(
                formatter,
                "{width}x{height} {} texture (usage {})",
                format.wire_name(),
                texture_usages_to_wire(*usage).join("|")
            ),
            Self::PixelBuffer {
                width,
                height,
                format,
            } => write!(
                formatter,
                "{width}x{height} {} pixel buffer",
                format.wire_name()
            ),
            Self::StorageBuffer { tensor_layout } => write!(
                formatter,
                "{:?} {} storage buffer",
                tensor_layout.shape(),
                tensor_layout.element_type().wire_name()
            ),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::RenderTargetImage {
                width,
                height,
                format,
            } => write!(
                formatter,
                "{width}x{height} {} render-target image",
                format.wire_name()
            ),
        }
    }
}

/// How many one-off acquisitions of each shape one node holds or has reserved.
#[derive(Default)]
pub(crate) struct OneOffAcquisitionCountsByShapeOfOneNode {
    /// A shape leaves the map when its count reaches zero.
    held_or_reserved_count_by_shape: HashMap<OneOffAcquisitionShape, usize>,
}

impl OneOffAcquisitionCountsByShapeOfOneNode {
    /// Count one more acquisition of `shape`, refused by name when the node
    /// already holds or has reserved as many of it as one node may.
    pub(crate) fn reserve_one_more_of_shape(
        &mut self,
        shape: &OneOffAcquisitionShape,
    ) -> Result<()> {
        let held_or_reserved_count = self.held_or_reserved_count_of_shape(shape);
        if held_or_reserved_count >= ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE {
            return Err(
                Error::EveryOneOffAcquisitionOfThisShapeTheNodeMayHoldIsHeld {
                    acquisition_shape: shape.to_string(),
                    one_off_acquisitions_one_node_may_hold_of_one_shape:
                        ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE,
                },
            );
        }
        *self
            .held_or_reserved_count_by_shape
            .entry(shape.clone())
            .or_insert(0) += 1;
        Ok(())
    }

    /// Hand back one count of `shape`.
    pub(crate) fn count_one_fewer_of_shape(&mut self, shape: &OneOffAcquisitionShape) {
        let Some(held_or_reserved_count) = self.held_or_reserved_count_by_shape.get_mut(shape)
        else {
            return;
        };
        *held_or_reserved_count -= 1;
        if *held_or_reserved_count == 0 {
            self.held_or_reserved_count_by_shape.remove(shape);
        }
    }

    /// How many acquisitions of `shape` are held or reserved.
    pub(crate) fn held_or_reserved_count_of_shape(&self, shape: &OneOffAcquisitionShape) -> usize {
        self.held_or_reserved_count_by_shape
            .get(shape)
            .copied()
            .unwrap_or(0)
    }
}

/// The one-off acquisitions one node holds under handle ids, and how many of
/// each shape it holds or has reserved.
pub(crate) struct OneOffAcquisitionsHeldForOneNode<HeldAcquisition> {
    held_by_handle_id: HashMap<String, (OneOffAcquisitionShape, HeldAcquisition)>,
    counts_by_shape: OneOffAcquisitionCountsByShapeOfOneNode,
}

impl<HeldAcquisition> Default for OneOffAcquisitionsHeldForOneNode<HeldAcquisition> {
    fn default() -> Self {
        Self {
            held_by_handle_id: HashMap::new(),
            counts_by_shape: OneOffAcquisitionCountsByShapeOfOneNode::default(),
        }
    }
}

impl<HeldAcquisition> OneOffAcquisitionsHeldForOneNode<HeldAcquisition> {
    /// Count one more acquisition of `shape`, refused by name when the node
    /// already holds or has reserved as many of it as one node may.
    pub(crate) fn reserve_one_more_of_shape(
        &mut self,
        shape: &OneOffAcquisitionShape,
    ) -> Result<()> {
        self.counts_by_shape.reserve_one_more_of_shape(shape)
    }

    /// Hand back the count of a reservation of `shape` that was never held.
    pub(crate) fn hand_back_an_unheld_reservation_of_shape(
        &mut self,
        shape: &OneOffAcquisitionShape,
    ) {
        self.counts_by_shape.count_one_fewer_of_shape(shape);
    }

    /// Hold an acquisition a reservation of `shape` already counted, under
    /// `handle_id`, answering one it replaced there, whose count goes back.
    pub(crate) fn hold_a_reserved_acquisition(
        &mut self,
        handle_id: String,
        shape: OneOffAcquisitionShape,
        held: HeldAcquisition,
    ) -> Option<HeldAcquisition> {
        let (replaced_shape, replaced) = self.held_by_handle_id.insert(handle_id, (shape, held))?;
        self.counts_by_shape
            .count_one_fewer_of_shape(&replaced_shape);
        Some(replaced)
    }

    /// Release the acquisition held under `handle_id`, handing its count
    /// back; `None` when the id names none.
    pub(crate) fn release_held_acquisition(&mut self, handle_id: &str) -> Option<HeldAcquisition> {
        let (shape, held) = self.held_by_handle_id.remove(handle_id)?;
        self.counts_by_shape.count_one_fewer_of_shape(&shape);
        Some(held)
    }

    /// Release every held acquisition, ids included, handing back each count
    /// it held; a reservation not yet held keeps its count.
    pub(crate) fn release_every_held_acquisition(&mut self) -> Vec<(String, HeldAcquisition)> {
        let released: Vec<(String, (OneOffAcquisitionShape, HeldAcquisition))> =
            self.held_by_handle_id.drain().collect();
        released
            .into_iter()
            .map(|(handle_id, (shape, held))| {
                self.counts_by_shape.count_one_fewer_of_shape(&shape);
                (handle_id, held)
            })
            .collect()
    }

    /// How many acquisitions are held.
    #[cfg(test)]
    pub(crate) fn held_count(&self) -> usize {
        self.held_by_handle_id.len()
    }

    /// How many acquisitions of `shape` are held or reserved.
    #[cfg(test)]
    pub(crate) fn held_or_reserved_count_of_shape(&self, shape: &OneOffAcquisitionShape) -> usize {
        self.counts_by_shape.held_or_reserved_count_of_shape(shape)
    }

    /// The acquisition held under `handle_id`.
    #[cfg(test)]
    pub(crate) fn held_acquisition(&self, handle_id: &str) -> Option<&HeldAcquisition> {
        self.held_by_handle_id.get(handle_id).map(|(_, held)| held)
    }
}
