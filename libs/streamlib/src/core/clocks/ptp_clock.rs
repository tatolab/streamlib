
use super::Clock;
use super::software_clock::SoftwareClock;

pub struct PTPClock {
    fallback: SoftwareClock,
}

impl PTPClock {
    pub fn new() -> Self {
        tracing::warn!("PTPClock not implemented, using software fallback");
        Self {
            fallback: SoftwareClock::with_description("PTP Stub Clock".to_string()),
        }
    }
}

impl Default for PTPClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for PTPClock {
    fn now_ns(&self) -> i64 {
        self.fallback.now_ns()
    }

    fn description(&self) -> &str {
        "PTP Clock (stub)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_ptp_clock_fallback() {
        let clock = PTPClock::new();
        let t1 = clock.now_ns();

        thread::sleep(Duration::from_millis(5));

        let t2 = clock.now_ns();
        assert!(t2 > t1);
    }
}
