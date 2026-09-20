// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Which machine's monotonic clock the bags crossing one link were stamped on,
//! as `graph` renders it.
//!
//! Carried only by a link whose source is on another runtime: a link inside
//! this runtime was stamped on this machine, which the renderer answers without
//! being told. The cell is the mesh ingress table's own, so the rendering
//! follows what arrives rather than what was true when the link was wired.

use std::sync::Arc;

use crate::core::runtime::mesh::MachineClockARemoteLinkCarriesFrom;

/// The cell naming the machine a link from another runtime is carrying from.
pub struct TheMachineClockALinksStampsAreTakenOnComponent(
    pub Arc<MachineClockARemoteLinkCarriesFrom>,
);

impl TheMachineClockALinksStampsAreTakenOnComponent {
    /// The machine as the canonical UUID text, or `None` while nothing has
    /// crossed the link.
    pub fn as_uuid_text(&self) -> Option<String> {
        self.0.what_it_is_now().map(|machine| machine.to_string())
    }
}
