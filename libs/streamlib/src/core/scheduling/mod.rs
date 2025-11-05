//! Scheduling and thread priority configuration
//!
//! This module provides types and configuration for scheduling processors
//! and managing thread priorities in the real-time processing pipeline.
//!
//! ## Module Organization
//!
//! - `mode` - Scheduling modes (Loop, Reactive, Callback, Timer)
//! - `priority` - Thread priority levels and integration with platform threads
//! - `clock` - Clock sources and synchronization
//! - `config` - Combined scheduling configuration
//!
//! ## Design Philosophy
//!
//! Following the threading model documented in `threading.md`:
//!
//! 1. **Scheduling Mode** = WHEN to run (loop, pull, push)
//! 2. **Thread Priority** = HOW IMPORTANT (real-time, high, normal)
//!
//! These are **orthogonal concerns** that compose cleanly:
//!
//! ```rust,ignore
//! SchedulingConfig {
//!     mode: SchedulingMode::Loop,
//!     priority: ThreadPriority::High,
//! }
//! ```
//!
//! ## Real-Time Safety
//!
//! The scheduling system ensures:
//! - No allocations in hot paths
//! - Lock-free communication (rtrb ring buffers)
//! - Platform-native thread priorities
//! - Predictable latency for critical processors
//!
//! ## Usage
//!
//! ```rust,ignore
//! use streamlib::core::scheduling::{SchedulingConfig, SchedulingMode, ThreadPriority};
//!
//! // Audio effect processor - high priority loop
//! let audio_config = SchedulingConfig {
//!     mode: SchedulingMode::Loop,
//!     priority: ThreadPriority::High,
//! };
//!
//! // ML inference - normal priority push (event-driven)
//! let ml_config = SchedulingConfig {
//!     mode: SchedulingMode::Push,
//!     priority: ThreadPriority::Normal,
//! };
//! ```

pub mod mode;
pub mod priority;
pub mod clock;
pub mod config;

// Re-export core types
pub use mode::SchedulingMode;
pub use priority::ThreadPriority;
pub use config::SchedulingConfig;
