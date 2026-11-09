// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A shader node island's program: the shader nodes one evaluator runs, the
//! kernels the split rule cuts them into, and the Slang of each kernel.

use std::collections::HashSet;
use std::fmt::Write as _;

use super::shader_node_program::{
    SHADER_NODE_KERNEL_WORKGROUP_TILE_SIZE, SHADER_NODE_OUTPUT_BINDING_NAME,
    SHADER_NODE_PUSH_CONSTANT_BYTES_AT_MOST, ShaderNodeImageInputBacking,
    ShaderNodeImageInputExtent, ShaderNodeInputValueType, ShaderNodeProgram,
};
use crate::core::rhi::{ComputeBindingDeclaration, ComputeBindingKind};

/// Where one input of an island member takes its value from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ShaderNodeIslandMemberInput {
    /// The Image the island member at this index computes.
    ImageFromTheMember(usize),
    /// The Image frames arriving at this input port of the evaluator.
    ImageReadAt(String),
    /// The values arriving at this input port of the evaluator, this one until
    /// the first arrives.
    FloatReadAt {
        input_port: String,
        value_until_one_is_read: f32,
    },
}

/// One shader node of an island: its program, and where each of its inputs,
/// in signature order, takes its value from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ShaderNodeIslandMember {
    pub(crate) node_name: String,
    pub(crate) program: ShaderNodeProgram,
    pub(crate) inputs: Vec<ShaderNodeIslandMemberInput>,
}

/// The shader nodes one evaluator runs, in evaluation order: each member is
/// read by exactly one later member, and the last writes the island's output.
#[derive(Debug, Clone, PartialEq)]
pub struct ShaderNodeIslandProgram {
    members: Vec<ShaderNodeIslandMember>,
}

/// Where one Image argument of a kernel is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShaderNodeKernelImageArgument {
    /// The newest frame at this input port of the evaluator.
    ReadAt(String),
    /// The Image an earlier kernel of the same evaluation stored for the
    /// member at this index.
    StoredOutputOf(usize),
}

/// One kernel of an island's evaluation: the members it fuses, in evaluation
/// order, the last writing its output, and the arguments it binds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShaderNodeIslandKernel {
    pub(crate) members: Vec<usize>,
    pub(crate) image_arguments: Vec<ShaderNodeKernelImageArgument>,
    float_arguments: Vec<ShaderNodeKernelFloatArgument>,
}

/// One float argument of a kernel: the float input at `float_index`, in
/// signature order among the floats, of the member at `member`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShaderNodeKernelFloatArgument {
    pub(crate) member: usize,
    pub(crate) float_index: usize,
}

impl ShaderNodeIslandProgram {
    /// The island of one shader node, every input read at the port of its own
    /// name, each float starting at `float_value_until_one_is_read`.
    pub(crate) fn of_one_node(
        node_name: String,
        program: ShaderNodeProgram,
        float_value_until_one_is_read: impl Fn(&str) -> f32,
    ) -> Self {
        let inputs = program
            .inputs
            .iter()
            .map(|input| match input.value_type {
                ShaderNodeInputValueType::ImageFrame => {
                    ShaderNodeIslandMemberInput::ImageReadAt(input.port.clone())
                }
                ShaderNodeInputValueType::Float => ShaderNodeIslandMemberInput::FloatReadAt {
                    input_port: input.port.clone(),
                    value_until_one_is_read: float_value_until_one_is_read(&input.port),
                },
            })
            .collect();
        Self {
            members: vec![ShaderNodeIslandMember {
                node_name,
                program,
                inputs,
            }],
        }
    }

