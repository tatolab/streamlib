// Platform-specific re-exports with unified names
// Users import these common names and get the appropriate platform implementation
#[cfg(target_os = "macos")]
pub use crate::apple::processors::display::{
    AppleDisplayConfig as DisplayConfig, AppleDisplayProcessor as DisplayProcessor,
    AppleWindowId as WindowId,
};

// Future platform implementations
// #[cfg(target_os = "linux")]
// pub use crate::linux::processors::display::{
//     LinuxDisplayProcessor as DisplayProcessor,
//     LinuxDisplayConfig as DisplayConfig,
//     LinuxWindowId as WindowId,
// };

// #[cfg(target_os = "windows")]
// pub use crate::windows::processors::display::{
//     WindowsDisplayProcessor as DisplayProcessor,
//     WindowsDisplayConfig as DisplayConfig,
//     WindowsWindowId as WindowId,
// };
