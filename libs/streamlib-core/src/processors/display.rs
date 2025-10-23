//! Display processor trait
//!
//! Defines the interface for display/window processors across platforms.

use crate::{StreamProcessor, StreamInput, VideoFrame};

/// Unique identifier for a display window
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct WindowId(pub u64);

/// Input ports for display processors
pub struct DisplayInputPorts {
    /// Video frame input (WebGPU textures)
    pub video: StreamInput<VideoFrame>,
}

/// Display processor trait
///
/// Platform implementations (AppleDisplayProcessor, etc.) implement this trait
/// to provide window/display functionality with WebGPU texture rendering.
///
/// Each DisplayProcessor instance manages one window.
pub trait DisplayProcessor: StreamProcessor {
    /// Set the window title
    fn set_window_title(&mut self, title: &str);

    /// Get the window ID (if window has been created)
    fn window_id(&self) -> Option<WindowId>;

    /// Get the input ports for this display
    fn input_ports(&mut self) -> &mut DisplayInputPorts;
}
