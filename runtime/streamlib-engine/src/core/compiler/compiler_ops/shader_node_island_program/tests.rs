// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

pub(crate) fn shader_program(
    function_name: &str,
    slang_body: &str,
    image_ports: &[&str],
    float_ports: &[&str],
) -> ShaderNodeProgram {
    let float_ports: Vec<(&str, &str, serde_json::Value)> = float_ports
        .iter()
        .map(|port| (*port, "float", serde_json::Value::Null))
        .collect();
    shader_program_taking_values(function_name, slang_body, image_ports, &float_ports)
}

/// A shader program taking `image_ports`, then each of `value_ports` as its port, its Slang
/// type and the pin type its signature states, a Float stated by its value type alone when that
/// pin type is null.
pub(crate) fn shader_program_taking_values(
    function_name: &str,
    slang_body: &str,
    image_ports: &[&str],
    value_ports: &[(&str, &str, serde_json::Value)],
) -> ShaderNodeProgram {
    let parameters: Vec<String> = image_ports
        .iter()
        .map(|port| format!("ImageInput {port}"))
        .chain(
            value_ports
                .iter()
                .map(|(port, slang_type, _)| format!("{slang_type} {port}")),
        )
        .collect();
    let inputs: Vec<serde_json::Value> = image_ports
        .iter()
        .map(|port| serde_json::json!({"port": port, "value_type": "image_frame"}))
        .chain(value_ports.iter().map(|(port, _, pin_type)| {
            if pin_type.is_null() {
                serde_json::json!({"port": port, "value_type": "float"})
            } else {
                serde_json::json!({
                    "port": port,
                    "value_type": pin_type["type"],
                    "pin_type": pin_type,
                })
            }
        }))
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
    numbers_at(input_port, vec![0.5])
}

fn numbers_at(
    input_port: &str,
    numbers_until_one_is_read: Vec<f32>,
) -> ShaderNodeIslandMemberInput {
    ShaderNodeIslandMemberInput::NumbersReadAt {
        input_port: input_port.into(),
        numbers_until_one_is_read,
    }
}

/// A shader tinting its Image by a Color and shifting it by a Vector2.
fn tint_by_a_color_and_shift() -> ShaderNodeProgram {
    shader_program_taking_values(
        "tint_and_shift",
        "return image.sample(image.uv + shift) * tint_color * amount;",
        &["image"],
        &[
            ("tint_color", "float4", serde_json::json!({"type": "color"})),
            (
                "shift",
                "float2",
                serde_json::json!({"type": "vector", "component_count": "two"}),
            ),
            ("amount", "float", serde_json::json!({"type": "float"})),
        ],
    )
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
                    ShaderNodeInputValueType::Numbers(_) => numbers_at(
                        &format!("{node_name}_{}", input.port),
                        vec![0.5; input.value_type.number_count()],
                    ),
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
    assert_eq!(kernels[0].number_arguments().len(), 2);
}

#[test]
fn a_kernel_whose_member_reads_no_pixel_of_its_image_declares_no_binding_for_it() {
    let stripes_sized_from_an_image = shader_program(
        "stripes",
        "return float4(frac(image.uv.x * count), 0.0, 0.0, 1.0);",
        &["image"],
        &["count"],
    );
    let island = chain_of(vec![("stripes", stripes_sized_from_an_image)]);
    let kernels = kernels_at_one_extent(&island);
    let backings = [ShaderNodeImageInputBacking::Rgba8PixelBuffer];
    let entry_point = ShaderNodeIslandKernel::entry_point(0);
    let spirv = crate::core::rhi::SlangShaderSourceCompiler::new()
        .compile_or_reuse(
            &kernels[0].compute_kernel_slang_source(&island, 0, &backings),
            crate::core::rhi::ShaderCompilationTargetStage::Compute,
            &entry_point,
            &entry_point,
        )
        .expect("the kernel compiles");
    let (reflected, push_constant_byte_len) =
        crate::core::rhi::derive_bindings_from_spirv(&spirv).expect("the kernel reflects");

    let declared =
        ShaderNodeIslandKernel::declared_bindings_the_compiled_kernel_kept(&backings, &reflected);

    assert_eq!(
        declared
            .iter()
            .map(|declaration| declaration.name.as_str())
            .collect::<Vec<_>>(),
        [SHADER_NODE_OUTPUT_BINDING_NAME]
    );
    crate::core::rhi::reconcile_compute_binding_declarations(&declared, &reflected)
        .expect("what the kernel declares is what the shader kept");
    assert_eq!(
        push_constant_byte_len as usize,
        kernels[0].push_constant_byte_len()
    );
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
                 streamlib_shader_node_push_constants.number_1_0);"
        ),
        "{source}"
    );
    assert!(
        source.contains("void streamlib_shader_node_kernel_0("),
        "{source}"
    );
}

