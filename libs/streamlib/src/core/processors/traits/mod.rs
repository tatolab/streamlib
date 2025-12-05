// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Core processor traits.

pub mod base_processor;
pub mod processor;

pub use base_processor::{BaseProcessor, ProcessorType};
pub use processor::Processor;
