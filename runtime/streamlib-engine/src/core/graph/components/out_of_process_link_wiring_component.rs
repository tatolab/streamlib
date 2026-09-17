// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use crate::core::processors::OutOfProcessLinkWiringEnvelope;

/// The link wiring of a processor whose iceoryx2 ports live out of process,
/// attached beside its instance.
///
/// Its presence is what says the processor is out of process, and it is how the
/// compiler op records and hands over that processor's links without taking the
/// processor's own lock — which a helper holds across its whole setup. Stored
/// but never rendered: `graph` reads what these links answered off each link.
pub struct OutOfProcessLinkWiringComponent(pub Arc<OutOfProcessLinkWiringEnvelope>);
