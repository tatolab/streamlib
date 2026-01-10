// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

pub mod arkit;
pub mod audio_utils;
pub mod corevideo_ffi;
pub mod iosurface;
pub mod media_clock;
pub mod pixel_transfer;
pub mod texture;
pub mod texture_pool_macos;
pub mod videotoolbox;
pub mod vimage_ffi;
pub mod webrtc;

pub mod processors;

pub mod permissions;

pub mod main_thread;

pub mod runtime_ext;

pub mod time;

pub mod thread_priority;

pub use crate::metal::MetalDevice;
pub use pixel_transfer::PixelTransferSession;

pub use processors::{
    AppleAudioCaptureProcessor,
    AppleAudioOutputProcessor,
    // Sources
    AppleCameraProcessor,
    // Sinks
    AppleDisplayProcessor,
    AppleMp4WriterProcessor,
    WebRtcWhepConfig,
    // WebRTC WHEP processor:
    WebRtcWhepProcessor,
    WebRtcWhipConfig,
    // WebRTC WHIP processor:
    WebRtcWhipProcessor,
};

// Re-export webrtc types
pub use webrtc::{WebRtcSession, WhepClient, WhepConfig, WhipClient, WhipConfig};

// Re-export videotoolbox types (VideoEncoderConfig and H264Profile now come from videotoolbox module)
pub use videotoolbox::{H264Profile, VideoCodec, VideoEncoderConfig, VideoToolboxEncoder};

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
