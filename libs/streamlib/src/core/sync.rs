//! Multimodal Synchronization Utilities
//!
//! Utilities for synchronizing data streams with different timing characteristics:
//! - Audio: Continuous samples at high rate (e.g., 48000 Hz)
//! - Video: Discrete frames at lower rate (e.g., 30 Hz camera)
//! - Display: Fixed refresh rate (e.g., 60 Hz)
//!
//! These utilities help processors align timestamps across modalities.

use crate::core::{VideoFrame, AudioFrame};

/// Default tolerance for considering frames synchronized (in milliseconds)
///
/// 16.6ms ≈ one frame at 60 FPS - a reasonable default for real-time systems
pub const DEFAULT_SYNC_TOLERANCE_MS: f64 = 16.6;

/// Calculate timestamp delta between two frames in milliseconds
///
/// Returns absolute difference - always positive regardless of order.
///
/// # Example
/// ```
/// use streamlib::sync::timestamp_delta_ms;
///
/// let delta = timestamp_delta_ms(1_000_000_000, 1_000_000_500);
/// assert_eq!(delta, 500.0); // 500ms difference
/// ```
pub fn timestamp_delta_ms(timestamp_a_ns: i64, timestamp_b_ns: i64) -> f64 {
    let delta_ns = (timestamp_a_ns - timestamp_b_ns).abs();
    delta_ns as f64 / 1_000_000.0
}

/// Calculate timestamp delta between video frame and audio frame
///
/// Note: VideoFrame uses f64 seconds, AudioFrame uses i64 nanoseconds.
/// This function converts VideoFrame timestamp to nanoseconds for comparison.
pub fn video_audio_delta_ms(video: &VideoFrame, audio: &AudioFrame) -> f64 {
    let video_ns = (video.timestamp * 1_000_000_000.0) as i64;
    timestamp_delta_ms(video_ns, audio.timestamp_ns)
}

/// Check if two timestamps are synchronized within tolerance
///
/// # Arguments
/// * `timestamp_a_ns` - First timestamp in nanoseconds
/// * `timestamp_b_ns` - Second timestamp in nanoseconds
/// * `tolerance_ms` - Maximum allowed difference in milliseconds
///
/// # Example
/// ```
/// use streamlib::sync::are_synchronized;
///
/// // Frames within 10ms are synchronized
/// assert!(are_synchronized(1_000_000_000, 1_000_005_000, 10.0));
///
/// // Frames 50ms apart are not synchronized with 10ms tolerance
/// assert!(!are_synchronized(1_000_000_000, 1_000_050_000, 10.0));
/// ```
pub fn are_synchronized(timestamp_a_ns: i64, timestamp_b_ns: i64, tolerance_ms: f64) -> bool {
    timestamp_delta_ms(timestamp_a_ns, timestamp_b_ns) <= tolerance_ms
}

/// Check if video and audio frames are synchronized
///
/// Uses default tolerance of 16.6ms (one 60 Hz frame).
pub fn video_audio_synchronized(video: &VideoFrame, audio: &AudioFrame) -> bool {
    let video_ns = (video.timestamp * 1_000_000_000.0) as i64;
    are_synchronized(video_ns, audio.timestamp_ns, DEFAULT_SYNC_TOLERANCE_MS)
}

/// Check if video and audio frames are synchronized with custom tolerance
pub fn video_audio_synchronized_with_tolerance(
    video: &VideoFrame,
    audio: &AudioFrame,
    tolerance_ms: f64,
) -> bool {
    let video_ns = (video.timestamp * 1_000_000_000.0) as i64;
    are_synchronized(video_ns, audio.timestamp_ns, tolerance_ms)
}

