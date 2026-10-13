// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The count a helper's one-off acquisition reserves before it allocates.

use super::{EscalateHandleRegistry, RegisteredHandle};
use crate::core::context::OneOffAcquisitionShape;

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
