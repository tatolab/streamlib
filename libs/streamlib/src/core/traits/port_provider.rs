use super::super::ports::{StreamInput, StreamOutput};
use super::super::VideoFrame;

/// Helper trait for port access (kept separate from StreamProcessor for object safety)
///
/// Enables generic port access by name for dynamic runtime connections.
/// This trait allows the runtime to connect ports without knowing the
/// concrete processor type at compile time.
///
/// # Purpose
///
/// With PortProvider, the runtime can:
/// - Connect any processor types (Camera, Display, Python, ML, etc.)
/// - Access ports by name dynamically
/// - Maintain a unified connection API instead of hardcoded downcasts
///
/// # Design
///
/// Uses a closure-based API to support different port storage patterns:
/// - Native processors (Camera, Display) provide direct &mut references to struct fields
/// - Dynamic processors (PythonProcessor) temporarily lock Arc<Mutex<>> to provide access
///
/// The closure pattern ensures mutable access is always properly scoped and released.
///
/// # Example
///
/// ```ignore
/// // Native processor with struct fields
/// impl PortProvider for MyProcessor {
///     fn with_video_output_mut<F, R>(&mut self, name: &str, f: F) -> Option<R>
///     where F: FnOnce(&mut StreamOutput<VideoFrame>) -> R
///     {
///         match name {
///             "video" => Some(f(&mut self.output_ports.video)),
///             _ => None,
///         }
///     }
///
///     fn with_video_input_mut<F, R>(&mut self, name: &str, f: F) -> Option<R>
///     where F: FnOnce(&mut StreamInput<VideoFrame>) -> R
///     {
///         match name {
///             "video" => Some(f(&mut self.input_ports.video)),
///             _ => None,
///         }
///     }
/// }
///
/// // Dynamic processor with HashMap<String, Arc<Mutex<Port>>>
/// impl PortProvider for PythonProcessor {
///     fn with_video_output_mut<F, R>(&mut self, name: &str, f: F) -> Option<R>
///     where F: FnOnce(&mut StreamOutput<VideoFrame>) -> R
///     {
///         let port_arc = self.output_ports.ports.get(name)?;
///         let mut port = port_arc.lock().unwrap();
///         Some(f(&mut *port))
///     }
/// }
/// ```
pub trait PortProvider {
    /// Provide temporary mutable access to a video output port by name
    ///
    /// # Arguments
    ///
    /// * `name` - Port name (e.g., "video", "output", "processed")
    /// * `f` - Closure that receives mutable access to the port
    ///
    /// # Returns
    ///
    /// Some(R) if port exists and closure was called, None if port not found
    ///
    /// # Example
    ///
    /// ```ignore
    /// processor.with_video_output_mut("video", |output| {
    ///     output.write(frame);
    /// });
    /// ```
    fn with_video_output_mut<F, R>(&mut self, name: &str, f: F) -> Option<R>
    where
        F: FnOnce(&mut StreamOutput<VideoFrame>) -> R;

    /// Provide temporary mutable access to a video input port by name
    ///
    /// # Arguments
    ///
    /// * `name` - Port name (e.g., "video", "input", "source")
    /// * `f` - Closure that receives mutable access to the port
    ///
    /// # Returns
    ///
    /// Some(R) if port exists and closure was called, None if port not found
    ///
    /// # Example
    ///
    /// ```ignore
    /// processor.with_video_input_mut("video", |input| {
    ///     input.connect(upstream_buffer);
    /// });
    /// ```
    fn with_video_input_mut<F, R>(&mut self, name: &str, f: F) -> Option<R>
    where
        F: FnOnce(&mut StreamInput<VideoFrame>) -> R;
}
