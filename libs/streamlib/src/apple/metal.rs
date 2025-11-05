
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLCommandBuffer, MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice,
};
use crate::core::{Result, StreamError};

pub struct MetalDevice {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    command_queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
}

impl MetalDevice {
    pub fn new() -> Result<Self> {
        let device =
            MTLCreateSystemDefaultDevice().ok_or_else(|| StreamError::GpuError("No Metal device available on this system. Metal requires macOS 10.11+ or iOS 8+.".into()))?;

        let command_queue = device.newCommandQueue().ok_or_else(|| {
            StreamError::GpuError("Failed to create Metal command queue".into())
        })?;

        Ok(Self {
            device,
            command_queue,
        })
    }

    pub fn device(&self) -> &ProtocolObject<dyn MTLDevice> {
        &self.device
    }

    pub fn clone_device(&self) -> Retained<ProtocolObject<dyn MTLDevice>> {
        Retained::clone(&self.device)
    }

    pub fn command_queue(&self) -> &ProtocolObject<dyn MTLCommandQueue> {
        &self.command_queue
    }

    pub fn clone_command_queue(&self) -> Retained<ProtocolObject<dyn MTLCommandQueue>> {
        Retained::clone(&self.command_queue)
    }

    pub fn create_command_buffer(&self) -> Result<Retained<ProtocolObject<dyn MTLCommandBuffer>>> {
        self.command_queue.commandBuffer().ok_or_else(|| {
            StreamError::GpuError("Failed to create Metal command buffer".into())
        })
    }

    pub fn name(&self) -> String {
        self.device.name().to_string()
    }
}

impl Default for MetalDevice {
    fn default() -> Self {
        Self::new().expect("Failed to create default Metal device")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metal_available() {
        let device = MetalDevice::new();
        assert!(device.is_ok(), "Metal should be available on macOS/iOS");
    }

    #[test]
    fn test_metal_device_info() {
        let device = MetalDevice::new().expect("Metal device");
        let name = device.name();
        assert!(!name.is_empty(), "Metal device should have a name");
        println!("Metal device: {}", name);
    }

    #[test]
    fn test_command_buffer_creation() {
        let device = MetalDevice::new().expect("Metal device");
        let cmd_buffer = device.create_command_buffer();
        assert!(cmd_buffer.is_ok(), "Should be able to create command buffer");
    }
}