#[test]
fn a_color_and_a_vector_carry_one_push_constant_per_part_and_reach_the_shader_whole() {
    let island = chain_of(vec![("tint_and_shift", tint_by_a_color_and_shift())]);
    let kernel = &kernels_at_one_extent(&island)[0];
    assert_eq!(
        kernel
            .number_arguments()
            .iter()
            .map(|argument| argument.number_count)
            .collect::<Vec<_>>(),
        [4, 2, 1]
    );
    assert_eq!(kernel.push_constant_byte_len(), 4 * (2 + 3 + 4 + 2 + 1));
    let extent = ShaderNodeImageInputExtent {
        width: 64,
        height: 16,
        row_pitch_in_pixels: 64,
    };
    let numbers = [1.0, 0.5, 0.25, 1.0, 0.125, -0.125, 2.0];
    let push_constants = kernel.push_constants((64, 16), &[extent], &numbers);
    assert_eq!(push_constants.len(), kernel.push_constant_byte_len());
    for (index, number) in numbers.iter().enumerate() {
        let start = 20 + 4 * index;
        assert_eq!(&push_constants[start..start + 4], &number.to_le_bytes());
    }
    let source = kernel.compute_kernel_slang_source(
        &island,
        0,
        &[ShaderNodeImageInputBacking::Rgba8PixelBuffer],
    );
    assert!(source.contains("    float number_0_3;\n"), "{source}");
    assert!(source.contains("    float number_1_1;\n"), "{source}");
    let constant = |argument: usize, part: usize| {
        format!("streamlib_shader_node_push_constants.number_{argument}_{part}")
    };
    let expected_call = format!(
        "tint_and_shift(member_0_input_0, float4({}, {}, {}, {}), float2({}, {}), {})",
        constant(0, 0),
        constant(0, 1),
        constant(0, 2),
        constant(0, 3),
        constant(1, 0),
        constant(1, 1),
        constant(2, 0),
    );
    assert!(source.contains(&expected_call), "{source}");
}

#[test]
fn an_island_reading_a_value_as_the_wrong_count_of_numbers_is_refused() {
    let refusal = ShaderNodeIslandProgram::of_the_members(vec![member(
        "tint_and_shift",
        tint_by_a_color_and_shift(),
        vec![
            read_at("image"),
            numbers_at("tint_color", vec![1.0]),
            numbers_at("shift", vec![0.0, 0.0]),
            float_at("amount"),
        ],
    )])
    .unwrap_err();
    assert!(
        refusal.contains("reads input `tint_color` as the wrong value type"),
        "{refusal}"
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
        member(
            "tint_and_shift",
            tint_by_a_color_and_shift(),
            vec![
                from_member(2),
                numbers_at("tint_color", vec![1.0, 0.5, 0.25, 1.0]),
                numbers_at("shift", vec![0.0, 0.0]),
                float_at("amount"),
            ],
        ),
    ])
    .unwrap();
    let kernels = kernels_at_one_extent(&island);
    assert_eq!(
        member_names_of_each_kernel(&island, &kernels),
        [vec!["tint"], vec!["blur", "blend"], vec!["tint_and_shift"]]
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
                .unwrap_or_else(|refusal| panic!("{backing:?} kernel {kernel_index}: {refusal}"));
            let (reflected, push_constant_byte_len) = derive_bindings_from_spirv(&spirv).unwrap();
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
