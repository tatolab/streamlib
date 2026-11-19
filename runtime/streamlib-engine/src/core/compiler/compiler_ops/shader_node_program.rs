// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A shader node's program: its Slang function and the signature its describe
//! states.

use crate::core::descriptors::PinType;
use crate::core::graph::input_values::the_control_value_of_a_pin;
use crate::core::pin_value_packing::{
    channel_count_of, the_channel_values_a_pin_type_reads, the_channel_values_of_a_stored_value,
};
use crate::core::rhi::{ComputeBindingKind, TextureFormat};

/// The side of the square workgroup each shader node's kernel runs in.
pub(crate) const SHADER_NODE_KERNEL_WORKGROUP_TILE_SIZE: u32 = 8;

/// The most push-constant bytes a shader node's kernel takes, the floor every
/// Vulkan device guarantees.
pub(crate) const SHADER_NODE_PUSH_CONSTANT_BYTES_AT_MOST: usize = 128;

/// The texture format every shader node's output Image states.
pub(crate) const SHADER_NODE_OUTPUT_TEXTURE_FORMAT: TextureFormat = TextureFormat::Rgba8Unorm;

/// The binding the kernel writes the output Image's pixel buffer through.
pub(crate) const SHADER_NODE_OUTPUT_BINDING_NAME: &str = "streamlib_shader_node_output";

const SHADER_NODE_SIGNATURE_FUNCTION_NAME_KEY: &str = "function_name";
const SHADER_NODE_SIGNATURE_INPUTS_KEY: &str = "inputs";
const SHADER_NODE_SIGNATURE_OUTPUT_PORT_KEY: &str = "output_port";
const SHADER_NODE_SIGNATURE_INPUT_PORT_KEY: &str = "port";
const SHADER_NODE_SIGNATURE_INPUT_VALUE_TYPE_KEY: &str = "value_type";
const SHADER_NODE_SIGNATURE_INPUT_PIN_TYPE_KEY: &str = "pin_type";

/// The value one input of a shader node takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShaderNodeInputValueType {
    /// An Image, read in Slang as an `ImageInput`.
    ImageFrame,
    /// A value of numbers of this pin type, a Float, a Color or a Vector, read in Slang as a
    /// `float` or a `float2`, `float3` or `float4` of its parts, carried as push constants.
    Numbers(PinType),
}

impl ShaderNodeInputValueType {
    /// The numbers a value of this type carries as push constants: none for an Image.
    pub(crate) fn number_count(&self) -> usize {
        match self {
            Self::ImageFrame => 0,
            Self::Numbers(pin_type) => channel_count_of(pin_type).unwrap_or_default(),
        }
    }

    /// The Slang type the shader's function takes a value of numbers as.
    pub(crate) fn slang_type_of_its_numbers(&self) -> &'static str {
        match self.number_count() {
            2 => "float2",
            3 => "float3",
            4 => "float4",
            _ => "float",
        }
    }

    /// The numbers `value`, a stored value of this type, carries as push constants; a value
    /// absent, or one this type does not hold, reads as the number 0 does: a Float of 0, a
    /// Vector of zeros, an opaque black Color. A Float takes a Boolean as 1 or 0.
    pub(crate) fn numbers_a_stored_value_holds(
        &self,
        value: Option<&serde_json::Value>,
    ) -> Vec<f32> {
        let Self::Numbers(pin_type) = self else {
            return Vec::new();
        };
        value
            .and_then(|value| match pin_type {
                PinType::Float { .. } => {
                    the_control_value_of_a_pin(value).map(|number| vec![number as f32])
                }
                _ => the_channel_values_of_a_stored_value(pin_type, value)
                    .ok()
                    .map(|numbers| numbers.into_iter().map(|number| number as f32).collect()),
            })
            .or_else(|| self.numbers_a_control_message_carries(&[0.0]))
            .unwrap_or_default()
    }

    /// The numbers a control message whose channels hold `channel_values` carries as push
    /// constants, read with the free conversion a link allows: one number as every part of a
    /// Vector, or as the red, green and blue of an opaque Color. None for an Image, or a message
    /// of no channel.
    pub(crate) fn numbers_a_control_message_carries(
        &self,
        channel_values: &[f64],
    ) -> Option<Vec<f32>> {
        let Self::Numbers(pin_type) = self else {
            return None;
        };
        the_channel_values_a_pin_type_reads(pin_type, channel_values)
            .ok()
            .map(|numbers| numbers.into_iter().map(|number| number as f32).collect())
    }

    /// The type a describe states for an input as `value_type`, with its `pin_type` when it
    /// states one: an Image, or a Float, a Color or a Vector. A describe of an older SDK states
    /// a Float by its `value_type` alone.
    fn read_from_the_signature(
        value_type: &str,
        pin_type: Option<&serde_json::Value>,
    ) -> std::result::Result<Self, String> {
        if value_type == "image_frame" {
            return Ok(Self::ImageFrame);
        }
        let pin_type = match pin_type {
            Some(pin_type) => {
                serde_json::from_value::<PinType>(pin_type.clone()).map_err(|not_a_pin_type| {
                    format!("a shader input's pin type does not read: {not_a_pin_type}")
                })?
            }
            None if value_type == "float" => PinType::Float { exact_size: None },
            None => {
                return Err(format!(
                    "a shader input's value type is {value_type:?} and states no pin type"
                ));
            }
        };
        match pin_type {
            PinType::Float { .. } | PinType::Color {} | PinType::Vector { .. } => {
                Ok(Self::Numbers(pin_type))
            }
            other => Err(format!(
                "a shader input is a {other}; a shader takes `image_frame` inputs and Float, \
                 Color and Vector values"
            )),
        }
    }
}

