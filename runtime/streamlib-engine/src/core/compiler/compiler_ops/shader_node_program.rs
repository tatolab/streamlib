// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A shader node's program: the signature its describe states, and the compute
//! kernel the engine wraps around its Slang function for the backings its
//! Image inputs arrive in.

use std::fmt::Write as _;

use crate::core::rhi::{ComputeBindingDeclaration, ComputeBindingKind, TextureFormat};

/// The entry point of every shader node's compute kernel.
pub(crate) const SHADER_NODE_KERNEL_ENTRY_POINT: &str = "streamlib_shader_node_main";

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

/// The value one input of a shader node takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShaderNodeInputValueType {
    /// An Image, read in Slang as an `ImageInput`.
    ImageFrame,
    /// A float, read in Slang as a push constant.
    Float,
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

    fn binding_kind(self) -> ComputeBindingKind {
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
                let value_type =
                    match string_at(input, SHADER_NODE_SIGNATURE_INPUT_VALUE_TYPE_KEY)?.as_str() {
                        "image_frame" => ShaderNodeInputValueType::ImageFrame,
                        "float" => ShaderNodeInputValueType::Float,
                        other => {
                            return Err(format!(
                                "a shader input's value type is {other:?}; a shader takes \
                                 `image_frame` and `float` inputs"
                            ));
                        }
                    };
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
                "{} takes {} Images and {} floats, whose push constants are {push_constant_byte_len} \
                 bytes, past the {SHADER_NODE_PUSH_CONSTANT_BYTES_AT_MOST} every device takes; take \
                 fewer inputs",
                program.function_name,
                program.image_input_count(),
                program.float_input_ports().count()
            ));
        }
        Ok(program)
    }

    /// The ports of the Image inputs, in signature order.
    pub(crate) fn image_input_ports(&self) -> impl Iterator<Item = &str> {
        self.inputs_of(ShaderNodeInputValueType::ImageFrame)
    }

    /// The ports of the float inputs, in signature order.
    pub(crate) fn float_input_ports(&self) -> impl Iterator<Item = &str> {
        self.inputs_of(ShaderNodeInputValueType::Float)
    }

    fn inputs_of(&self, value_type: ShaderNodeInputValueType) -> impl Iterator<Item = &str> {
        self.inputs
            .iter()
            .filter(move |input| input.value_type == value_type)
            .map(|input| input.port.as_str())
    }

    fn image_input_count(&self) -> usize {
        self.image_input_ports().count()
    }

    /// The bytes of the kernel's push constants: the output extent, each
    /// Image input's extent and row pitch, then each float, four bytes apiece.
    pub(crate) fn push_constant_byte_len(&self) -> usize {
        4 * (2 + 3 * self.image_input_count() + self.float_input_ports().count())
    }

    /// The push constants of one evaluation, laid out as
    /// [`Self::push_constant_byte_len`] states.
    pub(crate) fn push_constants(
        &self,
        output_extent: (u32, u32),
        image_input_extents: &[ShaderNodeImageInputExtent],
        float_values: &[f32],
    ) -> Vec<u8> {
        let mut push_constants = Vec::with_capacity(self.push_constant_byte_len());
        push_constants.extend_from_slice(&output_extent.0.to_le_bytes());
        push_constants.extend_from_slice(&output_extent.1.to_le_bytes());
        for extent in image_input_extents {
            push_constants.extend_from_slice(&extent.width.to_le_bytes());
            push_constants.extend_from_slice(&extent.height.to_le_bytes());
            push_constants.extend_from_slice(&extent.row_pitch_in_pixels.to_le_bytes());
        }
        for float_value in float_values {
            push_constants.extend_from_slice(&float_value.to_le_bytes());
        }
        push_constants
    }

    /// The binding name of the Image input at `image_input_index`.
    pub(crate) fn image_input_binding_name(image_input_index: usize) -> String {
        format!("streamlib_shader_node_input_{image_input_index}")
    }

    /// Every binding the kernel for `image_input_backings` declares, by name.
    pub(crate) fn declared_bindings(
        image_input_backings: &[ShaderNodeImageInputBacking],
    ) -> Vec<ComputeBindingDeclaration> {
        std::iter::once(ComputeBindingDeclaration {
            name: SHADER_NODE_OUTPUT_BINDING_NAME.to_string(),
            kind: ComputeBindingKind::StorageBuffer,
        })
        .chain(
            image_input_backings
                .iter()
                .enumerate()
                .map(|(image_input_index, backing)| ComputeBindingDeclaration {
                    name: Self::image_input_binding_name(image_input_index),
                    kind: backing.binding_kind(),
                }),
        )
        .collect()
    }

    /// The Slang compute kernel that calls the node's function once per output
    /// pixel, its Image inputs bound as `image_input_backings` states. The
    /// node's source comes first, so a compiler diagnostic names its own lines.
    pub(crate) fn compute_kernel_slang_source(
        &self,
        image_input_backings: &[ShaderNodeImageInputBacking],
    ) -> String {
        let mut kernel = String::new();
        kernel.push_str(&self.slang_source);
        kernel.push_str(SHADER_NODE_IMAGE_INPUT_PRELUDE);

        kernel.push_str("\nstruct StreamlibShaderNodePushConstants\n{\n");
        kernel.push_str("    uint output_width;\n    uint output_height;\n");
        for image_input_index in 0..image_input_backings.len() {
            let _ = write!(
                kernel,
                "    uint input_{image_input_index}_width;\n    uint \
                 input_{image_input_index}_height;\n    uint \
                 input_{image_input_index}_row_pitch_in_pixels;\n"
            );
        }
        for float_index in 0..self.float_input_ports().count() {
            let _ = writeln!(kernel, "    float float_{float_index};");
        }
        kernel.push_str(
            "};\n[[vk::push_constant]] ConstantBuffer<StreamlibShaderNodePushConstants> \
             streamlib_shader_node_push_constants;\n\n",
        );

        let _ = writeln!(
            kernel,
            "[[vk::binding(0, 0)]] RWStructuredBuffer<uint> {SHADER_NODE_OUTPUT_BINDING_NAME};"
        );
        for (image_input_index, backing) in image_input_backings.iter().enumerate() {
            let binding_type = match backing {
                ShaderNodeImageInputBacking::SampledTexture => "Sampler2D",
                ShaderNodeImageInputBacking::Rgba8PixelBuffer => "StructuredBuffer<uint>",
                ShaderNodeImageInputBacking::Rgba16FloatPixelBuffer => "StructuredBuffer<uint2>",
                ShaderNodeImageInputBacking::Rgba32FloatPixelBuffer => "StructuredBuffer<float4>",
            };
            let _ = writeln!(
                kernel,
                "[[vk::binding({}, 0)]] {binding_type} {};",
                image_input_index + 1,
                Self::image_input_binding_name(image_input_index)
            );
        }

        for (image_input_index, backing) in image_input_backings.iter().enumerate() {
            kernel.push_str(&image_input_load_and_sample_functions(
                image_input_index,
                *backing,
            ));
        }

        kernel.push_str(
            "\nfloat4 streamlib_shader_node_sample_input(uint input_index, float2 uv)\n{\n    \
             switch (input_index)\n    {\n",
        );
        for image_input_index in 0..image_input_backings.len() {
            let _ = writeln!(
                kernel,
                "    case {image_input_index}: return \
                 streamlib_shader_node_sample_input_{image_input_index}(uv);"
            );
        }
        kernel.push_str("    default: return float4(0.0, 0.0, 0.0, 0.0);\n    }\n}\n");

        let _ = write!(
            kernel,
            "\n[numthreads({SHADER_NODE_KERNEL_WORKGROUP_TILE_SIZE}, \
             {SHADER_NODE_KERNEL_WORKGROUP_TILE_SIZE}, 1)]\nvoid \
             {SHADER_NODE_KERNEL_ENTRY_POINT}(uint3 invocation : SV_DispatchThreadID)\n{{\n    \
             uint2 output_extent = uint2(streamlib_shader_node_push_constants.output_width, \
             streamlib_shader_node_push_constants.output_height);\n    if (any(invocation.xy >= \
             output_extent))\n    {{\n        return;\n    }}\n    float2 uv = \
             (float2(invocation.xy) + 0.5) / float2(output_extent);\n"
        );
        let mut call_arguments = Vec::with_capacity(self.inputs.len());
        let (mut image_input_index, mut float_index) = (0usize, 0usize);
        for input in &self.inputs {
            match input.value_type {
                ShaderNodeInputValueType::ImageFrame => {
                    let _ = write!(
                        kernel,
                        "    ImageInput image_input_{image_input_index};\n    \
                         image_input_{image_input_index}.streamlib_shader_node_input_index = \
                         {image_input_index};\n    image_input_{image_input_index}.pixel = \
                         streamlib_shader_node_pixel_of_input_{image_input_index}(uv);\n"
                    );
                    call_arguments.push(format!("image_input_{image_input_index}"));
                    image_input_index += 1;
                }
                ShaderNodeInputValueType::Float => {
                    call_arguments.push(format!(
                        "streamlib_shader_node_push_constants.float_{float_index}"
                    ));
                    float_index += 1;
                }
            }
        }
        let _ = write!(
            kernel,
            "    float4 pixel = {}({});\n    {SHADER_NODE_OUTPUT_BINDING_NAME}[invocation.y * \
             output_extent.x + invocation.x] = streamlib_shader_node_pack_rgba8(pixel);\n}}\n",
            self.function_name,
            call_arguments.join(", ")
        );
        kernel
    }
}

