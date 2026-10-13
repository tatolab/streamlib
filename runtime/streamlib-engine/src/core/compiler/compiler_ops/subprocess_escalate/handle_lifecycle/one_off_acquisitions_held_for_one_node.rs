// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one-off acquisitions one node holds, counted per shape and refused by
//! name once the node holds as many of a shape as one node may.

#[cfg(test)]
mod tests;

use std::collections::HashMap;

use super::super::acquisition::texture_usages_to_wire;
use super::{EscalateHandleRegistry, RegisteredHandle};
use crate::core::context::processor_output_surface_pool::PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY;
use crate::core::rhi::{PixelFormat, TensorStorageBufferLayout, TextureFormat, TextureUsages};
use crate::core::{Error, Result};

/// The most one-off acquisitions of one shape a node may hold at once.
pub(crate) const ONE_OFF_ACQUISITIONS_ONE_NODE_MAY_HOLD_OF_ONE_SHAPE: usize =
    PROCESSOR_OUTPUT_SURFACE_POOL_CAPACITY;

/// The shape a helper's one-off acquisition is counted under.
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
    #[cfg(target_os = "linux")]
    RenderTargetImage {
        width: u32,
        height: u32,
        format: TextureFormat,
    },
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
            #[cfg(target_os = "linux")]
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

/// The one-off acquisitions one node holds, and how many of each shape it
/// holds or has reserved.
pub(crate) struct OneOffAcquisitionsHeldForOneNode<HeldAcquisition> {
    held_by_handle_id: HashMap<String, (OneOffAcquisitionShape, HeldAcquisition)>,
    /// A shape leaves the map when its count reaches zero.
    held_or_reserved_count_by_shape: HashMap<OneOffAcquisitionShape, usize>,
}

impl<HeldAcquisition> Default for OneOffAcquisitionsHeldForOneNode<HeldAcquisition> {
    fn default() -> Self {
        Self {
            held_by_handle_id: HashMap::new(),
            held_or_reserved_count_by_shape: HashMap::new(),
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
        let held_or_reserved_count = self
            .held_or_reserved_count_by_shape
            .get(shape)
            .copied()
            .unwrap_or(0);
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

    /// Hand back the count of a reservation of `shape` that was never held.
    pub(crate) fn hand_back_an_unheld_reservation_of_shape(
        &mut self,
        shape: &OneOffAcquisitionShape,
    ) {
        self.count_one_fewer_of_shape(shape);
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
        self.count_one_fewer_of_shape(&replaced_shape);
        Some(replaced)
    }

    /// Release the acquisition held under `handle_id`, handing its count
    /// back; `None` when the id names none.
    pub(crate) fn release_held_acquisition(&mut self, handle_id: &str) -> Option<HeldAcquisition> {
        let (shape, held) = self.held_by_handle_id.remove(handle_id)?;
        self.count_one_fewer_of_shape(&shape);
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
                self.count_one_fewer_of_shape(&shape);
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
        self.held_or_reserved_count_by_shape
            .get(shape)
            .copied()
            .unwrap_or(0)
    }

    /// The acquisition held under `handle_id`.
    #[cfg(test)]
    pub(crate) fn held_acquisition(&self, handle_id: &str) -> Option<&HeldAcquisition> {
        self.held_by_handle_id.get(handle_id).map(|(_, held)| held)
    }

    fn count_one_fewer_of_shape(&mut self, shape: &OneOffAcquisitionShape) {
        let Some(held_or_reserved_count) = self.held_or_reserved_count_by_shape.get_mut(shape)
        else {
            return;
        };
        *held_or_reserved_count -= 1;
        if *held_or_reserved_count == 0 {
            self.held_or_reserved_count_by_shape.remove(shape);
        }
    }
}

/// One count a one-off acquisition reserved before allocating, handed back
/// when it drops unless the acquisition was held.
#[must_use = "a reservation dropped unheld hands its count straight back; hold the acquisition \
              with `hold_until_the_helper_releases_it`"]
pub(super) struct OneOffAcquisitionCountReservation<'registry> {
    registry: &'registry EscalateHandleRegistry,
    reserved_shape: Option<OneOffAcquisitionShape>,
}

impl<'registry> OneOffAcquisitionCountReservation<'registry> {
    /// The reservation of one count of `reserved_shape` that `registry` has
    /// just counted.
    pub(super) fn of_a_count_the_registry_counted(
        registry: &'registry EscalateHandleRegistry,
        reserved_shape: OneOffAcquisitionShape,
    ) -> Self {
        Self {
            registry,
            reserved_shape: Some(reserved_shape),
        }
    }

    /// Hold `registered_handle` under `handle_id`, counted under the reserved
    /// shape, until the helper releases it.
    pub(super) fn hold_until_the_helper_releases_it(
        mut self,
        handle_id: String,
        registered_handle: RegisteredHandle,
    ) {
        // Taken out first: once held, the count is the held acquisition's,
        // and this reservation's drop has none to hand back.
        let Some(reserved_shape) = self.reserved_shape.take() else {
            return;
        };
        let replaced_under_the_same_handle_id = self
            .registry
            .locked_one_off_acquisitions_held_for_the_helper()
            .hold_a_reserved_acquisition(handle_id, reserved_shape, registered_handle);
        // Dropped with the registry's lock released: a held texture releases
        // its pool slot under the texture pool's lock.
        drop(replaced_under_the_same_handle_id);
    }
}

impl Drop for OneOffAcquisitionCountReservation<'_> {
    fn drop(&mut self) {
        if let Some(reserved_shape) = self.reserved_shape.take() {
            self.registry
                .locked_one_off_acquisitions_held_for_the_helper()
                .hand_back_an_unheld_reservation_of_shape(&reserved_shape);
        }
    }
}