    /// The island of `members` in evaluation order, refusing one where a
    /// member reads a later one, an input's value type differs from its
    /// signature's, or a member other than the last is not read exactly once.
    pub(crate) fn of_the_members(
        members: Vec<ShaderNodeIslandMember>,
    ) -> std::result::Result<Self, String> {
        if members.is_empty() {
            return Err("a shader node island has at least one member".into());
        }
        let mut times_each_member_is_read = vec![0usize; members.len()];
        for (member_index, member) in members.iter().enumerate() {
            if member.inputs.len() != member.program.inputs.len() {
                return Err(format!(
                    "shader node `{}` takes {} inputs, and its island names {}",
                    member.node_name,
                    member.program.inputs.len(),
                    member.inputs.len()
                ));
            }
            for (input, signature_input) in member.inputs.iter().zip(&member.program.inputs) {
                let input_is_an_image =
                    !matches!(input, ShaderNodeIslandMemberInput::FloatReadAt { .. });
                if input_is_an_image
                    != (signature_input.value_type == ShaderNodeInputValueType::ImageFrame)
                {
                    return Err(format!(
                        "shader node `{}`: its island reads input `{}` as the wrong value type",
                        member.node_name, signature_input.port
                    ));
                }
                if let ShaderNodeIslandMemberInput::ImageFromTheMember(read_member) = input {
                    if *read_member >= member_index {
                        return Err(format!(
                            "shader node `{}` reads a member evaluated after it",
                            member.node_name
                        ));
                    }
                    times_each_member_is_read[*read_member] += 1;
                }
            }
        }
        let output_member_index = members.len() - 1;
        if let Some(member_index) =
            times_each_member_is_read
                .iter()
                .enumerate()
                .position(|(member_index, &times_read)| {
                    (member_index == output_member_index) != (times_read == 0) || times_read > 1
                })
        {
            return Err(format!(
                "shader node `{}` is read {} times inside its island; every member but the \
                 island's output is read exactly once",
                members[member_index].node_name, times_each_member_is_read[member_index]
            ));
        }
        Ok(Self { members })
    }

    /// The members in evaluation order.
    pub(crate) fn members(&self) -> &[ShaderNodeIslandMember] {
        &self.members
    }

    /// The member that writes the island's output.
    pub(crate) fn output_member(&self) -> &ShaderNodeIslandMember {
        &self.members[self.members.len() - 1]
    }

    /// The input port whose new frame starts an evaluation: reached from the
    /// output member through each member's first Image input.
    pub(crate) fn the_input_port_each_evaluation_waits_on(&self) -> &str {
        let mut member_index = self.members.len() - 1;
        loop {
            match self.first_image_input_of(member_index) {
                ShaderNodeIslandMemberInput::ImageFromTheMember(read_member) => {
                    member_index = *read_member;
                }
                ShaderNodeIslandMemberInput::ImageReadAt(input_port) => return input_port,
                ShaderNodeIslandMemberInput::FloatReadAt { .. } => {
                    unreachable!("a member's first Image input is an Image")
                }
            }
        }
    }

    fn first_image_input_of(&self, member_index: usize) -> &ShaderNodeIslandMemberInput {
        let member = &self.members[member_index];
        member
            .inputs
            .iter()
            .find(|input| !matches!(input, ShaderNodeIslandMemberInput::FloatReadAt { .. }))
            .unwrap_or_else(|| unreachable!("a shader node takes at least one Image"))
    }

    /// The extent of the Image each member computes: that of its first Image
    /// input, where `extent_read_at` gives the extent of a frame at an input port.
    pub(crate) fn output_extent_of_each_member(
        &self,
        extent_read_at: impl Fn(&str) -> (u32, u32),
    ) -> Vec<(u32, u32)> {
        let mut output_extents: Vec<(u32, u32)> = Vec::with_capacity(self.members.len());
        for member_index in 0..self.members.len() {
            let extent = match self.first_image_input_of(member_index) {
                ShaderNodeIslandMemberInput::ImageFromTheMember(read_member) => {
                    output_extents[*read_member]
                }
                ShaderNodeIslandMemberInput::ImageReadAt(input_port) => extent_read_at(input_port),
                ShaderNodeIslandMemberInput::FloatReadAt { .. } => unreachable!(),
            };
            output_extents.push(extent);
        }
        output_extents
    }

