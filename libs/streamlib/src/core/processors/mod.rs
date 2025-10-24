//! Standard processor traits
//!
//! Defines common processor types (Camera, Display) that platform implementations
//! provide concrete implementations for.

pub mod camera;
pub mod display;

#[cfg(feature = "debug-overlay")]
pub mod performance_overlay;

pub use camera::{CameraProcessor, CameraDevice, CameraOutputPorts};
pub use display::{DisplayProcessor, WindowId, DisplayInputPorts};

#[cfg(feature = "debug-overlay")]
pub use performance_overlay::{
    PerformanceOverlayProcessor, PerformanceOverlayInputPorts, PerformanceOverlayOutputPorts,
};