/// Multimodal frame buffer for timestamp-based synchronization
///
/// Holds the latest frame from multiple modalities and allows matching by timestamp.
/// Useful for processors that need synchronized multimodal input (e.g., audio-visual effects).
///
/// # Example
/// ```
/// use streamlib::sync::MultimodalBuffer;
///
/// let mut buffer = MultimodalBuffer::new();
///
/// // Store frames as they arrive
/// buffer.store_video(video_frame);
/// buffer.store_audio(audio_frame);
///
/// // Get synchronized pair if available
/// if let Some((video, audio)) = buffer.get_synchronized_pair(16.6) {
///     // Process synchronized frames
/// }
/// ```
#[derive(Default)]
pub struct MultimodalBuffer {
    /// Latest video frame
    pub video: Option<VideoFrame>,

    /// Latest audio frame
    pub audio: Option<AudioFrame>,
}

impl MultimodalBuffer {
    /// Create new empty buffer
    pub fn new() -> Self {
        Self {
            video: None,
            audio: None,
        }
    }

    /// Store video frame (replaces previous frame)
    pub fn store_video(&mut self, frame: VideoFrame) {
        self.video = Some(frame);
    }

    /// Store audio frame (replaces previous frame)
    pub fn store_audio(&mut self, frame: AudioFrame) {
        self.audio = Some(frame);
    }

    /// Get synchronized video-audio pair if available
    ///
    /// Returns `Some((video, audio))` if both frames exist and are within tolerance.
    /// Consumes both frames (removes them from buffer).
    pub fn get_synchronized_pair(&mut self, tolerance_ms: f64) -> Option<(VideoFrame, AudioFrame)> {
        if let (Some(ref video), Some(ref audio)) = (&self.video, &self.audio) {
            if video_audio_synchronized_with_tolerance(video, audio, tolerance_ms) {
                // Take both frames (remove from buffer)
                let video = self.video.take().unwrap();
                let audio = self.audio.take().unwrap();
                return Some((video, audio));
            }
        }
        None
    }

    /// Get synchronized video-audio pair without consuming (peek)
    ///
    /// Returns references to frames if both exist and are within tolerance.
    /// Does not remove frames from buffer.
    pub fn peek_synchronized_pair(&self, tolerance_ms: f64) -> Option<(&VideoFrame, &AudioFrame)> {
        if let (Some(ref video), Some(ref audio)) = (&self.video, &self.audio) {
            if video_audio_synchronized_with_tolerance(video, audio, tolerance_ms) {
                return Some((video, audio));
            }
        }
        None
    }

    /// Check which frame is older (arrived earlier in time)
    ///
    /// Returns:
    /// - `Some(true)` if video is older than audio
    /// - `Some(false)` if audio is older than video
    /// - `None` if either frame is missing
    pub fn is_video_older(&self) -> Option<bool> {
        if let (Some(ref video), Some(ref audio)) = (&self.video, &self.audio) {
            let video_ns = (video.timestamp * 1_000_000_000.0) as i64;
            Some(video_ns < audio.timestamp_ns)
        } else {
            None
        }
    }

    /// Clear all stored frames
    pub fn clear(&mut self) {
        self.video = None;
        self.audio = None;
    }

    /// Get timestamp delta between stored frames in milliseconds
    ///
    /// Returns `None` if either frame is missing.
    pub fn delta_ms(&self) -> Option<f64> {
        if let (Some(ref video), Some(ref audio)) = (&self.video, &self.audio) {
            Some(video_audio_delta_ms(video, audio))
        } else {
            None
        }
    }
}

