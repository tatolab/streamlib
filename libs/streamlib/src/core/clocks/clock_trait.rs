
use std::time::Duration;

pub trait Clock: Send + Sync {
    fn now_ns(&self) -> i64;

    fn now(&self) -> Duration {
        Duration::from_nanos(self.now_ns() as u64)
    }

    fn rate_hz(&self) -> Option<f64> {
        None
    }

    fn description(&self) -> &str;
}