/// One input of a shader node's signature, in the order its Slang function
/// takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShaderNodeSignatureInput {
    pub(crate) port: String,
    pub(crate) value_type: ShaderNodeInputValueType,
}

/// What an Image input of a shader node is bound as for one evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ShaderNodeImageInputBacking {
    /// A registered texture, read through a combined sampler.
    SampledTexture,
    /// A pixel buffer of four 8-bit channels, read as a storage buffer.
    Rgba8PixelBuffer,
    /// A pixel buffer of four half floats, read as a storage buffer.
    Rgba16FloatPixelBuffer,
    /// A pixel buffer of four floats, read as a storage buffer.
    Rgba32FloatPixelBuffer,
}

impl ShaderNodeImageInputBacking {
    /// The backing a pixel buffer of `format` is read through; `None` for a
    /// format that is not RGBA.
    pub(crate) fn of_a_pixel_buffer_of(format: TextureFormat) -> Option<Self> {
        match format {
            TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb => {
                Some(Self::Rgba8PixelBuffer)
            }
            TextureFormat::Rgba16Float => Some(Self::Rgba16FloatPixelBuffer),
            TextureFormat::Rgba32Float => Some(Self::Rgba32FloatPixelBuffer),
            TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb | TextureFormat::Nv12 => None,
        }
    }

    pub(crate) fn binding_kind(self) -> ComputeBindingKind {
        match self {
            Self::SampledTexture => ComputeBindingKind::SampledTexture,
            Self::Rgba8PixelBuffer
            | Self::Rgba16FloatPixelBuffer
            | Self::Rgba32FloatPixelBuffer => ComputeBindingKind::StorageBuffer,
        }
    }
}

/// Whether a shader node takes an Image of `format`: one of four channels in
/// RGBA order.
pub(crate) fn a_shader_node_takes_an_image_of(format: TextureFormat) -> bool {
    ShaderNodeImageInputBacking::of_a_pixel_buffer_of(format).is_some()
}

/// The extent and row pitch of one Image input, as its kernel reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShaderNodeImageInputExtent {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) row_pitch_in_pixels: u32,
}

/// A shader node's Slang function and the signature it is called with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaderNodeProgram {
    pub(crate) function_name: String,
    pub(crate) slang_source: String,
    pub(crate) inputs: Vec<ShaderNodeSignatureInput>,
    pub(crate) output_port: String,
}

