// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

mod dyn_generated_node;
mod generated_node;
mod generated_node_impl;

pub use dyn_generated_node::DynGeneratedNode;
pub use generated_node::GeneratedNode;
pub(crate) use generated_node::land_the_unlinked_input_values_in_the_input_mailboxes;
pub(crate) use generated_node_impl::{
    FiresNotHandedToTheFarSide, FunctionNodeCallUnderWayInTheFarSide,
    FunctionNodePromotionAwaitedBeforeTheReaderRestarts,
    FunctionNodeThatOverranItsFarSidesFrameInterval,
};
pub use generated_node_impl::{OutOfProcessFarSideLinkDelivery, OutOfProcessLinkWiringEnvelope};
