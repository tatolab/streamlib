// Platform-specific re-exports with unified names
// Users import these common names and get the appropriate platform implementation
#[cfg(target_os = "macos")]
pub use crate::apple::processors::audio_output::{
    AppleAudioOutputProcessor as AudioOutputProcessor,
    AppleAudioOutputConfig as AudioOutputConfig,
    AppleAudioDevice as AudioDevice,
};

// Future platform implementations
// #[cfg(target_os = "linux")]
// pub use crate::linux::processors::audio_output::{
//     LinuxAudioOutputProcessor as AudioOutputProcessor,
//     LinuxAudioOutputConfig as AudioOutputConfig,
//     LinuxAudioDevice as AudioDevice,
// };

// #[cfg(target_os = "windows")]
// pub use crate::windows::processors::audio_output::{
//     WindowsAudioOutputProcessor as AudioOutputProcessor,
//     WindowsAudioOutputConfig as AudioOutputConfig,
//     WindowsAudioDevice as AudioDevice,
// };
