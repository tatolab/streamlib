//! Standard processor traits
//!
//! Defines common processor types (Camera, Display) that platform implementations
//! provide concrete implementations for.

pub mod camera;
pub mod display;

pub use camera::{CameraProcessor, CameraDevice, CameraOutputPorts};
pub use display::{DisplayProcessor, WindowId, DisplayInputPorts};
