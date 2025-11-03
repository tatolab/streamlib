use std::time::{SystemTime, UNIX_EPOCH};

#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

pub fn mach_ticks_to_ns(ticks: u64) -> i64 {
    unsafe {
        let mut timebase = MachTimebaseInfo { numer: 0, denom: 0 };
        mach_timebase_info(&mut timebase);
        (ticks * timebase.numer as u64 / timebase.denom as u64) as i64
    }
}

pub fn mach_now_ns() -> i64 {
    unsafe {
        let ticks = mach_absolute_time();
        mach_ticks_to_ns(ticks)
    }
}

pub fn system_time_to_ns(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mach_ticks_conversion() {
        let ns1 = mach_now_ns();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let ns2 = mach_now_ns();

        assert!(ns2 > ns1);
        let elapsed = ns2 - ns1;
        assert!(elapsed >= 10_000_000 && elapsed < 20_000_000);
    }

    #[test]
    fn test_system_time_conversion() {
        let now = SystemTime::now();
        let ns = system_time_to_ns(now);
        assert!(ns > 0);
    }
}
