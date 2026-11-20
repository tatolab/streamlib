// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use rspirv_reflect::rspirv::dr::{Builder, Operand};
use rspirv_reflect::spirv::Decoration;

use super::shader_specialization_constants::{
    ShaderDeclaredSpecializationConstant, ShaderSpecializationConstant,
    ShaderSpecializationConstantType, ShaderSpecializationData, ShaderSpecializationMapEntry,
    ShaderSpecializationValue, specialization_constants_a_shader_declares,
};

#[test]
fn each_value_sits_at_an_offset_its_own_size_divides() {
    let data = ShaderSpecializationData::of_the_constants(&[
        ShaderSpecializationConstant {
            constant_id: 3,
            value: ShaderSpecializationValue::Bool(true),
        },
        ShaderSpecializationConstant {
            constant_id: 1,
            value: ShaderSpecializationValue::F64(0.5),
        },
        ShaderSpecializationConstant {
            constant_id: 9,
            value: ShaderSpecializationValue::I32(-2),
        },
    ]);
    assert_eq!(
        data.map_entries,
        vec![
            ShaderSpecializationMapEntry {
                constant_id: 3,
                offset: 0,
                size: 4,
            },
            ShaderSpecializationMapEntry {
                constant_id: 1,
                offset: 8,
                size: 8,
            },
            ShaderSpecializationMapEntry {
                constant_id: 9,
                offset: 16,
                size: 4,
            },
        ]
    );
    let mut expected = 1u32.to_ne_bytes().to_vec();
    expected.extend_from_slice(&[0; 4]);
    expected.extend_from_slice(&0.5f64.to_ne_bytes());
    expected.extend_from_slice(&(-2i32).to_ne_bytes());
    assert_eq!(
        data.bytes, expected,
        "a bool is a 32-bit VkBool32, padded before an 8-byte value"
    );
}

#[test]
fn no_constants_lay_out_no_data() {
    assert_eq!(
        ShaderSpecializationData::of_the_constants(&[]),
        ShaderSpecializationData::default()
    );
}

#[test]
fn reflection_reads_each_constants_id_type_and_name() {
    let mut builder = Builder::new();
    let unsigned_32 = builder.type_int(32, 0);
    let signed_64 = builder.type_int(64, 1);
    let half_float = builder.type_float(16);
    let boolean = builder.type_bool();
    let workgroup_size = builder.spec_constant_bit32(unsigned_32, 64);
    let offset = builder.spec_constant_bit64(signed_64, 0);
    let half_scale = builder.spec_constant_bit32(half_float, 0);
    let enabled = builder.spec_constant_true(boolean);
    let undecorated = builder.spec_constant_bit32(unsigned_32, 1);
    for (result_id, constant_id) in [
        (workgroup_size, 0),
        (offset, 4),
        (half_scale, 2),
        (enabled, 9),
    ] {
        builder.decorate(
            result_id,
            Decoration::SpecId,
            [Operand::LiteralBit32(constant_id)],
        );
    }
    builder.name(workgroup_size, "WORKGROUP_SIZE");
    builder.name(enabled, "ENABLED");
    let _ = undecorated;

    let declared: Vec<(u32, ShaderDeclaredSpecializationConstant)> =
        specialization_constants_a_shader_declares(&builder.module())
            .into_iter()
            .collect();
    let named = |constant_type, name: Option<&str>| ShaderDeclaredSpecializationConstant {
        constant_type,
        name: name.map(str::to_string),
    };
    assert_eq!(
        declared,
        vec![
            (
                0,
                named(
                    ShaderSpecializationConstantType::U32,
                    Some("WORKGROUP_SIZE")
                )
            ),
            (
                2,
                named(
                    ShaderSpecializationConstantType::Unspecializable("float16".to_string()),
                    None
                )
            ),
            (4, named(ShaderSpecializationConstantType::I64, None)),
            (
                9,
                named(ShaderSpecializationConstantType::Bool, Some("ENABLED"))
            ),
        ],
        "a constant with no SpecId is not specializable and is left out"
    );
}
