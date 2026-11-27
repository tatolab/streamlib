// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The built-in nodes a person or an agent places between two ports whose types differ, so
//! every conversion, and what it costs, is a node in the graph. Those named for the pin types or
//! the formats they convert between state `conversion`, so the link verdict finds them, and a
//! number conversion casts a saved value retyped between two numbers.

pub mod audio_to_tensor;
#[cfg(test)]
mod audio_to_tensor_tests;
mod conversion_node_config_defaults;
#[cfg(test)]
mod conversion_node_config_defaults_tests;
mod conversion_node_config_refusals;
#[cfg(test)]
mod conversion_node_config_refusals_tests;
mod messages_read_by_a_conversion;
pub mod number_conversions;
#[cfg(test)]
mod number_conversions_tests;
pub mod tensor_to_audio;
#[cfg(test)]
mod tensor_to_audio_tests;
pub mod tensor_to_texture;
#[cfg(test)]
mod tensor_to_texture_tests;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod texture_format_convert;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod texture_format_convert_tests;
pub mod texture_to_tensor;
#[cfg(test)]
mod texture_to_tensor_tests;

pub use audio_to_tensor::AudioToTensor;
pub use number_conversions::{
    BooleanToFloat, BooleanToInteger, FloatToBoolean, FloatToInteger, IntegerToBoolean,
    IntegerToFloat,
};
pub use tensor_to_audio::{TensorToAudio, TensorToAudioInputs};
pub use tensor_to_texture::TensorToTexture;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use texture_format_convert::TextureFormatConvert;
pub use texture_to_tensor::TextureToTensor;
