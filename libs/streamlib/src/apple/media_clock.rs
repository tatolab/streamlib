use std::time::Duration;

/// Platform-specific media clock that provides system-level monotonic time
///
/// This is a zero-cost abstraction over the platform's native high-resolution timer.
/// On macOS, this uses mach_absolute_time() which is the same timebase used by CoreAudio.
pub struct MediaClock;

impl MediaClock {
    /// Get current monotonic time since system boot
    ///
    /// This is the same timebase used by audio callbacks, ensuring perfect A/V sync.
    #[inline]
    pub fn now() -> Duration {
        unsafe {
            let host_time = mach_absolute_time();
            let nanos = Self::host_time_to_nanos(host_time);
            Duration::from_nanos(nanos)
        }
    }

    /// Get the raw platform timestamp (mach_absolute_time)
    #[inline]
    pub fn raw_timestamp() -> u64 {
        unsafe { mach_absolute_time() }
    }

    #[inline]
    fn host_time_to_nanos(host_time: u64) -> u64 {
        unsafe {
            let mut info: MachTimebaseInfo = std::mem::zeroed();
            mach_timebase_info(&mut info);
            host_time * info.numer as u64 / info.denom as u64
        }
    }
}

#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

#[link(name = "System", kind = "dylib")]
extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}
