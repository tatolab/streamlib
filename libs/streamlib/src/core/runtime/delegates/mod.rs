// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Default delegate implementations for the runtime.
//!
//! These provide concrete implementations of the delegate traits
//! defined in `core::delegates`.

mod factory;
mod link;
mod processor;
mod scheduler;

pub use factory::{DefaultFactory, FactoryAdapter};
pub use link::DefaultLinkDelegate;
pub use processor::DefaultProcessorDelegate;
pub use scheduler::DefaultScheduler;
