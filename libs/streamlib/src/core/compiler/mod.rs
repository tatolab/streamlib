//! Graph compilation pipeline.
//!
//! Converts graph topology changes into running processor instances.
//! The compilation process has 4 phases:
//! 1. CREATE - Instantiate processor instances from factory
//! 2. WIRE - Create ring buffers and connect ports
//! 3. SETUP - Call __generated_setup on each processor
//! 4. START - Spawn threads based on execution config

mod core;
mod phases;
mod wiring;

pub use self::core::Compiler;
