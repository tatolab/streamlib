// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `VideoFrame` message a painted page is published as: the msgpack map
//! `tatolab.stream.VideoFrame.from_bytes` reads.

/// One painted frame, named by the surface its pixels sit in.
#[derive(Debug, Clone, Copy)]
pub struct PaintedPageVideoFrame<'surface> {
    pub surface_id: &'surface str,
    pub width: u32,
    pub height: u32,
    pub timestamp_ns: i64,
    pub frames_per_second: u32,
}

/// A page paints sRGB: BT.709 primaries, the sRGB curve, RGB samples at full
/// range.
const PAINTED_PAGE_COLOUR_AXES: [(&str, &str); 4] = [
    ("primaries", "bt709"),
    ("transfer", "srgb"),
    ("matrix", "identity"),
    ("range", "full"),
];

impl PaintedPageVideoFrame<'_> {
    /// The message bytes: `surface_id`, `width`, `height`, `timestamp_ns`, `fps`
    /// and `color_info`.
    pub fn to_msgpack(&self) -> Vec<u8> {
        let mut record = Vec::with_capacity(192);
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
        rmp::encode::write_uint(&mut record, u64::from(self.frames_per_second))
            .expect(write_failure);
        write_text(&mut record, "color_info");
        rmp::encode::write_map_len(&mut record, 4).expect(write_failure);
        for (axis, name) in PAINTED_PAGE_COLOUR_AXES {
            write_text(&mut record, axis);
            write_text(&mut record, name);
        }
        record
    }
}

fn write_text(record: &mut Vec<u8>, text: &str) {
    rmp::encode::write_str(record, text).expect("writing msgpack into a Vec cannot fail");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_reads_back_as_the_map_video_frame_reads() {
        let frame = PaintedPageVideoFrame {
            surface_id: "slot-1#7",
            width: 1920,
            height: 1080,
            timestamp_ns: 42,
            frames_per_second: 60,
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
        assert_eq!(entry("surface_id").as_str(), Some("slot-1#7"));
        assert_eq!(entry("width").as_u64(), Some(1920));
        assert_eq!(entry("height").as_u64(), Some(1080));
        assert_eq!(entry("timestamp_ns").as_i64(), Some(42));
        assert_eq!(entry("fps").as_u64(), Some(60));
        let color_info = entry("color_info");
        let axes: Vec<(&str, &str)> = color_info
            .as_map()
            .expect("a colour map")
            .iter()
            .map(|(axis, name)| (axis.as_str().unwrap(), name.as_str().unwrap()))
            .collect();
        assert_eq!(axes, PAINTED_PAGE_COLOUR_AXES);
    }
}
