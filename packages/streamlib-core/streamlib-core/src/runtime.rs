//! Core StreamRuntime trait definition
//!
//! This trait defines the platform-agnostic API for GPU-based video processing.
//! Platform-specific implementations (Metal, Vulkan) provide concrete types.

use crate::texture::GpuTexture;
use anyhow::Result;

/// Core runtime trait - provides GPU primitives for agents to compose
pub trait StreamRuntime {
    /// Create a GPU texture with specified dimensions
    fn create_texture(&mut self, width: u32, height: u32) -> Result<GpuTexture>;

    /// Create a compute shader from HLSL source
    fn create_compute_shader(&mut self, hlsl: &str) -> Result<ShaderId>;

    /// Execute a compute shader on GPU textures
    fn run_shader(
        &mut self,
        shader: ShaderId,
        inputs: &[GpuTexture],
    ) -> Result<GpuTexture>;

    // Platform capabilities (implemented per-platform)

    /// Get GPU texture from camera device
    fn get_camera_texture(&mut self, device: &str) -> Result<GpuTexture>;

    /// Display a GPU texture
    fn display_texture(&mut self, texture: GpuTexture) -> Result<()>;

    // Stream graph management

    /// Add a stream handler to the runtime
    fn add_stream(&mut self, handler: Box<dyn StreamHandler>);

    /// Connect output port to input port
    fn connect(&mut self, output: OutputPort, input: InputPort);

    /// Run the stream processing loop
    fn run(&mut self) -> Result<()>;
}

/// Opaque shader ID returned by create_compute_shader
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShaderId(pub u64);

/// Stream handler trait - implemented by decorators
pub trait StreamHandler: Send {
    fn process(
        &mut self,
        runtime: &mut dyn StreamRuntime,
        inputs: &[GpuTexture],
    ) -> Result<Vec<GpuTexture>>;
}

/// Output port identifier
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OutputPort {
    pub stream_id: String,
    pub port_name: String,
}

/// Input port identifier
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InputPort {
    pub stream_id: String,
    pub port_name: String,
}
