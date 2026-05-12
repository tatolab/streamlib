// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated schema types. DO NOT EDIT.

pub mod com_streamlib_api_server_config;
pub mod com_streamlib_bgra_file_source_config;
pub mod com_streamlib_clap_effect_config;
pub mod com_streamlib_escalate_request;
pub mod com_streamlib_escalate_response;
pub mod com_streamlib_h265_decoder_config;
pub mod com_streamlib_h265_encoder_config;
pub mod com_streamlib_linux_mp4_writer_config;
pub mod com_streamlib_moq_publish_track_config;
pub mod com_streamlib_moq_subscribe_track_config;
pub mod com_streamlib_opus_decoder_config;
pub mod com_streamlib_opus_encoder_config;
pub mod com_streamlib_test_configured_processor_config;
pub mod com_streamlib_webrtc_whep_config;
pub mod com_streamlib_webrtc_whip_config;
pub mod com_tatolab_mp4_writer_config;
pub mod com_tatolab_screen_capture_config;
pub mod com_tatolab_simple_passthrough_config;
#[allow(non_snake_case)]
pub mod tatolab__core;

pub use com_streamlib_api_server_config::ApiServerConfig;
pub use com_streamlib_bgra_file_source_config::BgraFileSourceConfig;
pub use com_streamlib_clap_effect_config::EffectConfig;
pub use com_streamlib_escalate_request::EscalateRequest;
pub use com_streamlib_escalate_response::EscalateResponse;
pub use com_streamlib_h265_decoder_config::H265DecoderConfig;
pub use com_streamlib_h265_encoder_config::H265EncoderConfig;
pub use com_streamlib_linux_mp4_writer_config::LinuxMp4WriterConfig;
pub use com_streamlib_moq_publish_track_config::MoqPublishTrackConfig;
pub use com_streamlib_moq_subscribe_track_config::MoqSubscribeTrackConfig;
pub use com_streamlib_opus_decoder_config::OpusDecoderConfig;
pub use com_streamlib_opus_encoder_config::OpusEncoderConfig;
pub use com_streamlib_test_configured_processor_config::ConfiguredProcessorConfig;
pub use com_streamlib_webrtc_whep_config::WebrtcWhepConfig;
pub use com_streamlib_webrtc_whip_config::WebrtcWhipConfig;
pub use com_tatolab_mp4_writer_config::Mp4WriterConfig;
pub use com_tatolab_screen_capture_config::ScreenCaptureConfig;
pub use com_tatolab_simple_passthrough_config::SimplePassthroughConfig;
pub use tatolab__core::AudioFrame;
pub use tatolab__core::EncodedAudioFrame;
pub use tatolab__core::EncodedVideoFrame;
pub use tatolab__core::VideoFrame;
