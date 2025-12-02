//! Delegate traits for extensible runtime behavior.
//!
//! Delegates allow customization of:
//! - Factory: How processors are instantiated
//! - Processor: Lifecycle callbacks (will_create, did_create, etc.)
//! - Scheduler: How processors are scheduled (thread, pool, main thread)

mod factory;
mod processor;
mod scheduler;

pub use factory::{DefaultFactory, FactoryAdapter, FactoryDelegate};
pub use processor::{DefaultProcessorDelegate, ProcessorDelegate};
pub use scheduler::{DefaultScheduler, SchedulerDelegate, SchedulingStrategy, ThreadPriority};
