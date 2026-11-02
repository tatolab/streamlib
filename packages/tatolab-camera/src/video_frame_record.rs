// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `VideoFrame` message this camera publishes: the msgpack map
//! `tatolab.stream.VideoFrame.from_bytes` reads.

use crate::v4l2_colour_description::CapturedFrameColourDescription;

/// One converted frame, named by the surface its pixels sit in.
#[derive(Debug, Clone, Copy)]
pub struct PublishedVideoFrame<'surface> {
    pub surface_id: &'surface str,
    pub width: u32,
    pub height: u32,
    pub timestamp_ns: i64,
    pub nominal_frames_per_second: Option<u32>,
    pub colour: CapturedFrameColourDescription,
}

impl PublishedVideoFrame<'_> {
    /// The message bytes: `surface_id`, `width`, `height`, `timestamp_ns`, `fps`
    /// and `color_info`, an unknown axis written as nil.
    pub fn to_msgpack(&self) -> Vec<u8> {
        let mut record = Vec::with_capacity(160);
        let write_failure = "writing msgpack into a Vec cannot fail";
        rmp::encode::write_map_len(&mut record, 6).expect(write_failure);
        write_text(&mut record, "surface_id");
        write_text(&mut record, self.surface_id);
        write_text(&mut record, "width");
        rmp::encode::write_uint(&mut record, u64::from(self.width)).expect(write_failure);
        write_text(&mut record, "height");
        rmp::encode::write_uint(&mut record, u64::from(self.height)).expect(write_failure);
        write_text(&mut record, "timestamp_ns");
        rmp::encode::write_sint(&mut record, self.timestamp_ns).expect(write_failure);
        write_text(&mut record, "fps");
        write_optional_uint(&mut record, self.nominal_frames_per_second);
        write_text(&mut record, "color_info");
        rmp::encode::write_map_len(&mut record, 4).expect(write_failure);
        for (axis, name) in [
            ("primaries", self.colour.primaries),
            ("transfer", self.colour.transfer),
            ("matrix", self.colour.matrix),
            ("range", self.colour.range),
        ] {
            write_text(&mut record, axis);
            match name {
                Some(name) => write_text(&mut record, name),
                None => rmp::encode::write_nil(&mut record).expect(write_failure),
            }
        }
        record
    }
}

fn write_text(record: &mut Vec<u8>, text: &str) {
    rmp::encode::write_str(record, text).expect("writing msgpack into a Vec cannot fail");
}

fn write_optional_uint(record: &mut Vec<u8>, value: Option<u32>) {
    let write_failure = "writing msgpack into a Vec cannot fail";
    match value {
        Some(value) => {
            rmp::encode::write_uint(record, u64::from(value)).expect(write_failure);
        }
        None => rmp::encode::write_nil(record).expect(write_failure),
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
            nominal_frames_per_second: None,
            colour: CapturedFrameColourDescription {
                primaries: Some("bt709"),
                transfer: Some("bt709"),
                matrix: Some("bt709"),
                range: None,
            },
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
        assert!(entry("fps").is_nil());
        let color_info = entry("color_info");
        let color_info = color_info.as_map().expect("a colour map");
        assert_eq!(color_info.len(), 4);
        assert!(
            color_info
                .iter()
                .any(|(axis, name)| axis.as_str() == Some("range") && name.is_nil())
        );
        assert!(
            color_info
                .iter()
                .any(|(axis, name)| axis.as_str() == Some("matrix")
                    && name.as_str() == Some("bt709"))
        );
    }
}
