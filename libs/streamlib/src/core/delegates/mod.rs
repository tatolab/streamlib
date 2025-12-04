//! Delegate traits for extensible runtime behavior.
//!
//! Delegates allow customization of:
//! - Factory: How processors are instantiated
//! - Processor: Lifecycle callbacks (will_create, did_create, etc.)
//! - Link: Wiring lifecycle callbacks (will_wire, did_wire, etc.)
//! - Scheduler: How processors are scheduled (thread, pool, main thread)
//!
//! Default implementations live in `runtime::delegates`.

mod factory;
mod link;
mod processor;
mod scheduler;

pub use factory::FactoryDelegate;
pub use link::LinkDelegate;
pub use processor::ProcessorDelegate;
pub use scheduler::{SchedulerDelegate, SchedulingStrategy, ThreadPriority};
