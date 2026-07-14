// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

pub mod arkit;
pub mod audio_clock;
pub mod corevideo_ffi;
pub mod iosurface;
pub mod media_clock;
// AppleMp4Muxer + VideoToolboxEncoder/Decoder live in their domain
// packages' `_apple_impl_pending_/` directories (#786): muxer in
// packages/mp4, videotoolbox in packages/h264.
pub mod pixel_transfer;
pub mod texture;
pub mod texture_pool_macos;
pub mod vimage_ffi;
pub mod xpc_ffi;

pub mod permissions;

pub mod main_thread;

pub mod runtime_ext;

pub mod time;

pub mod thread_priority;

pub use audio_clock::CoreAudioClock;
pub use pixel_transfer::PixelTransferSession;

// TODO: Re-export when processors are migrated to iceoryx2
// pub use crate::metal::MetalDevice;

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