/// Sample-and-hold buffer for managing multiple asynchronous inputs
///
/// Stores the last received value from each named input. When collecting all inputs,
/// returns the most recent value for each, using held (previous) values for inputs
/// that haven't arrived yet this cycle.
///
/// This is essential for real-time mixing/muxing where inputs arrive at different times
/// but need to be combined. Industry-standard approach used in audio mixers, video muxers,
/// and multi-sensor fusion systems.
///
/// # Example
///
/// ```rust
/// use streamlib::sync::SampleAndHoldBuffer;
///
/// // Create buffer for 3 audio inputs
/// let mut buffer = SampleAndHoldBuffer::new(vec!["tone1", "tone2", "tone3"]);
///
/// // First cycle: only tone1 and tone2 arrive
/// buffer.update("tone1", audio_frame_1);
/// buffer.update("tone2", audio_frame_2);
///
/// // Can't collect yet - tone3 has never arrived
/// assert!(buffer.collect_all().is_none());
///
/// // Second cycle: tone3 arrives
/// buffer.update("tone3", audio_frame_3);
///
/// // Now we can collect all 3 (tone1 and tone2 held from previous cycle)
/// let all_inputs = buffer.collect_all().unwrap();
/// // Mix all 3 together!
/// ```
///
/// # Generic Over Any Type
///
/// Works with AudioFrame, VideoFrame, or any cloneable type:
/// ```rust
/// let audio_buffer = SampleAndHoldBuffer::<AudioFrame>::new(vec!["mic1", "mic2"]);
/// let video_buffer = SampleAndHoldBuffer::<VideoFrame>::new(vec!["cam1", "cam2"]);
/// ```
pub struct SampleAndHoldBuffer<T: Clone> {
    /// Map of input name → last received value
    inputs: std::collections::HashMap<String, Option<T>>,
}

impl<T: Clone> SampleAndHoldBuffer<T> {
    /// Create new buffer with named inputs
    ///
    /// # Arguments
    /// * `input_names` - Names of all inputs that will be tracked
    ///
    /// # Example
    /// ```rust
    /// let buffer = SampleAndHoldBuffer::<AudioFrame>::new(
    ///     vec!["input_0", "input_1", "input_2"]
    /// );
    /// ```
    pub fn new<S: Into<String>>(input_names: impl IntoIterator<Item = S>) -> Self {
        let inputs = input_names
            .into_iter()
            .map(|name| (name.into(), None))
            .collect();

        Self { inputs }
    }

    /// Update an input with new data
    ///
    /// Stores the value and marks this input as "having data".
    /// This value will be held (reused) until updated again.
    pub fn update(&mut self, input_name: &str, value: T) {
        if let Some(slot) = self.inputs.get_mut(input_name) {
            *slot = Some(value);
        } else {
            tracing::warn!(
                "SampleAndHoldBuffer: Received data for unknown input '{}', ignoring",
                input_name
            );
        }
    }

    /// Collect all inputs, using held values where needed
    ///
    /// Returns `Some(Vec<T>)` if all inputs have received at least one value.
    /// Returns `None` if any input has never received data (cold start).
    ///
    /// # Ordering
    /// Values are returned in alphabetical order by input name for consistency.
    ///
    /// # Example
    /// ```rust
    /// // First call: tone1 and tone2 arrive
    /// buffer.update("tone1", frame1.clone());
    /// buffer.update("tone2", frame2.clone());
    /// buffer.update("tone3", frame3.clone());
    ///
    /// // All 3 present
    /// let samples = buffer.collect_all().unwrap();
    ///
    /// // Second call: only tone1 arrives
    /// buffer.update("tone1", new_frame1.clone());
    ///
    /// // Collects: new_frame1, frame2 (held), frame3 (held)
    /// let samples = buffer.collect_all().unwrap();
    /// ```
    pub fn collect_all(&self) -> Option<Vec<T>> {
        // Check if all inputs have data (no cold start)
        if self.inputs.values().any(|v| v.is_none()) {
            return None;
        }

        // Collect all values in sorted order for consistency
        let mut keys: Vec<_> = self.inputs.keys().collect();
        keys.sort();

        Some(
            keys.iter()
                .filter_map(|key| self.inputs.get(*key).and_then(|v| v.as_ref()))
                .cloned()
                .collect()
        )
    }

    /// Collect all inputs as a named map
    ///
    /// Returns `Some(HashMap<String, T>)` with all inputs.
    /// Returns `None` if any input has never received data.
    ///
    /// Useful when you need to know which value came from which input.
    pub fn collect_all_named(&self) -> Option<std::collections::HashMap<String, T>> {
        // Check if all inputs have data
        if self.inputs.values().any(|v| v.is_none()) {
            return None;
        }

        Some(
            self.inputs
                .iter()
                .filter_map(|(k, v)| v.as_ref().map(|val| (k.clone(), val.clone())))
                .collect()
        )
    }

