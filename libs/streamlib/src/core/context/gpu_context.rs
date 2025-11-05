//! GPU Context - Shared WebGPU device and queue
//!
//! The GPU context is created by the runtime and passed to all processors.
//! This ensures all processors share the same WebGPU device, allowing
//! zero-copy texture sharing between processors.
//!
//! This is equivalent to Python streamlib's `gpu_ctx`.

use crate::core::{Result, StreamError};
use std::sync::Arc;
use wgpu;

/// Shared GPU context passed to all processors
///
/// Contains the WebGPU device and queue that all processors must use
/// for GPU operations. This ensures textures can be shared between
/// processors without device isolation issues.
#[derive(Clone)]
pub struct GpuContext {
    /// WebGPU device (shared across all processors)
    device: Arc<wgpu::Device>,

    /// WebGPU queue (shared across all processors)
    queue: Arc<wgpu::Queue>,
}

impl GpuContext {
    /// Create a new GPU context with the given device and queue
    ///
    /// This is called by the runtime during initialization.
    /// Platform-specific code provides the device/queue.
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self {
            device: Arc::new(device),
            queue: Arc::new(queue),
        }
    }

    /// Get the WebGPU device
    pub fn device(&self) -> &Arc<wgpu::Device> {
        &self.device
    }

    /// Get the WebGPU queue
    pub fn queue(&self) -> &Arc<wgpu::Queue> {
        &self.queue
    }

    /// Get both device and queue as a tuple
    pub fn device_and_queue(&self) -> (&Arc<wgpu::Device>, &Arc<wgpu::Queue>) {
        (&self.device, &self.queue)
    }

    /// Initialize GPU context for the current platform
    ///
    /// This automatically selects the correct backend (Metal, Vulkan, D3D12)
    /// based on the platform.
    pub async fn init_for_platform() -> Result<Self> {
        // Create wgpu instance with platform-specific backend
        let backends = if cfg!(target_os = "macos") || cfg!(target_os = "ios") {
            wgpu::Backends::METAL
        } else if cfg!(target_os = "linux") {
            wgpu::Backends::VULKAN
        } else if cfg!(target_os = "windows") {
            wgpu::Backends::DX12
        } else {
            return Err(StreamError::GpuError(
                "Unsupported platform for GPU initialization".into()
            ));
        };

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends,
            ..Default::default()
        });

        // Request adapter
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| StreamError::GpuError(format!("Failed to find GPU adapter: {}", e)))?;

        tracing::info!(
            "GPU: Using adapter '{}' (backend: {:?})",
            adapter.get_info().name,
            adapter.get_info().backend
        );

        // Request device and queue with timestamp query support for GPU performance monitoring
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("StreamLib GPU Context"),
                required_features: wgpu::Features::TIMESTAMP_QUERY
                    | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: Default::default(),
            })
            .await
            .map_err(|e| StreamError::GpuError(format!("Failed to create device: {}", e)))?;

        tracing::info!("GPU: Device and queue created successfully");
        tracing::info!("GPU: Device address: {:p}", &device);
        tracing::info!("GPU: Queue address: {:p}", &queue);

        Ok(Self::new(device, queue))
    }
}

impl std::fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContext")
            .field("device", &format!("{:p}", self.device.as_ref()))
            .field("queue", &format!("{:p}", self.queue.as_ref()))
            .finish()
    }
}