/// What every shader node's kernel declares beside the node's own source:
/// `ImageInput`, and the packing of pixel-buffer channels.
const SHADER_NODE_IMAGE_INPUT_PRELUDE: &str = r#"

struct ImageInput
{
    float4 pixel;
    uint streamlib_shader_node_input_index;

    float4 sample(float2 uv)
    {
        return streamlib_shader_node_sample_input(streamlib_shader_node_input_index, uv);
    }
};

float4 streamlib_shader_node_unpack_rgba8(uint packed)
{
    return float4(packed & 0xFFu, (packed >> 8) & 0xFFu, (packed >> 16) & 0xFFu, packed >> 24) / 255.0;
}

float4 streamlib_shader_node_unpack_rgba16_float(uint2 packed)
{
    return float4(f16tof32(packed.x & 0xFFFFu), f16tof32(packed.x >> 16), f16tof32(packed.y & 0xFFFFu), f16tof32(packed.y >> 16));
}

uint streamlib_shader_node_pack_rgba8(float4 pixel)
{
    uint4 channel_bytes = uint4(round(saturate(pixel) * 255.0));
    return channel_bytes.x | (channel_bytes.y << 8) | (channel_bytes.z << 16) | (channel_bytes.w << 24);
}
"#;

/// The functions reading Image input `image_input_index` bound as `backing`:
/// its pixel nearest `uv`, and its bilinear sample at `uv`, clamped at the edge.
fn image_input_load_and_sample_functions(
    image_input_index: usize,
    backing: ShaderNodeImageInputBacking,
) -> String {
    let binding = ShaderNodeProgram::image_input_binding_name(image_input_index);
    let extent = format!(
        "uint2(streamlib_shader_node_push_constants.input_{image_input_index}_width, \
         streamlib_shader_node_push_constants.input_{image_input_index}_height)"
    );
    let row_pitch = format!(
        "streamlib_shader_node_push_constants.input_{image_input_index}_row_pitch_in_pixels"
    );
    let texel_decoded = match backing {
        ShaderNodeImageInputBacking::SampledTexture => {
            format!("{binding}.Load(int3(clamped_texel, 0))")
        }
        ShaderNodeImageInputBacking::Rgba8PixelBuffer => format!(
            "streamlib_shader_node_unpack_rgba8({binding}[clamped_texel.y * {row_pitch} + \
             clamped_texel.x])"
        ),
        ShaderNodeImageInputBacking::Rgba16FloatPixelBuffer => format!(
            "streamlib_shader_node_unpack_rgba16_float({binding}[clamped_texel.y * {row_pitch} + \
             clamped_texel.x])"
        ),
        ShaderNodeImageInputBacking::Rgba32FloatPixelBuffer => {
            format!("{binding}[clamped_texel.y * {row_pitch} + clamped_texel.x]")
        }
    };
    let sample = match backing {
        ShaderNodeImageInputBacking::SampledTexture => {
            format!("    return {binding}.SampleLevel(uv, 0.0);\n")
        }
        _ => format!(
            "    float2 at = uv * float2({extent}) - 0.5;\n    int2 base = int2(floor(at));\n    \
             float2 weight = at - float2(base);\n    float4 top = \
             lerp(streamlib_shader_node_load_input_{image_input_index}(base), \
             streamlib_shader_node_load_input_{image_input_index}(base + int2(1, 0)), \
             weight.x);\n    float4 bottom = \
             lerp(streamlib_shader_node_load_input_{image_input_index}(base + int2(0, 1)), \
             streamlib_shader_node_load_input_{image_input_index}(base + int2(1, 1)), \
             weight.x);\n    return lerp(top, bottom, weight.y);\n"
        ),
    };
    format!(
        "\nfloat4 streamlib_shader_node_load_input_{image_input_index}(int2 \
         texel)\n{{\n    uint2 extent = {extent};\n    uint2 clamped_texel = uint2(clamp(texel, \
         int2(0, 0), int2(extent) - 1));\n    return {texel_decoded};\n}}\n\nfloat4 \
         streamlib_shader_node_pixel_of_input_{image_input_index}(float2 uv)\n{{\n    return \
         streamlib_shader_node_load_input_{image_input_index}(int2(floor(uv * \
         float2({extent}))));\n}}\n\nfloat4 \
         streamlib_shader_node_sample_input_{image_input_index}(float2 uv)\n{{\n{sample}}}\n"
    )
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
        assert_eq!(program.float_input_ports().collect::<Vec<_>>(), ["amount"]);
        assert_eq!(program.output_port, "result");
    }

    #[test]
    fn the_push_constants_are_the_output_extent_each_image_extent_then_each_float() {
        let program = blend_program();
        let image_extent = ShaderNodeImageInputExtent {
            width: 64,
            height: 32,
            row_pitch_in_pixels: 80,
        };
        let push_constants =
            program.push_constants((64, 32), &[image_extent, image_extent], &[0.25]);

        assert_eq!(push_constants.len(), program.push_constant_byte_len());
        assert_eq!(push_constants.len(), 4 * (2 + 3 * 2 + 1));
        assert_eq!(&push_constants[8..12], &64u32.to_le_bytes());
        assert_eq!(&push_constants[16..20], &80u32.to_le_bytes());
        assert_eq!(&push_constants[32..36], &0.25f32.to_le_bytes());
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
    fn the_kernel_declares_the_output_then_each_image_input_as_its_backing_binds_it() {
        let declared = ShaderNodeProgram::declared_bindings(&[
            ShaderNodeImageInputBacking::Rgba8PixelBuffer,
            ShaderNodeImageInputBacking::SampledTexture,
        ]);
        assert_eq!(
            declared
                .iter()
                .map(|binding| (binding.name.as_str(), binding.kind))
                .collect::<Vec<_>>(),
            [
                (
                    SHADER_NODE_OUTPUT_BINDING_NAME,
                    ComputeBindingKind::StorageBuffer
                ),
                (
                    "streamlib_shader_node_input_0",
                    ComputeBindingKind::StorageBuffer
                ),
                (
                    "streamlib_shader_node_input_1",
                    ComputeBindingKind::SampledTexture
                ),
            ]
        );
    }

    #[test]
    fn the_kernel_starts_with_the_nodes_own_source_and_calls_its_function() {
        let program = blend_program();
        let kernel = program.compute_kernel_slang_source(&[
            ShaderNodeImageInputBacking::Rgba8PixelBuffer,
            ShaderNodeImageInputBacking::SampledTexture,
        ]);
        assert!(kernel.starts_with(&program.slang_source));
        assert!(
            kernel.contains(
                "blend(image_input_0, image_input_1, streamlib_shader_node_push_constants.float_0)"
            ),
            "{kernel}"
        );
        assert!(
            kernel.contains("Sampler2D streamlib_shader_node_input_1"),
            "{kernel}"
        );
        assert!(
            kernel.contains("StructuredBuffer<uint> streamlib_shader_node_input_0"),
            "{kernel}"
        );
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn the_kernel_compiles_for_every_backing_and_reflects_the_bindings_it_declares() {
        use crate::core::rhi::{
            ShaderCompilationTargetStage, SlangShaderSourceCompiler, derive_bindings_from_spirv,
        };

        let program = blend_program();
        let compiler = SlangShaderSourceCompiler::new();
        for backing in [
            ShaderNodeImageInputBacking::SampledTexture,
            ShaderNodeImageInputBacking::Rgba8PixelBuffer,
            ShaderNodeImageInputBacking::Rgba16FloatPixelBuffer,
            ShaderNodeImageInputBacking::Rgba32FloatPixelBuffer,
        ] {
            let backings = [backing, ShaderNodeImageInputBacking::Rgba8PixelBuffer];
            let spirv = compiler
                .compile_or_reuse(
                    &program.compute_kernel_slang_source(&backings),
                    ShaderCompilationTargetStage::Compute,
                    SHADER_NODE_KERNEL_ENTRY_POINT,
                    &program.function_name,
                )
                .unwrap_or_else(|refusal| panic!("{backing:?}: {refusal}"));
            let (reflected, push_constant_byte_len) = derive_bindings_from_spirv(&spirv).unwrap();
            crate::core::rhi::reconcile_compute_binding_declarations(
                &ShaderNodeProgram::declared_bindings(&backings),
                &reflected,
            )
            .unwrap_or_else(|refusal| panic!("{backing:?}: {refusal}"));
            assert_eq!(
                push_constant_byte_len as usize,
                program.push_constant_byte_len()
            );
        }
    }
}
