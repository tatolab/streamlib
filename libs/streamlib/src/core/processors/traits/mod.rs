// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Core processor traits.

mod config;
mod continuous;
mod manual;
mod reactive;

pub use config::{Config, ConfigValidationError};
pub use continuous::ContinuousProcessor;
pub use manual::ManualProcessor;
pub use reactive::ReactiveProcessor;
