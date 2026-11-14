// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! A video decoder node's config keys.

/// The config keys `H264Decoder` and `H265Decoder` take: a cap on the extent
/// the decoded picture buffer is allocated for, absent to size it from the
/// stream's first SPS.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoDecoderSettings {
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
}

impl VideoDecoderSettings {
    /// The cap the decoder is minted with, `(0, 0)` meaning the first SPS
    /// sizes it. A half-specified pair caps nothing and warns.
    pub fn decoded_picture_buffer_extent_cap(&self, node_type_name: &str) -> (u32, u32) {
        match (self.max_width, self.max_height) {
            (Some(max_width), Some(max_height)) => (max_width, max_height),
            (None, None) => (0, 0),
            (max_width, max_height) => {
                tracing::warn!(
                    node_type_name,
                    ?max_width,
                    ?max_height,
                    "max_width and max_height cap the decoded picture buffer together or not at \
                     all; sizing it from the first SPS"
                );
                (0, 0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_pair_caps_the_decoded_picture_buffer() {
        let settings = VideoDecoderSettings {
            max_width: Some(1920),
            max_height: Some(1088),
        };
        assert_eq!(
            settings.decoded_picture_buffer_extent_cap("H264Decoder"),
            (1920, 1088)
        );
    }

    #[test]
    fn an_absent_or_half_pair_sizes_from_the_first_sps() {
        assert_eq!(
            VideoDecoderSettings::default().decoded_picture_buffer_extent_cap("H264Decoder"),
            (0, 0)
        );
        let half = VideoDecoderSettings {
            max_width: Some(1920),
            max_height: None,
        };
        assert_eq!(
            half.decoded_picture_buffer_extent_cap("H264Decoder"),
            (0, 0)
        );
    }
}
