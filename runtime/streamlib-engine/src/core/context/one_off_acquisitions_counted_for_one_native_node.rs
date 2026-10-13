// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A native node's one-off acquisitions, each counted against the node for as
//! long as the handle it was returned in lives.

#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::one_off_acquisitions_held_for_one_node::{
    OneOffAcquisitionCountsByShapeOfOneNode, OneOffAcquisitionShape,
};
use crate::core::Result;

/// The one-off acquisitions one native node holds, counted per shape.
#[derive(Default)]
pub(crate) struct OneOffAcquisitionsCountedForOneNativeNode {
    /// Taken by a counted handle's drop, so it recovers rather than panics
    /// when poisoned, and is never held across an allocation.
    counts_by_shape: Mutex<OneOffAcquisitionCountsByShapeOfOneNode>,
}

impl OneOffAcquisitionsCountedForOneNativeNode {
    /// An empty count for one native node.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn locked_counts_by_shape(&self) -> MutexGuard<'_, OneOffAcquisitionCountsByShapeOfOneNode> {
        self.counts_by_shape
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// How many one-offs of `shape` the node holds.
    #[cfg(test)]
    pub(crate) fn held_count_of_shape(&self, shape: &OneOffAcquisitionShape) -> usize {
        self.locked_counts_by_shape()
            .held_or_reserved_count_of_shape(shape)
    }
}

/// Acquire a one-off with `allocate`, counted under `shape` against
/// `one_offs_of_the_node` when there is a node and refused by name before
/// `allocate` runs once the node holds as many of `shape` as one node may.
pub(crate) fn acquire_a_one_off_counted_against_its_node<Acquisition>(
    one_offs_of_the_node: Option<&Arc<OneOffAcquisitionsCountedForOneNativeNode>>,
    shape: OneOffAcquisitionShape,
    allocate: impl FnOnce() -> Result<Acquisition>,
) -> Result<OneOffAcquisitionCountedForItsNode<Acquisition>> {
    let count_held_against_the_node = match one_offs_of_the_node {
        None => None,
        Some(one_offs_of_the_node) => {
            one_offs_of_the_node
                .locked_counts_by_shape()
                .reserve_one_more_of_shape(&shape)?;
            Some(OneOffAcquisitionCountHeldAgainstANativeNode {
                one_offs_of_the_node: Arc::clone(one_offs_of_the_node),
                shape,
            })
        }
    };
    let acquisition = allocate()?;
    Ok(OneOffAcquisitionCountedForItsNode {
        acquisition,
        count_held_against_the_node,
    })
}

/// One count of a shape a native node holds, handed back when it drops.
struct OneOffAcquisitionCountHeldAgainstANativeNode {
    one_offs_of_the_node: Arc<OneOffAcquisitionsCountedForOneNativeNode>,
    shape: OneOffAcquisitionShape,
}

impl Drop for OneOffAcquisitionCountHeldAgainstANativeNode {
    fn drop(&mut self) {
        self.one_offs_of_the_node
            .locked_counts_by_shape()
            .count_one_fewer_of_shape(&self.shape);
    }
}

/// A one-off acquisition a node holds, counted against the node's sixteen of
/// its shape until this drops; a clone of what it derefs to is not counted.
pub struct OneOffAcquisitionCountedForItsNode<Acquisition> {
    // Declared first so the acquisition is freed before its count goes back.
    acquisition: Acquisition,
    count_held_against_the_node: Option<OneOffAcquisitionCountHeldAgainstANativeNode>,
}

impl<Acquisition> std::ops::Deref for OneOffAcquisitionCountedForItsNode<Acquisition> {
    type Target = Acquisition;

    fn deref(&self) -> &Acquisition {
        &self.acquisition
    }
}

impl<AcquiredUnderId, Acquisition>
    OneOffAcquisitionCountedForItsNode<(AcquiredUnderId, Acquisition)>
{
    /// Split off the id an acquisition was answered with, the count staying
    /// on the acquisition.
    pub(crate) fn split_off_the_id_it_was_acquired_under(
        self,
    ) -> (
        AcquiredUnderId,
        OneOffAcquisitionCountedForItsNode<Acquisition>,
    ) {
        let (acquired_under_id, acquisition) = self.acquisition;
        (
            acquired_under_id,
            OneOffAcquisitionCountedForItsNode {
                acquisition,
                count_held_against_the_node: self.count_held_against_the_node,
            },
        )
    }
}

impl<Acquisition: std::fmt::Debug> std::fmt::Debug
    for OneOffAcquisitionCountedForItsNode<Acquisition>
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OneOffAcquisitionCountedForItsNode")
            .field("acquisition", &self.acquisition)
            .field(
                "counted_against_its_node",
                &self.count_held_against_the_node.is_some(),
            )
            .finish()
    }
}
