// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

pub mod arkit;
// TODO: Migrate to iceoryx2 API (depends on AudioDevice)
// pub mod audio_utils;
pub mod corevideo_ffi;
pub mod iosurface;
pub mod media_clock;
pub mod muxer;
pub mod pixel_transfer;
pub mod texture;
pub mod texture_pool_macos;
pub mod videotoolbox;
pub mod vimage_ffi;
pub mod xpc_ffi;

// Note: WebRTC module moved to core::streaming

pub mod processors;

pub mod permissions;

pub mod main_thread;

pub mod runtime_ext;

pub mod time;

pub mod thread_priority;

pub use pixel_transfer::PixelTransferSession;

// Sources and sinks migrated to iceoryx2 API
pub use processors::{
    // Sources
    AppleAudioCaptureProcessor,
    // Sinks
    AppleAudioOutputProcessor,
    AppleCameraProcessor,
    AppleDisplayProcessor,
    AppleMp4WriterProcessor,
};

// Note: WebRTC types (WhipClient, WhepClient, etc.) are now in core::streaming
// Note: WHIP/WHEP processors are now in core::processors

// TODO: Re-export when processors are migrated to iceoryx2
// pub use crate::metal::MetalDevice;
// pub use videotoolbox::VideoToolboxEncoder;

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
