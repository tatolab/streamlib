// Platform-specific re-exports with unified names
// Users import these common names and get the appropriate platform implementation
#[cfg(target_os = "macos")]
pub use crate::apple::processors::audio_capture::{
    AppleAudioCaptureConfig as AudioCaptureConfig,
    AppleAudioCaptureProcessor as AudioCaptureProcessor, AppleAudioInputDevice as AudioInputDevice,
};

// Future platform implementations
// #[cfg(target_os = "linux")]
// pub use crate::linux::processors::audio_capture::{
//     LinuxAudioCaptureProcessor as AudioCaptureProcessor,
//     LinuxAudioCaptureConfig as AudioCaptureConfig,
//     LinuxAudioInputDevice as AudioInputDevice,
// };

// #[cfg(target_os = "windows")]
// pub use crate::windows::processors::audio_capture::{
//     WindowsAudioCaptureProcessor as AudioCaptureProcessor,
//     WindowsAudioCaptureConfig as AudioCaptureConfig,
//     WindowsAudioInputDevice as AudioInputDevice,
// };
