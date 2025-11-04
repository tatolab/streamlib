
// Re-export platform-specific MediaClock
#[cfg(target_os = "macos")]
pub use crate::apple::media_clock::MediaClock;

// Fallback for non-macOS platforms
#[cfg(not(target_os = "macos"))]
pub struct MediaClock;

#[cfg(not(target_os = "macos"))]
impl MediaClock {
    /// Get current monotonic time
    #[inline]
    pub fn now() -> Duration {
        // Fallback to Instant on non-macOS platforms
        // Note: This doesn't have a stable epoch, but is monotonic
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let start = START.get_or_init(|| std::time::Instant::now());
        start.elapsed()
    }
}
