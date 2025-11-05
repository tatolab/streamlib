use std::time::Duration;

pub struct MediaClock;

impl MediaClock {
    #[inline]
    pub fn now() -> Duration {
        unsafe {
            let host_time = mach_absolute_time();
            let nanos = Self::host_time_to_nanos(host_time);
            Duration::from_nanos(nanos)
        }
    }

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