    /// The kernels one evaluation runs, in order. A member is fused into the
    /// kernel of the member reading it unless that member samples beyond its
    /// own pixel, the two extents differ, or the fused push constants would
    /// pass [`SHADER_NODE_PUSH_CONSTANT_BYTES_AT_MOST`]; a member not fused
    /// has its Image stored for the kernel reading it.
    pub(crate) fn kernels_under_the_split_rule(
        &self,
        output_extent_of_each_member: &[(u32, u32)],
    ) -> Vec<ShaderNodeIslandKernel> {
        let mut kernel_output_member_of_each_member: Vec<usize> = (0..self.members.len()).collect();
        for (reading_member, member) in self.members.iter().enumerate() {
            for input in &member.inputs {
                let ShaderNodeIslandMemberInput::ImageFromTheMember(read_member) = *input else {
                    continue;
                };
                if member.program.samples_beyond_its_own_pixel()
                    || output_extent_of_each_member[read_member]
                        != output_extent_of_each_member[reading_member]
                {
                    continue;
                }
                let fused: Vec<usize> = (0..self.members.len())
                    .filter(|&candidate| {
                        let kernel_output = kernel_output_member_of_each_member[candidate];
                        kernel_output == read_member || kernel_output == reading_member
                    })
                    .collect();
                if self
                    .kernel_of(reading_member, fused)
                    .push_constant_byte_len()
                    > SHADER_NODE_PUSH_CONSTANT_BYTES_AT_MOST
                {
                    continue;
                }
                for kernel_output in &mut kernel_output_member_of_each_member {
                    if *kernel_output == read_member {
                        *kernel_output = reading_member;
                    }
                }
            }
        }
        let mut kernel_output_members: Vec<usize> = kernel_output_member_of_each_member.clone();
        kernel_output_members.sort_unstable();
        kernel_output_members.dedup();
        kernel_output_members
            .into_iter()
            .map(|kernel_output| {
                let members = (0..self.members.len())
                    .filter(|&member| kernel_output_member_of_each_member[member] == kernel_output)
                    .collect();
                self.kernel_of(kernel_output, members)
            })
            .collect()
    }

    fn kernel_of(
        &self,
        kernel_output_member: usize,
        members: Vec<usize>,
    ) -> ShaderNodeIslandKernel {
        debug_assert_eq!(members.last(), Some(&kernel_output_member));
        let fused: HashSet<usize> = members.iter().copied().collect();
        let mut image_arguments = Vec::new();
        let mut float_arguments = Vec::new();
        for &member in &members {
            let mut float_index = 0;
            for input in &self.members[member].inputs {
                match input {
                    ShaderNodeIslandMemberInput::ImageFromTheMember(read_member)
                        if fused.contains(read_member) => {}
                    ShaderNodeIslandMemberInput::ImageFromTheMember(read_member) => image_arguments
                        .push(ShaderNodeKernelImageArgument::StoredOutputOf(*read_member)),
                    ShaderNodeIslandMemberInput::ImageReadAt(input_port) => image_arguments
                        .push(ShaderNodeKernelImageArgument::ReadAt(input_port.clone())),
                    ShaderNodeIslandMemberInput::FloatReadAt { .. } => {
                        float_arguments.push(ShaderNodeKernelFloatArgument {
                            member,
                            float_index,
                        });
                        float_index += 1;
                    }
                }
            }
        }
        ShaderNodeIslandKernel {
            members,
            image_arguments,
            float_arguments,
        }
    }
}

impl ShaderNodeIslandKernel {
    /// The member whose Image this kernel writes.
    pub(crate) fn output_member(&self) -> usize {
        self.members[self.members.len() - 1]
    }

    /// The float arguments, in the order the push constants carry them.
    pub(crate) fn float_arguments(&self) -> &[ShaderNodeKernelFloatArgument] {
        &self.float_arguments
    }

    /// The bytes of the kernel's push constants: the output extent, each
    /// Image argument's extent and row pitch, then each float, four bytes apiece.
    pub(crate) fn push_constant_byte_len(&self) -> usize {
        4 * (2 + 3 * self.image_arguments.len() + self.float_arguments.len())
    }

    /// The push constants of one dispatch, laid out as
    /// [`Self::push_constant_byte_len`] states.
    pub(crate) fn push_constants(
        &self,
        output_extent: (u32, u32),
        image_argument_extents: &[ShaderNodeImageInputExtent],
        float_values: &[f32],
    ) -> Vec<u8> {
        let mut push_constants = Vec::with_capacity(self.push_constant_byte_len());
        push_constants.extend_from_slice(&output_extent.0.to_le_bytes());
        push_constants.extend_from_slice(&output_extent.1.to_le_bytes());
        for extent in image_argument_extents {
            push_constants.extend_from_slice(&extent.width.to_le_bytes());
            push_constants.extend_from_slice(&extent.height.to_le_bytes());
            push_constants.extend_from_slice(&extent.row_pitch_in_pixels.to_le_bytes());
        }
        for float_value in float_values {
            push_constants.extend_from_slice(&float_value.to_le_bytes());
        }
        push_constants
    }

    /// The entry point of the kernel at `kernel_index` of an evaluation, which
    /// keeps two kernels of one submission apart even when their code is alike.
    pub(crate) fn entry_point(kernel_index: usize) -> String {
        format!("streamlib_shader_node_kernel_{kernel_index}")
    }

    /// The binding name of the Image argument at `image_argument_index`.
    pub(crate) fn image_argument_binding_name(image_argument_index: usize) -> String {
        format!("streamlib_shader_node_input_{image_argument_index}")
    }