impl ShaderNodeProgram {
    /// Read the program a describe states as `shader_source` and
    /// `shader_signature`, refusing one whose push constants pass
    /// [`SHADER_NODE_PUSH_CONSTANT_BYTES_AT_MOST`].
    pub(crate) fn read_from_described_shader(
        shader_source: &serde_json::Value,
        shader_signature: &serde_json::Value,
    ) -> std::result::Result<Self, String> {
        let slang_source = shader_source
            .as_str()
            .filter(|source| !source.trim().is_empty())
            .ok_or_else(|| "shader_source must be a non-blank string".to_string())?
            .to_string();
        let string_at = |value: &serde_json::Value, key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("shader_signature.{key} must be a string"))
        };
        let function_name = string_at(shader_signature, SHADER_NODE_SIGNATURE_FUNCTION_NAME_KEY)?;
        let output_port = string_at(shader_signature, SHADER_NODE_SIGNATURE_OUTPUT_PORT_KEY)?;
        let inputs = shader_signature
            .get(SHADER_NODE_SIGNATURE_INPUTS_KEY)
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                format!("shader_signature.{SHADER_NODE_SIGNATURE_INPUTS_KEY} must be a list")
            })?
            .iter()
            .map(|input| {
                let value_type = ShaderNodeInputValueType::read_from_the_signature(
                    &string_at(input, SHADER_NODE_SIGNATURE_INPUT_VALUE_TYPE_KEY)?,
                    input.get(SHADER_NODE_SIGNATURE_INPUT_PIN_TYPE_KEY),
                )?;
                Ok(ShaderNodeSignatureInput {
                    port: string_at(input, SHADER_NODE_SIGNATURE_INPUT_PORT_KEY)?,
                    value_type,
                })
            })
            .collect::<std::result::Result<Vec<_>, String>>()?;
        let program = Self {
            function_name,
            slang_source,
            inputs,
            output_port,
        };
        if program.image_input_count() == 0 {
            return Err(format!(
                "{} takes no Image, so nothing sets the extent of the Image it returns",
                program.function_name
            ));
        }
        let push_constant_byte_len = program.push_constant_byte_len();
        if push_constant_byte_len > SHADER_NODE_PUSH_CONSTANT_BYTES_AT_MOST {
            return Err(format!(
                "{} takes {} Images and {} numbers, whose push constants are \
                 {push_constant_byte_len} bytes, past the {SHADER_NODE_PUSH_CONSTANT_BYTES_AT_MOST} \
                 every device takes; take fewer inputs",
                program.function_name,
                program.image_input_count(),
                program.number_count()
            ));
        }
        Ok(program)
    }

    /// The ports of the Image inputs, in signature order.
    pub(crate) fn image_input_ports(&self) -> impl Iterator<Item = &str> {
        self.inputs
            .iter()
            .filter(|input| input.value_type == ShaderNodeInputValueType::ImageFrame)
            .map(|input| input.port.as_str())
    }

    /// The port and pin type of each input taking a value of numbers, in signature order.
    pub(crate) fn number_inputs(&self) -> impl Iterator<Item = (&str, &PinType)> {
        self.inputs
            .iter()
            .filter_map(|input| match &input.value_type {
                ShaderNodeInputValueType::Numbers(pin_type) => {
                    Some((input.port.as_str(), pin_type))
                }
                ShaderNodeInputValueType::ImageFrame => None,
            })
    }

    fn image_input_count(&self) -> usize {
        self.image_input_ports().count()
    }

    /// The numbers every value input carries together, one push constant apiece.
    fn number_count(&self) -> usize {
        self.inputs
            .iter()
            .map(|input| input.value_type.number_count())
            .sum()
    }

    /// The bytes of the kernel's push constants: the output extent, each
    /// Image input's extent and row pitch, then each number of each value,
    /// four bytes apiece.
    pub(crate) fn push_constant_byte_len(&self) -> usize {
        4 * (2 + 3 * self.image_input_count() + self.number_count())
    }

    /// Whether the node reads an Image beyond its own pixel, through `sample(uv)`.
    pub(crate) fn samples_beyond_its_own_pixel(&self) -> bool {
        self.slang_source.contains(".sample(")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blend_program() -> ShaderNodeProgram {
        ShaderNodeProgram::read_from_described_shader(
            &serde_json::json!(
                "float4 blend(ImageInput a, ImageInput b, float amount)\n{\n    return lerp(a.pixel, b.sample(float2(0.5, 0.5)), amount);\n}\n"
            ),
            &serde_json::json!({
                "function_name": "blend",
                "inputs": [
                    {"port": "a", "value_type": "image_frame"},
                    {"port": "b", "value_type": "image_frame"},
                    {"port": "amount", "value_type": "float"},
                ],
                "output_port": "result",
            }),
        )
        .expect("the blend signature is a shader's")
    }

    #[test]
    fn the_signature_reads_into_its_image_and_float_inputs_in_order() {
        let program = blend_program();
        assert_eq!(program.image_input_ports().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(
            program.number_inputs().collect::<Vec<_>>(),
            [("amount", &PinType::Float { exact_size: None })]
        );
        assert_eq!(program.output_port, "result");
    }

    #[test]
    fn a_shader_whose_push_constants_pass_128_bytes_is_refused_naming_its_inputs() {
        let inputs: Vec<_> = (0..11)
            .map(|index| serde_json::json!({"port": format!("i{index}"), "value_type": "image_frame"}))
            .collect();
        let refusal = ShaderNodeProgram::read_from_described_shader(
            &serde_json::json!("float4 wide(ImageInput i0) { return i0.pixel; }"),
            &serde_json::json!({"function_name": "wide", "inputs": inputs, "output_port": "result"}),
        )
        .unwrap_err();
        assert!(refusal.contains("11 Images"), "{refusal}");
        assert!(refusal.contains("128"), "{refusal}");
    }

    #[test]
    fn a_shader_with_no_image_input_is_refused() {
        let refusal = ShaderNodeProgram::read_from_described_shader(
            &serde_json::json!("float4 flat(float level) { return float4(level); }"),
            &serde_json::json!({
                "function_name": "flat",
                "inputs": [{"port": "level", "value_type": "float"}],
                "output_port": "result",
            }),
        )
        .unwrap_err();
        assert!(refusal.contains("takes no Image"), "{refusal}");
    }

    #[test]
    fn only_rgba_images_are_taken() {
        for format in [
            TextureFormat::Rgba8Unorm,
            TextureFormat::Rgba8UnormSrgb,
            TextureFormat::Rgba16Float,
            TextureFormat::Rgba32Float,
        ] {
            assert!(a_shader_node_takes_an_image_of(format), "{format:?}");
        }
        for format in [
            TextureFormat::Bgra8Unorm,
            TextureFormat::Bgra8UnormSrgb,
            TextureFormat::Nv12,
        ] {
            assert!(!a_shader_node_takes_an_image_of(format), "{format:?}");
        }
    }

    #[test]
    fn a_shader_calling_sample_reads_beyond_its_own_pixel() {
        assert!(blend_program().samples_beyond_its_own_pixel());
    }
}