    /// Get the number of inputs being tracked
    pub fn len(&self) -> usize {
        self.inputs.len()
    }

    /// Check if buffer is empty (no inputs tracked)
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty()
    }

    /// Clear all held values (reset to cold start)
    ///
    /// Useful if you want to force all inputs to provide new data before mixing.
    pub fn clear(&mut self) {
        for value in self.inputs.values_mut() {
            *value = None;
        }
    }

    /// Check if a specific input has received data at least once
    pub fn has_data(&self, input_name: &str) -> bool {
        self.inputs
            .get(input_name)
            .map(|v| v.is_some())
            .unwrap_or(false)
    }

    /// Check if all inputs have received data at least once
    pub fn all_ready(&self) -> bool {
        self.inputs.values().all(|v| v.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_timestamp_delta() {
        // Same timestamp = 0 delta
        assert_eq!(timestamp_delta_ms(1_000_000_000, 1_000_000_000), 0.0);

        // 1ms difference
        assert_eq!(timestamp_delta_ms(1_000_000_000, 1_001_000_000), 1.0);

        // Order doesn't matter (absolute value)
        assert_eq!(timestamp_delta_ms(1_001_000_000, 1_000_000_000), 1.0);

        // 16.6ms (one 60 Hz frame)
        let delta = timestamp_delta_ms(1_000_000_000, 1_016_600_000);
        assert!((delta - 16.6).abs() < 0.01);
    }

    #[test]
    fn test_are_synchronized() {
        // Within tolerance
        assert!(are_synchronized(1_000_000_000, 1_010_000_000, 20.0));

        // Exactly at tolerance boundary
        assert!(are_synchronized(1_000_000_000, 1_020_000_000, 20.0));

        // Exceeds tolerance
        assert!(!are_synchronized(1_000_000_000, 1_030_000_000, 20.0));
    }

    #[tokio::test]
    async fn test_multimodal_buffer() {
        use wgpu::{TextureDescriptor, TextureFormat, TextureUsages, Extent3d};

        // Create dummy GPU context for testing
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });

        let adapter = instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: None,
            force_fallback_adapter: false,
        }).await
        .expect("Failed to find adapter");

        let (device, _queue) = adapter.request_device(
            &wgpu::DeviceDescriptor {
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                label: None,
                trace: Default::default(),
            },
        ).await
        .expect("Failed to create device");

        let texture = device.create_texture(&TextureDescriptor {
            label: Some("test_texture"),
            size: Extent3d {
                width: 1920,
                height: 1080,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TextureFormat::Rgba8UnormSrgb,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let video_frame = VideoFrame {
            texture: Arc::new(texture),
            format: TextureFormat::Rgba8UnormSrgb,
            timestamp: 1.0, // 1 second (f64)
            frame_number: 0,
            width: 1920,
            height: 1080,
            metadata: None,
        };

        let audio_frame = AudioFrame {
            samples: Arc::new(vec![0.0; 2048]),
            gpu_buffer: None,
            timestamp_ns: 1_010_000_000, // 10ms after video (1.01 seconds in nanoseconds)
            frame_number: 0,
            sample_count: 2048,
            sample_rate: 48000,
            channels: 2,
            format: crate::core::AudioFormat::F32,
            metadata: None,
        };

        let mut buffer = MultimodalBuffer::new();
        buffer.store_video(video_frame);
        buffer.store_audio(audio_frame);

        // Frames are 10ms apart - should be synchronized with 20ms tolerance
        assert!(buffer.peek_synchronized_pair(20.0).is_some());

        // But not with 5ms tolerance
        assert!(buffer.peek_synchronized_pair(5.0).is_none());

        // Delta should be ~10ms
        let delta = buffer.delta_ms().unwrap();
        assert!((delta - 10.0).abs() < 0.01);

        // Video is older
        assert_eq!(buffer.is_video_older(), Some(true));
    }
}
