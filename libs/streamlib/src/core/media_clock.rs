#[cfg(target_os = "macos")]
pub use crate::apple::media_clock::MediaClock;

#[cfg(not(target_os = "macos"))]
pub struct MediaClock;

#[cfg(not(target_os = "macos"))]
impl MediaClock {
    #[inline]
    pub fn now() -> Duration {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let start = START.get_or_init(|| std::time::Instant::now());
        start.elapsed()
    }
}
