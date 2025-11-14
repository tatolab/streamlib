// Platform-specific re-exports with unified names
// Users import these common names and get the appropriate platform implementation
#[cfg(target_os = "macos")]
pub use crate::apple::processors::mp4_writer::{
    AppleMp4WriterProcessor as Mp4WriterProcessor,
    AppleMp4WriterConfig as Mp4WriterConfig,
};

// Future platform implementations
// #[cfg(target_os = "linux")]
// pub use crate::linux::processors::mp4_writer::{
//     LinuxMp4WriterProcessor as Mp4WriterProcessor,
//     LinuxMp4WriterConfig as Mp4WriterConfig,
// };

// #[cfg(target_os = "windows")]
// pub use crate::windows::processors::mp4_writer::{
//     WindowsMp4WriterProcessor as Mp4WriterProcessor,
//     WindowsMp4WriterConfig as Mp4WriterConfig,
// };
