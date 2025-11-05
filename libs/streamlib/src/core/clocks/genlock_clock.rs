
use super::Clock;
use super::software_clock::SoftwareClock;

pub struct GenlockClock {
    fallback: SoftwareClock,
}

impl GenlockClock {
    pub fn new() -> Self {
        tracing::warn!("GenlockClock not implemented, using software fallback");
        Self {
            fallback: SoftwareClock::with_description("Genlock Stub Clock".to_string()),
        }
    }
}

impl Default for GenlockClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for GenlockClock {
    fn now_ns(&self) -> i64 {
        self.fallback.now_ns()
    }

    fn description(&self) -> &str {
        "Genlock Clock (stub)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_genlock_clock_fallback() {
        let clock = GenlockClock::new();
        let t1 = clock.now_ns();

        thread::sleep(Duration::from_millis(5));

        let t2 = clock.now_ns();
        assert!(t2 > t1);
    }
}
