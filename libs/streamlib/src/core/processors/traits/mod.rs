// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Core processor traits.

pub mod config;
pub mod processor;

pub use config::{Config, ConfigValidationError};
pub use processor::Processor;
