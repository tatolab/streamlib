// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated crate root — the mechanical projection of this package's
//! `processors/` directory. Do not edit: it is rewritten before every
//! cargo invocation and is excluded from the packed `.slpkg`.

#[allow(non_snake_case, unused_imports, dead_code, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[cfg(any())]
#[path = "../processors/_apple_impl_pending_/mod.rs"]
pub mod _apple_impl_pending_;
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[path = "../processors/audio_capture_apple.rs"]
pub mod audio_capture_apple;
#[cfg(target_os = "linux")]
#[path = "../processors/audio_capture_linux.rs"]
pub mod audio_capture_linux;
#[path = "../processors/audio_channel_converter.rs"]
pub mod audio_channel_converter;
#[path = "../processors/audio_mixer.rs"]
pub mod audio_mixer;
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[path = "../processors/audio_output_apple.rs"]
pub mod audio_output_apple;
#[cfg(target_os = "linux")]
#[path = "../processors/audio_output_linux.rs"]
pub mod audio_output_linux;
#[path = "../processors/audio_resample.rs"]
pub mod audio_resample;
#[path = "../processors/audio_resampler.rs"]
pub mod audio_resampler;
#[path = "../processors/audio_utils.rs"]
pub mod audio_utils;
#[path = "../processors/buffer_rechunker.rs"]
pub mod buffer_rechunker;
#[path = "../processors/chord_generator.rs"]
pub mod chord_generator;
#[path = "../processors/processor_audio_converter.rs"]
pub mod processor_audio_converter;

streamlib_plugin_abi::export_plugin!(
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    crate::audio_capture_apple::AppleAudioCaptureProcessor::Processor,
    #[cfg(target_os = "linux")]
    crate::audio_capture_linux::LinuxAudioCaptureProcessor::Processor,
    crate::audio_channel_converter::AudioChannelConverterProcessor::Processor,
    crate::audio_mixer::AudioMixerProcessor::Processor,
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    crate::audio_output_apple::AppleAudioOutputProcessor::Processor,
    #[cfg(target_os = "linux")]
    crate::audio_output_linux::LinuxAudioOutputProcessor::Processor,
    crate::audio_resampler::AudioResamplerProcessor::Processor,
    crate::buffer_rechunker::BufferRechunkerProcessor::Processor,
    crate::chord_generator::ChordGeneratorProcessor::Processor,
);
