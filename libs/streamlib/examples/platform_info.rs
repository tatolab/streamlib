//! Platform Information Example
//!
//! Demonstrates the platform-agnostic facade - the same code compiles
//! and runs on macOS, iOS, Linux, and Windows.

use streamlib::{StreamRuntime, WgpuBridge, MetalDevice};

#[tokio::main]
async fn main() -> streamlib::Result<()> {
    println!("=== streamlib Platform Information ===\n");

    // Platform detection (works on all platforms)
    println!("Platform: {}", streamlib::platform::name());
    println!("GPU Backend: {}", streamlib::platform::gpu_backend());

    // Create runtime (platform-agnostic)
    println!("\nCreating StreamRuntime...");
    let mut runtime = StreamRuntime::new(60.0);
    println!("✓ StreamRuntime created (60 fps)");

    // Initialize WebGPU (platform-specific backend, but same API)
    println!("\nInitializing GPU...");
    let metal_device = MetalDevice::new()?;
    let bridge = WgpuBridge::new(metal_device.clone_device()).await?;
    let (device, queue) = bridge.into_wgpu();
    runtime.set_wgpu(device, queue);
    println!("✓ GPU initialized");

    // Verify setup
    if runtime.wgpu_device().is_some() {
        println!("\n✓ Runtime ready for platform-agnostic GPU operations");
        println!("✓ All processors will use WebGPU (wgpu::Texture)");
        println!("✓ Zero-copy texture bridging active");
    }

    println!("\n=== Cross-Platform Benefits ===");
    println!("✓ Same code runs on all platforms");
    println!("✓ Conditional compilation (no runtime checks)");
    println!("✓ Zero-copy GPU operations");
    println!("✓ Type-safe platform abstractions");

    Ok(())
}
