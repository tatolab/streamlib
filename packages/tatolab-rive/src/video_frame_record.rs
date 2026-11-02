// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `VideoFrame` message a `RiveSource` publishes: the msgpack map
//! `tatolab.stream.VideoFrame.from_bytes` reads.

/// One rendered frame, named by the output texture its pixels sit in.
#[derive(Debug, Clone, Copy)]
pub struct PublishedVideoFrame<'surface> {
    pub surface_id: &'surface str,
    pub width: u32,
    pub height: u32,
    pub timestamp_ns: i64,
    pub frames_per_second: u32,
    pub texture_layout: i32,
}

impl PublishedVideoFrame<'_> {
    /// The message bytes: `surface_id`, `width`, `height`, `timestamp_ns`,
    /// `fps` and `texture_layout`.
    pub fn to_msgpack(&self) -> Vec<u8> {
        let mut record = Vec::with_capacity(96);
        let write_failure = "writing msgpack into a Vec cannot fail";
        rmp::encode::write_map_len(&mut record, 6).expect(write_failure);
        rmp::encode::write_str(&mut record, "surface_id").expect(write_failure);
        rmp::encode::write_str(&mut record, self.surface_id).expect(write_failure);
        rmp::encode::write_str(&mut record, "width").expect(write_failure);
        rmp::encode::write_uint(&mut record, u64::from(self.width)).expect(write_failure);
        rmp::encode::write_str(&mut record, "height").expect(write_failure);
        rmp::encode::write_uint(&mut record, u64::from(self.height)).expect(write_failure);
        rmp::encode::write_str(&mut record, "timestamp_ns").expect(write_failure);
        rmp::encode::write_sint(&mut record, self.timestamp_ns).expect(write_failure);
        rmp::encode::write_str(&mut record, "fps").expect(write_failure);
        rmp::encode::write_uint(&mut record, u64::from(self.frames_per_second))
            .expect(write_failure);
        rmp::encode::write_str(&mut record, "texture_layout").expect(write_failure);
        rmp::encode::write_sint(&mut record, i64::from(self.texture_layout)).expect(write_failure);
        record
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_reads_back_as_the_map_video_frame_reads() {
        let frame = PublishedVideoFrame {
            surface_id: "slot-3#41",
            width: 1920,
            height: 1080,
            timestamp_ns: 123_456_789,
            frames_per_second: 60,
            texture_layout: 5,
        };
        let record = rmpv::decode::read_value(&mut frame.to_msgpack().as_slice())
            .expect("the record decodes");
        let entry = |key: &str| {
            record
                .as_map()
                .expect("a map")
                .iter()
                .find(|(entry_key, _)| entry_key.as_str() == Some(key))
                .map(|(_, value)| value.clone())
                .unwrap_or_else(|| panic!("no {key:?} in {record}"))
        };
        assert_eq!(entry("surface_id").as_str(), Some("slot-3#41"));
        assert_eq!(entry("width").as_u64(), Some(1920));
        assert_eq!(entry("height").as_u64(), Some(1080));
        assert_eq!(entry("timestamp_ns").as_i64(), Some(123_456_789));
        assert_eq!(entry("fps").as_u64(), Some(60));
        assert_eq!(entry("texture_layout").as_i64(), Some(5));
    }
}
