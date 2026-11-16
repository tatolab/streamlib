// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

mod generated_node;
mod generated_node_impl;

pub use generated_node::GeneratedNode;
pub use generated_node_impl::{
    DynGeneratedNode, OutOfProcessFarSideLinkDelivery, OutOfProcessLinkWiringEnvelope,
};
pub(crate) use generated_node_impl::{
    FunctionNodeCallUnderWayInTheFarSide, FunctionNodePromotionAwaitedBeforeTheReaderRestarts,
    FunctionNodeThatOverranItsFarSidesFrameInterval,
};
