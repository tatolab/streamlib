// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Core processor traits.

mod config;
mod continuous;
mod manual;
mod reactive;

pub use config::{Config, ConfigValidationError};
pub use continuous::ContinuousProcessor;
pub use manual::ManualProcessor;
pub use reactive::ReactiveProcessor;
