//! Video bus implementation with custom ring buffer
//!
//! Video frames are large GPU textures that need special handling:
//! - Ring buffer with fixed capacity (drop old frames if full)
//! - Each reader independently tracks its position
//! - Real-time priority: drop old frames over blocking
//!
//! # Design
//!
//! - Fixed-size ring buffer (default: 3 frames)
//! - Each reader has own position index
//! - Writer always succeeds (oldest frame dropped if full)
//! - Readers get None if they're behind or no data yet
//! - Thread-safe with Arc<Mutex<...>>

use super::{Bus, BusId, BusReader};
use crate::core::VideoFrame;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Video bus with ring buffer
pub struct VideoBus {
    id: BusId,
    /// Ring buffer of video frames (max capacity)
    buffer: Arc<Mutex<VecDeque<VideoFrame>>>,
    /// Maximum number of frames to buffer
    capacity: usize,
    /// Sequence number for each write (monotonic counter)
    write_seq: Arc<Mutex<u64>>,
}

impl VideoBus {
    pub fn new(capacity: usize) -> Self {
        Self {
            id: BusId::new(),
            buffer: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
            capacity,
            write_seq: Arc::new(Mutex::new(0)),
        }
    }

    pub fn with_default_capacity() -> Self {
        Self::new(3) // 3 frames = ~50ms at 60fps
    }
}

impl Bus<VideoFrame> for VideoBus {
    fn id(&self) -> BusId {
        self.id
    }

    fn create_reader(&self) -> Box<dyn BusReader<VideoFrame>> {
        Box::new(VideoBusReader {
            bus_id: self.id,
            buffer: Arc::clone(&self.buffer),
            write_seq: Arc::clone(&self.write_seq),
            last_read_seq: 0,
        })
    }

    fn write(&self, message: VideoFrame) {
        let mut buffer = self.buffer.lock().unwrap();
        let mut seq = self.write_seq.lock().unwrap();

        // Drop oldest frame if at capacity
        if buffer.len() >= self.capacity {
            buffer.pop_front();
            tracing::trace!("[VideoBus {}] Dropped oldest frame (capacity {})", self.id, self.capacity);
        }

        buffer.push_back(message);
        *seq += 1;

        tracing::trace!("[VideoBus {}] Wrote frame (seq: {}, buffer: {}/{})",
            self.id, *seq, buffer.len(), self.capacity);
    }
}

/// Reader for VideoBus
pub struct VideoBusReader {
    bus_id: BusId,
    buffer: Arc<Mutex<VecDeque<VideoFrame>>>,
    write_seq: Arc<Mutex<u64>>,
    /// Last sequence number read (to detect if we're behind)
    last_read_seq: u64,
}

impl BusReader<VideoFrame> for VideoBusReader {
    fn read_latest(&mut self) -> Option<VideoFrame> {
        let buffer = self.buffer.lock().unwrap();
        let write_seq = *self.write_seq.lock().unwrap();

        // If no new writes since last read, return None
        if write_seq == self.last_read_seq {
            return None;
        }

        // Get the latest frame (back of queue)
        let frame = buffer.back().cloned();

        if frame.is_some() {
            // Update our sequence to current
            self.last_read_seq = write_seq;
            tracing::trace!("[VideoBus {}] Reader read frame (seq: {})",
                self.bus_id, write_seq);
        }

        frame
    }

    fn has_data(&self) -> bool {
        let buffer = self.buffer.lock().unwrap();
        !buffer.is_empty()
    }

    fn clone_reader(&self) -> Box<dyn BusReader<VideoFrame>> {
        Box::new(Self {
            bus_id: self.bus_id,
            buffer: Arc::clone(&self.buffer),
            write_seq: Arc::clone(&self.write_seq),
            last_read_seq: self.last_read_seq,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{GpuContext, Texture, TextureDescriptor, TextureFormat, TextureUsages};

    fn create_test_frame(frame_num: u64) -> VideoFrame {
        let gpu = GpuContext::new_headless();
        let texture = gpu.device.create_texture(&TextureDescriptor {
            label: Some(&format!("test_frame_{}", frame_num)),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::COPY_SRC | TextureUsages::COPY_DST,
            view_formats: &[],
        });

        VideoFrame::new(Texture::new(texture), frame_num as i64, frame_num)
    }

    #[test]
    fn test_video_bus_basic() {
        let bus = VideoBus::with_default_capacity();
        let mut reader = bus.create_reader();

        // No data initially
        assert!(!reader.has_data());
        assert!(reader.read_latest().is_none());

        // Write a frame
        bus.write(create_test_frame(1));

        // Reader should now have data
        assert!(reader.has_data());
        let frame = reader.read_latest();
        assert!(frame.is_some());
        assert_eq!(frame.unwrap().frame_number, 1);

        // Reading again returns None (no new data)
        assert!(reader.read_latest().is_none());
    }

    #[test]
    fn test_video_bus_fan_out() {
        let bus = VideoBus::with_default_capacity();
        let mut reader1 = bus.create_reader();
        let mut reader2 = bus.create_reader();

        bus.write(create_test_frame(1));

        // Both readers get the frame
        assert!(reader1.read_latest().is_some());
        assert!(reader2.read_latest().is_some());
    }

    #[test]
    fn test_video_bus_overflow() {
        let bus = VideoBus::new(2); // Capacity of 2
        let mut reader = bus.create_reader();

        // Write 3 frames (will drop oldest)
        bus.write(create_test_frame(1));
        bus.write(create_test_frame(2));
        bus.write(create_test_frame(3));

        // Reader should get latest frame (3)
        let frame = reader.read_latest();
        assert!(frame.is_some());
        assert_eq!(frame.unwrap().frame_number, 3);
    }
}
