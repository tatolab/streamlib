// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Internal generated processor traits.
//!
//! **DO NOT USE DIRECTLY** - These traits are implementation details.
//! Use [`Processor`](super::Processor) trait instead.

mod generated_processor;
mod generated_processor_impl;

pub use generated_processor::GeneratedProcessor;
pub use generated_processor_impl::{
    DynGeneratedProcessor, OutOfProcessFarSideLinkDelivery, OutOfProcessLinkWiringEnvelope,
};
pub(crate) use generated_processor_impl::{
    FunctionNodeCallUnderWayInTheFarSide, FunctionNodePromotionAwaitedBeforeTheReaderRestarts,
    FunctionNodeThatOverranItsFarSidesFrameInterval,
};
