// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A video encoder node's config keys, and the session shape they resolve to
//! against the first frame.

/// The rate a session is minted at when the config states none.
pub const DEFAULT_ENCODE_FPS: u32 = 60;
/// Seconds between IDR sync points when the config states none.
pub const DEFAULT_KEYFRAME_INTERVAL_SECONDS: u32 = 2;

/// The config keys `H264Encoder` and `H265Encoder` take, each absent unless set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoEncoderSettings {
    /// Expected frame width, a guardrail: a mismatching frame wins.
    pub width: Option<u32>,
    /// Expected frame height, a guardrail like `width`.
    pub height: Option<u32>,
    /// The rate sessions are minted at.
    pub fps: Option<u32>,
    /// Target bitrate; absent is constant-QP at the balanced preset.
    pub bitrate_bps: Option<u32>,
    pub keyframe_interval_seconds: Option<u32>,
    /// The hardware encoder's analysis budget index.
    pub effort_level: Option<u32>,
}

/// The session one frame mints: the frame's extent and the rate it resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoEncodeSessionShape {
    pub width: u32,
    pub height: u32,
    pub frames_per_second: u32,
    pub keyframe_interval_seconds: u32,
}

impl VideoEncoderSettings {
    /// Resolve the session a `frame_width` x `frame_height` frame mints: the
    /// frame's extent wins over the guardrails, with a warning, and the rate
    /// is the config's, or the default.
    pub fn session_shape_for_frame(
        &self,
        node_type_name: &str,
        frame_width: u32,
        frame_height: u32,
    ) -> VideoEncodeSessionShape {
        if let Some(config_width) = self.width
            && config_width != frame_width
        {
            tracing::warn!(
                node_type_name,
                config_width,
                frame_width,
                "config width does not match the incoming frame; using the frame's"
            );
        }
        if let Some(config_height) = self.height
            && config_height != frame_height
        {
            tracing::warn!(
                node_type_name,
                config_height,
                frame_height,
                "config height does not match the incoming frame; using the frame's"
            );
        }
        VideoEncodeSessionShape {
            width: frame_width,
            height: frame_height,
            frames_per_second: self.fps.unwrap_or(DEFAULT_ENCODE_FPS),
            keyframe_interval_seconds: self
                .keyframe_interval_seconds
                .unwrap_or(DEFAULT_KEYFRAME_INTERVAL_SECONDS),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_extent_wins_over_the_guardrails() {
        let settings = VideoEncoderSettings {
            width: Some(1920),
            height: Some(1080),
            ..Default::default()
        };
        let shape = settings.session_shape_for_frame("H264Encoder", 1280, 720);
        assert_eq!((shape.width, shape.height), (1280, 720));
    }

    #[test]
    fn the_rate_is_the_configs_or_the_default() {
        let configured = VideoEncoderSettings {
            fps: Some(24),
            ..Default::default()
        };
        assert_eq!(
            configured
                .session_shape_for_frame("H264Encoder", 2, 2)
                .frames_per_second,
            24
        );
        assert_eq!(
            VideoEncoderSettings::default()
                .session_shape_for_frame("H264Encoder", 2, 2)
                .frames_per_second,
            DEFAULT_ENCODE_FPS
        );
    }

    #[test]
    fn the_keyframe_interval_defaults_to_two_seconds() {
        let shape = VideoEncoderSettings::default().session_shape_for_frame("H264Encoder", 2, 2);
        assert_eq!(
            shape.keyframe_interval_seconds,
            DEFAULT_KEYFRAME_INTERVAL_SECONDS
        );
    }
}