    /// Every binding the kernel declares for `image_argument_backings`, by name.
    pub(crate) fn declared_bindings(
        image_argument_backings: &[ShaderNodeImageInputBacking],
    ) -> Vec<ComputeBindingDeclaration> {
        std::iter::once(ComputeBindingDeclaration {
            name: SHADER_NODE_OUTPUT_BINDING_NAME.to_string(),
            kind: ComputeBindingKind::StorageBuffer,
        })
        .chain(
            image_argument_backings
                .iter()
                .enumerate()
                .map(
                    |(image_argument_index, backing)| ComputeBindingDeclaration {
                        name: Self::image_argument_binding_name(image_argument_index),
                        kind: backing.binding_kind(),
                    },
                ),
        )
        .collect()
    }

    /// The Slang compute kernel that evaluates each fused member once per
    /// output pixel, a fused member's Image handed to its reader as that
    /// pixel, the Image arguments bound as `image_argument_backings` states.
    /// The members' sources come first, so a diagnostic names their own lines.
    pub(crate) fn compute_kernel_slang_source(
        &self,
        island: &ShaderNodeIslandProgram,
        kernel_index: usize,
        image_argument_backings: &[ShaderNodeImageInputBacking],
    ) -> String {
        let mut kernel = String::new();
        let mut functions_already_declared = HashSet::new();
        for &member in &self.members {
            let program = &island.members[member].program;
            if functions_already_declared.insert(program.function_name.as_str()) {
                kernel.push_str(&program.slang_source);
                kernel.push('\n');
            }
        }
        kernel.push_str(SHADER_NODE_IMAGE_INPUT_PRELUDE);

        kernel.push_str("\nstruct StreamlibShaderNodePushConstants\n{\n");
        kernel.push_str("    uint output_width;\n    uint output_height;\n");
        for image_argument_index in 0..image_argument_backings.len() {
            let _ = write!(
                kernel,
                "    uint input_{image_argument_index}_width;\n    uint \
                 input_{image_argument_index}_height;\n    uint \
                 input_{image_argument_index}_row_pitch_in_pixels;\n"
            );
        }
        for float_index in 0..self.float_arguments.len() {
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
        for (image_argument_index, backing) in image_argument_backings.iter().enumerate() {
            let binding_type = match backing {
                ShaderNodeImageInputBacking::SampledTexture => "Sampler2D",
                ShaderNodeImageInputBacking::Rgba8PixelBuffer => "StructuredBuffer<uint>",
                ShaderNodeImageInputBacking::Rgba16FloatPixelBuffer => "StructuredBuffer<uint2>",
                ShaderNodeImageInputBacking::Rgba32FloatPixelBuffer => "StructuredBuffer<float4>",
            };
            let _ = writeln!(
                kernel,
                "[[vk::binding({}, 0)]] {binding_type} {};",
                image_argument_index + 1,
                Self::image_argument_binding_name(image_argument_index)
            );
        }

        for (image_argument_index, backing) in image_argument_backings.iter().enumerate() {
            kernel.push_str(&image_argument_load_and_sample_functions(
                image_argument_index,
                *backing,
            ));
        }

        kernel.push_str(
            "\nfloat4 streamlib_shader_node_sample_input(uint input_index, float2 uv)\n{\n    \
             switch (input_index)\n    {\n",
        );
        for image_argument_index in 0..image_argument_backings.len() {
            let _ = writeln!(
                kernel,
                "    case {image_argument_index}: return \
                 streamlib_shader_node_sample_input_{image_argument_index}(uv);"
            );
        }
        kernel.push_str("    default: return float4(0.0, 0.0, 0.0, 0.0);\n    }\n}\n");

        let _ = write!(
            kernel,
            "\n[numthreads({SHADER_NODE_KERNEL_WORKGROUP_TILE_SIZE}, \
             {SHADER_NODE_KERNEL_WORKGROUP_TILE_SIZE}, 1)]\nvoid {}(uint3 invocation : \
             SV_DispatchThreadID)\n{{\n    uint2 output_extent = \
             uint2(streamlib_shader_node_push_constants.output_width, \
             streamlib_shader_node_push_constants.output_height);\n    if (any(invocation.xy >= \
             output_extent))\n    {{\n        return;\n    }}\n    float2 uv = \
             (float2(invocation.xy) + 0.5) / float2(output_extent);\n",
            Self::entry_point(kernel_index)
        );
        let fused: HashSet<usize> = self.members.iter().copied().collect();
        let (mut image_argument_index, mut float_argument_index) = (0usize, 0usize);
        for &member in &self.members {
            let island_member = &island.members[member];
            let mut call_arguments = Vec::with_capacity(island_member.inputs.len());
            for (input_index, input) in island_member.inputs.iter().enumerate() {
                let image_input = format!("member_{member}_input_{input_index}");
                match input {
                    ShaderNodeIslandMemberInput::ImageFromTheMember(read_member)
                        if fused.contains(read_member) =>
                    {
                        let _ = write!(
                            kernel,
                            "    ImageInput {image_input};\n    \
                             {image_input}.streamlib_shader_node_input_index = \
                             STREAMLIB_SHADER_NODE_FUSED_INPUT_INDEX;\n    {image_input}.pixel = \
                             member_{read_member}_pixel;\n    {image_input}.uv = uv;\n"
                        );
                        call_arguments.push(image_input);
                    }
                    ShaderNodeIslandMemberInput::ImageFromTheMember(_)
                    | ShaderNodeIslandMemberInput::ImageReadAt(_) => {
                        let _ = write!(
                            kernel,
                            "    ImageInput {image_input};\n    \
                             {image_input}.streamlib_shader_node_input_index = \
                             {image_argument_index};\n    {image_input}.pixel = \
                             streamlib_shader_node_pixel_of_input_{image_argument_index}(uv);\n    \
                             {image_input}.uv = uv;\n"
                        );
                        call_arguments.push(image_input);
                        image_argument_index += 1;
                    }
                    ShaderNodeIslandMemberInput::FloatReadAt { .. } => {
                        call_arguments.push(format!(
                            "streamlib_shader_node_push_constants.float_{float_argument_index}"
                        ));
                        float_argument_index += 1;
                    }
                }
            }
            let _ = writeln!(
                kernel,
                "    float4 member_{member}_pixel = {}({});",
                island_member.program.function_name,
                call_arguments.join(", ")
            );
        }
        let _ = write!(
            kernel,
            "    {SHADER_NODE_OUTPUT_BINDING_NAME}[invocation.y * output_extent.x + invocation.x] \
             = streamlib_shader_node_pack_rgba8(member_{}_pixel);\n}}\n",
            self.output_member()
        );
        kernel
    }
}

/// What every shader node kernel declares beside the members' own sources:
/// `ImageInput`, and the packing of pixel-buffer channels.
const SHADER_NODE_IMAGE_INPUT_PRELUDE: &str = r#"
static const uint STREAMLIB_SHADER_NODE_FUSED_INPUT_INDEX = 0xFFFFFFFFu;

struct ImageInput
{
    float4 pixel;
    float2 uv;
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

/// The functions reading Image argument `image_argument_index` bound as
/// `backing`: its pixel nearest `uv`, and its bilinear sample at `uv`, clamped
/// at the edge.
fn image_argument_load_and_sample_functions(
    image_argument_index: usize,
    backing: ShaderNodeImageInputBacking,
) -> String {
    let binding = ShaderNodeIslandKernel::image_argument_binding_name(image_argument_index);
    let extent = format!(
        "uint2(streamlib_shader_node_push_constants.input_{image_argument_index}_width, \
         streamlib_shader_node_push_constants.input_{image_argument_index}_height)"
    );
    let row_pitch = format!(
        "streamlib_shader_node_push_constants.input_{image_argument_index}_row_pitch_in_pixels"
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
             lerp(streamlib_shader_node_load_input_{image_argument_index}(base), \
             streamlib_shader_node_load_input_{image_argument_index}(base + int2(1, 0)), \
             weight.x);\n    float4 bottom = \
             lerp(streamlib_shader_node_load_input_{image_argument_index}(base + int2(0, 1)), \
             streamlib_shader_node_load_input_{image_argument_index}(base + int2(1, 1)), \
             weight.x);\n    return lerp(top, bottom, weight.y);\n"
        ),
    };
    format!(
        "\nfloat4 streamlib_shader_node_load_input_{image_argument_index}(int2 \
         texel)\n{{\n    uint2 extent = {extent};\n    uint2 clamped_texel = uint2(clamp(texel, \
         int2(0, 0), int2(extent) - 1));\n    return {texel_decoded};\n}}\n\nfloat4 \
         streamlib_shader_node_pixel_of_input_{image_argument_index}(float2 uv)\n{{\n    return \
         streamlib_shader_node_load_input_{image_argument_index}(int2(floor(uv * \
         float2({extent}))));\n}}\n\nfloat4 \
         streamlib_shader_node_sample_input_{image_argument_index}(float2 uv)\n{{\n{sample}}}\n"
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn shader_program(
        function_name: &str,
        slang_body: &str,
        image_ports: &[&str],
        float_ports: &[&str],
    ) -> ShaderNodeProgram {
        let parameters: Vec<String> = image_ports
            .iter()
            .map(|port| format!("ImageInput {port}"))
            .chain(float_ports.iter().map(|port| format!("float {port}")))
            .collect();
        let inputs: Vec<serde_json::Value> = image_ports
            .iter()
            .map(|port| serde_json::json!({"port": port, "value_type": "image_frame"}))
            .chain(
                float_ports
                    .iter()
                    .map(|port| serde_json::json!({"port": port, "value_type": "float"})),
            )
            .collect();
        ShaderNodeProgram::read_from_described_shader(
            &serde_json::json!(format!(
                "float4 {function_name}({})\n{{\n    {slang_body}\n}}\n",
                parameters.join(", ")
            )),
            &serde_json::json!({
                "function_name": function_name,
                "inputs": inputs,
                "output_port": "result",
            }),
        )
        .unwrap_or_else(|refusal| panic!("{function_name}: {refusal}"))
    }

    fn tint() -> ShaderNodeProgram {
        shader_program(
            "tint",
            "return float4(image.pixel.rgb * amount, image.pixel.a);",
            &["image"],
            &["amount"],
        )
    }

    fn invert() -> ShaderNodeProgram {
        shader_program(
            "invert",
            "return float4(1.0 - image.pixel.rgb, image.pixel.a);",
            &["image"],
            &[],
        )
    }

    fn blur() -> ShaderNodeProgram {
        shader_program(
            "blur",
            "return (image.sample(float2(0.25, 0.5)) + image.sample(float2(0.75, 0.5))) * 0.5;",
            &["image"],
            &[],
        )
    }

    fn blend() -> ShaderNodeProgram {
        shader_program(
            "blend",
            "return lerp(a.pixel, b.pixel, 0.5);",
            &["a", "b"],
            &[],
        )
    }

    fn member(
        node_name: &str,
        program: ShaderNodeProgram,
        inputs: Vec<ShaderNodeIslandMemberInput>,
    ) -> ShaderNodeIslandMember {
        ShaderNodeIslandMember {
            node_name: node_name.into(),
            program,
            inputs,
        }
    }

    fn read_at(input_port: &str) -> ShaderNodeIslandMemberInput {
        ShaderNodeIslandMemberInput::ImageReadAt(input_port.into())
    }

    fn float_at(input_port: &str) -> ShaderNodeIslandMemberInput {
        ShaderNodeIslandMemberInput::FloatReadAt {
            input_port: input_port.into(),
            value_until_one_is_read: 0.5,
        }
    }

    fn from_member(member_index: usize) -> ShaderNodeIslandMemberInput {
        ShaderNodeIslandMemberInput::ImageFromTheMember(member_index)
    }

    fn chain_of(programs: Vec<(&str, ShaderNodeProgram)>) -> ShaderNodeIslandProgram {
        let members = programs
            .into_iter()
            .enumerate()
            .map(|(member_index, (node_name, program))| {
                let inputs = program
                    .inputs
                    .iter()
                    .map(|input| match input.value_type {
                        ShaderNodeInputValueType::ImageFrame if member_index == 0 => {
                            read_at("source_image")
                        }
                        ShaderNodeInputValueType::ImageFrame => from_member(member_index - 1),
                        ShaderNodeInputValueType::Float => {
                            float_at(&format!("{node_name}_{}", input.port))
                        }
                    })
                    .collect();
                member(node_name, program, inputs)
            })
            .collect();
        ShaderNodeIslandProgram::of_the_members(members).expect("a chain is an island")
    }

    fn member_names_of_each_kernel(
        island: &ShaderNodeIslandProgram,
        kernels: &[ShaderNodeIslandKernel],
    ) -> Vec<Vec<String>> {
        kernels
            .iter()
            .map(|kernel| {
                kernel
                    .members
                    .iter()
                    .map(|&member| island.members()[member].node_name.clone())
                    .collect()
            })
            .collect()
    }

    fn kernels_at_one_extent(island: &ShaderNodeIslandProgram) -> Vec<ShaderNodeIslandKernel> {
        island.kernels_under_the_split_rule(&island.output_extent_of_each_member(|_| (64, 16)))
    }

    #[test]
    fn a_point_wise_chain_fuses_into_one_kernel() {
        let island = chain_of(vec![
            ("tint", tint()),
            ("invert", invert()),
            ("tint_again", tint()),
        ]);
        let kernels = kernels_at_one_extent(&island);
        assert_eq!(
            member_names_of_each_kernel(&island, &kernels),
            [["tint", "invert", "tint_again"]]
        );
        assert_eq!(
            kernels[0].image_arguments,
            [ShaderNodeKernelImageArgument::ReadAt("source_image".into())]
        );
        assert_eq!(kernels[0].float_arguments().len(), 2);
    }

    #[test]
    fn a_node_sampling_beyond_its_pixel_splits_the_chain_storing_what_it_reads() {
        let island = chain_of(vec![
            ("tint", tint()),
            ("blur", blur()),
            ("invert", invert()),
        ]);
        let kernels = kernels_at_one_extent(&island);
        assert_eq!(
            member_names_of_each_kernel(&island, &kernels),
            [vec!["tint"], vec!["blur", "invert"]]
        );
        assert_eq!(
            kernels[1].image_arguments,
            [ShaderNodeKernelImageArgument::StoredOutputOf(0)]
        );
    }

    #[test]
    fn a_change_of_extent_splits_the_chain() {
        let island = ShaderNodeIslandProgram::of_the_members(vec![
            member("invert", invert(), vec![read_at("small_image")]),
            member(
                "blend",
                blend(),
                vec![read_at("large_image"), from_member(0)],
            ),
        ])
        .unwrap();
        let at_two_extents = island.output_extent_of_each_member(|input_port| {
            if input_port == "small_image" {
                (32, 8)
            } else {
                (64, 16)
            }
        });
        assert_eq!(
            member_names_of_each_kernel(
                &island,
                &island.kernels_under_the_split_rule(&at_two_extents)
            ),
            [vec!["invert"], vec!["blend"]]
        );
        let at_one_extent = island.output_extent_of_each_member(|_| (64, 16));
        assert_eq!(
            member_names_of_each_kernel(
                &island,
                &island.kernels_under_the_split_rule(&at_one_extent)
            ),
            [vec!["invert", "blend"]]
        );
    }

    #[test]
    fn a_fusion_whose_push_constants_would_pass_128_bytes_splits_the_chain() {
        let many_floats = |function_name: &str| {
            let float_ports: Vec<String> = (0..14).map(|index| format!("f{index}")).collect();
            let float_port_names: Vec<&str> = float_ports.iter().map(String::as_str).collect();
            shader_program(
                function_name,
                "return image.pixel * f0;",
                &["image"],
                &float_port_names,
            )
        };
        let island = chain_of(vec![
            ("first", many_floats("first")),
            ("second", many_floats("second")),
        ]);
        let kernels = kernels_at_one_extent(&island);
        assert_eq!(
            member_names_of_each_kernel(&island, &kernels),
            [vec!["first"], vec!["second"]]
        );
        assert!(kernels.iter().all(|kernel| {
            kernel.push_constant_byte_len() <= SHADER_NODE_PUSH_CONSTANT_BYTES_AT_MOST
        }));
    }

    #[test]
    fn a_member_read_twice_or_reading_a_later_one_is_refused() {
        let read_twice = ShaderNodeIslandProgram::of_the_members(vec![
            member("invert", invert(), vec![read_at("image")]),
            member("blend", blend(), vec![from_member(0), from_member(0)]),
        ])
        .unwrap_err();
        assert!(
            read_twice.contains("`invert` is read 2 times"),
            "{read_twice}"
        );
        let reads_later = ShaderNodeIslandProgram::of_the_members(vec![
            member("invert", invert(), vec![from_member(1)]),
            member("tint", tint(), vec![read_at("image"), float_at("amount")]),
        ])
        .unwrap_err();
        assert!(reads_later.contains("evaluated after it"), "{reads_later}");
    }

    #[test]
    fn an_evaluation_waits_on_the_port_the_output_reaches_through_first_images() {
        let island = ShaderNodeIslandProgram::of_the_members(vec![
            member("invert", invert(), vec![read_at("background")]),
            member(
                "blend",
                blend(),
                vec![from_member(0), read_at("foreground")],
            ),
        ])
        .unwrap();
        assert_eq!(
            island.the_input_port_each_evaluation_waits_on(),
            "background"
        );
    }

    #[test]
    fn the_push_constants_are_the_output_extent_each_image_extent_then_each_float() {
        let island = chain_of(vec![("tint", tint()), ("tint_again", tint())]);
        let kernel = &kernels_at_one_extent(&island)[0];
        let extent = ShaderNodeImageInputExtent {
            width: 64,
            height: 16,
            row_pitch_in_pixels: 80,
        };
        let push_constants = kernel.push_constants((64, 16), &[extent], &[0.25, 0.75]);
        assert_eq!(push_constants.len(), kernel.push_constant_byte_len());
        assert_eq!(push_constants.len(), 4 * (2 + 3 + 2));
        assert_eq!(&push_constants[16..20], &80u32.to_le_bytes());
        assert_eq!(&push_constants[20..24], &0.25f32.to_le_bytes());
        assert_eq!(&push_constants[24..28], &0.75f32.to_le_bytes());
    }

    #[test]
    fn a_fused_kernel_declares_each_function_once_and_hands_each_member_the_pixel_before_it() {
        let island = chain_of(vec![
            ("tint", tint()),
            ("invert", invert()),
            ("tint_again", tint()),
        ]);
        let kernel = &kernels_at_one_extent(&island)[0];
        let source = kernel.compute_kernel_slang_source(
            &island,
            0,
            &[ShaderNodeImageInputBacking::Rgba8PixelBuffer],
        );
        assert_eq!(source.matches("float4 tint(").count(), 1, "{source}");
        assert!(
            source.contains("member_1_input_0.pixel = member_0_pixel;"),
            "{source}"
        );
        assert!(
            source.contains(
                "float4 member_2_pixel = tint(member_2_input_0, \
                 streamlib_shader_node_push_constants.float_1);"
            ),
            "{source}"
        );
        assert!(
            source.contains("void streamlib_shader_node_kernel_0("),
            "{source}"
        );
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn every_kernel_compiles_for_every_backing_and_reflects_the_bindings_it_declares() {
        use crate::core::rhi::{
            ShaderCompilationTargetStage, SlangShaderSourceCompiler, derive_bindings_from_spirv,
        };

        let island = ShaderNodeIslandProgram::of_the_members(vec![
            member(
                "tint",
                tint(),
                vec![read_at("background"), float_at("amount")],
            ),
            member("blur", blur(), vec![from_member(0)]),
            member(
                "blend",
                blend(),
                vec![from_member(1), read_at("foreground")],
            ),
        ])
        .unwrap();
        let kernels = kernels_at_one_extent(&island);
        assert_eq!(
            member_names_of_each_kernel(&island, &kernels),
            [vec!["tint"], vec!["blur", "blend"]]
        );
        let compiler = SlangShaderSourceCompiler::new();
        for backing in [
            ShaderNodeImageInputBacking::SampledTexture,
            ShaderNodeImageInputBacking::Rgba8PixelBuffer,
            ShaderNodeImageInputBacking::Rgba16FloatPixelBuffer,
            ShaderNodeImageInputBacking::Rgba32FloatPixelBuffer,
        ] {
            for (kernel_index, kernel) in kernels.iter().enumerate() {
                let backings: Vec<ShaderNodeImageInputBacking> = kernel
                    .image_arguments
                    .iter()
                    .map(|argument| match argument {
                        ShaderNodeKernelImageArgument::ReadAt(_) => backing,
                        ShaderNodeKernelImageArgument::StoredOutputOf(_) => {
                            ShaderNodeImageInputBacking::Rgba8PixelBuffer
                        }
                    })
                    .collect();
                let entry_point = ShaderNodeIslandKernel::entry_point(kernel_index);
                let spirv = compiler
                    .compile_or_reuse(
                        &kernel.compute_kernel_slang_source(&island, kernel_index, &backings),
                        ShaderCompilationTargetStage::Compute,
                        &entry_point,
                        &entry_point,
                    )
                    .unwrap_or_else(|refusal| {
                        panic!("{backing:?} kernel {kernel_index}: {refusal}")
                    });
                let (reflected, push_constant_byte_len) =
                    derive_bindings_from_spirv(&spirv).unwrap();
                crate::core::rhi::reconcile_compute_binding_declarations(
                    &ShaderNodeIslandKernel::declared_bindings(&backings),
                    &reflected,
                )
                .unwrap_or_else(|refusal| panic!("{backing:?} kernel {kernel_index}: {refusal}"));
                assert_eq!(
                    push_constant_byte_len as usize,
                    kernel.push_constant_byte_len()
                );
            }
        }
    }
}
