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
//! 1. **Scheduling Mode** = WHEN to run (loop, reactive, callback)
//! 2. **Thread Priority** = HOW IMPORTANT (real-time, high, normal)
//! 3. **Clock Source** = WHAT TIMING (audio, vsync, software)
//!
//! These are **orthogonal concerns** that compose cleanly:
//!
//! ```rust,ignore
//! SchedulingConfig {
//!     mode: SchedulingMode::Loop,
//!     priority: ThreadPriority::High,
//!     clock: ClockSource::Audio,
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
//! use streamlib::core::scheduling::{SchedulingConfig, SchedulingMode, ThreadPriority, ClockSource};
//!
//! // Audio effect processor - high priority loop
//! let audio_config = SchedulingConfig {
//!     mode: SchedulingMode::Loop,
//!     priority: ThreadPriority::High,
//!     clock: ClockSource::Audio,
//! };
//!
//! // ML inference - normal priority reactive
//! let ml_config = SchedulingConfig {
//!     mode: SchedulingMode::Reactive,
//!     priority: ThreadPriority::Normal,
//!     clock: ClockSource::Software,
//! };
//! ```

pub mod mode;
pub mod priority;
pub mod clock;
pub mod config;

// Re-export core types
pub use mode::SchedulingMode;
pub use priority::ThreadPriority;
pub use clock::{ClockSource, ClockConfig, ClockType, SyncMode};
pub use config::SchedulingConfig;
