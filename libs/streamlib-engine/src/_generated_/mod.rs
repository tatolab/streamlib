// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated schema types. DO NOT EDIT.

pub mod com_streamlib_bgra_file_source_config;
pub mod com_streamlib_escalate_request;
pub mod com_streamlib_escalate_response;
pub mod com_streamlib_test_configured_processor_config;
pub mod com_tatolab_screen_capture_config;
pub mod com_tatolab_simple_passthrough_config;
#[allow(non_snake_case)]
pub mod tatolab__core;

pub use com_streamlib_bgra_file_source_config::BgraFileSourceConfig;
pub use com_streamlib_escalate_request::EscalateRequest;
pub use com_streamlib_escalate_response::EscalateResponse;
pub use com_streamlib_test_configured_processor_config::ConfiguredProcessorConfig;
pub use com_tatolab_screen_capture_config::ScreenCaptureConfig;
pub use com_tatolab_simple_passthrough_config::SimplePassthroughConfig;
pub use tatolab__core::AudioFrame;
pub use tatolab__core::EncodedAudioFrame;
pub use tatolab__core::EncodedVideoFrame;
pub use tatolab__core::VideoFrame;
