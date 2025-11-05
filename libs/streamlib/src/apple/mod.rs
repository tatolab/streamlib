
pub mod arkit;
pub mod audio_utils;
pub mod iosurface;
pub mod media_clock;
pub mod metal;
pub mod texture;
pub mod wgpu_bridge;

pub mod sources;

pub mod sinks;

pub mod permissions;

pub mod main_thread;

pub mod runtime_ext;
pub use runtime_ext::configure_macos_event_loop;

mod runtime_helpers;

pub mod time;

pub mod display_link;


pub use metal::MetalDevice;
pub use wgpu_bridge::WgpuBridge;

pub use sources::{
    AppleCameraProcessor,
    AppleAudioCaptureProcessor,
};

pub use sinks::{
    AppleDisplayProcessor,
    AppleAudioOutputProcessor,
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
