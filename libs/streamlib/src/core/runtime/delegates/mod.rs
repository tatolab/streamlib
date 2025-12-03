//! Default delegate implementations for the runtime.
//!
//! These provide concrete implementations of the delegate traits
//! defined in `core::delegates`.

mod factory;
mod processor;
mod scheduler;

pub use factory::{DefaultFactory, FactoryAdapter};
pub use processor::DefaultProcessorDelegate;
pub use scheduler::DefaultScheduler;
