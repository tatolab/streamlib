
pub mod arkit;
pub mod audio_utils;
pub mod iosurface;
pub mod pixel_transfer;
pub mod media_clock;
pub mod metal;
pub mod texture;
pub mod wgpu_bridge;

pub mod processors;

pub mod permissions;

pub mod main_thread;

pub mod runtime_ext;
pub use runtime_ext::configure_macos_event_loop;

pub mod time;

pub mod thread_priority;

pub mod display_link;


pub use metal::MetalDevice;
pub use wgpu_bridge::WgpuBridge;
pub use pixel_transfer::PixelTransferSession;

pub use processors::{
    // Sources
    AppleCameraProcessor,
    AppleAudioCaptureProcessor,
    // Sinks
    AppleDisplayProcessor,
    AppleAudioOutputProcessor,
    AppleMp4WriterProcessor,
};

#[cfg(test)]
mod tests {
    #[test]
    fn test_platform_detection() {
        #[cfg(target_os = "macos")]
        println!("Running on macOS");

        #[cfg(target_os = "ios")]
        println!("Running on iOS");
    }
}
