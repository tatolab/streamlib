// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/audio` — audio processors carved out of the streamlib engine.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

// Audio DSP utilities reach-for by processors that need conversion / resampling /
// rechunking. Engine no longer hosts these; consumers (this package, packages/clap)
// instantiate `ProcessorAudioConverter` directly as a struct field.
pub mod audio_resample;
pub mod audio_utils;
pub mod processor_audio_converter;

// Cross-platform processors
pub mod audio_channel_converter;
pub mod audio_mixer;
pub mod audio_resampler;
pub mod buffer_rechunker;
pub mod chord_generator;

// Cross-platform shims that re-export the per-platform impl under a unified name.
pub mod audio_capture;
pub mod audio_output;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod apple;

// `_apple_impl_pending_` holds the cross-platform Apple-flavored audio
// codec types parked out of the engine in #786. Gated so it never
// compiles; re-enable + rewire imports once Apple support is activated.
#[cfg(any())]
mod _apple_impl_pending_;

pub use audio_capture::{AudioCaptureProcessor, AudioInputDevice};
pub use audio_channel_converter::AudioChannelConverterProcessor;
pub use audio_mixer::AudioMixerProcessor;
pub use audio_output::{AudioDevice, AudioOutputProcessor};
pub use audio_resample::{AudioResampler, ResamplingQuality, StereoResampler};
pub use audio_resampler::AudioResamplerProcessor;
pub use audio_utils::{convert_audio_frame, convert_channels, resample_frame, AudioRechunker};
pub use buffer_rechunker::BufferRechunkerProcessor;
pub use chord_generator::ChordGeneratorProcessor;
pub use processor_audio_converter::{
    ProcessorAudioConverter, ProcessorAudioConverterStatus, ProcessorAudioConverterTargetFormat,
};

#[cfg(all(
    feature = "plugin",
    any(target_os = "linux", target_os = "macos", target_os = "ios")
))]
streamlib_plugin_abi::export_plugin!(
    crate::AudioChannelConverterProcessor::Processor,
    crate::AudioMixerProcessor::Processor,
    crate::AudioResamplerProcessor::Processor,
    crate::BufferRechunkerProcessor::Processor,
    crate::ChordGeneratorProcessor::Processor,
    crate::AudioCaptureProcessor::Processor,
    crate::AudioOutputProcessor::Processor,
);
