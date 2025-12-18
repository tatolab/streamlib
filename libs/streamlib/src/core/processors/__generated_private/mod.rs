// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Internal generated processor traits.
//!
//! **DO NOT USE DIRECTLY** - These traits are implementation details.
//! Use [`Processor`](super::Processor) trait instead.

mod generated_processor;
mod generated_processor_impl;

pub use generated_processor::GeneratedProcessor;
pub use generated_processor_impl::DynGeneratedProcessor;
