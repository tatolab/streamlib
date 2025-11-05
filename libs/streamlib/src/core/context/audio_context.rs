
#[derive(Debug, Clone, Copy)]
pub struct AudioContext {
    pub sample_rate: u32,
    pub buffer_size: usize,
}

impl Default for AudioContext {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            buffer_size: 1024,
        }
    }
}

impl AudioContext {
    pub fn new(sample_rate: u32, buffer_size: usize) -> Self {
        Self {
            sample_rate,
            buffer_size,
        }
    }

    pub fn buffer_duration_ms(&self) -> f64 {
        (self.buffer_size as f64 / self.sample_rate as f64) * 1000.0
    }

    pub fn buffer_duration_ns(&self) -> i64 {
        (self.buffer_size as i64 * 1_000_000_000) / self.sample_rate as i64
    }

    pub fn samples_per_second(&self, channels: u32) -> usize {
        self.sample_rate as usize * channels as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_context_default() {
        let ctx = AudioContext::default();
        assert_eq!(ctx.sample_rate, 48000);
        assert_eq!(ctx.buffer_size, 512);
    }

    #[test]
    fn test_buffer_duration_ms() {
        let ctx = AudioContext::new(48000, 512);
        assert!((ctx.buffer_duration_ms() - 10.666).abs() < 0.001);
    }

    #[test]
    fn test_buffer_duration_ns() {
        let ctx = AudioContext::new(48000, 512);
        assert_eq!(ctx.buffer_duration_ns(), 10_666_666);
    }

    #[test]
    fn test_samples_per_second() {
        let ctx = AudioContext::new(48000, 512);

        assert_eq!(ctx.samples_per_second(1), 48000);

        assert_eq!(ctx.samples_per_second(2), 96000);

        assert_eq!(ctx.samples_per_second(6), 288000);
    }
}
