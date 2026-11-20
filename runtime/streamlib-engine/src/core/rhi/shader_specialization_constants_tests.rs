// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use rspirv_reflect::rspirv::dr::{Builder, Operand};
use rspirv_reflect::spirv::Decoration;

use super::shader_specialization_constants::{
    ShaderDeclaredSpecializationConstant, ShaderSpecializationConstant,
    ShaderSpecializationConstantType, ShaderSpecializationData, ShaderSpecializationMapEntry,
    ShaderSpecializationScalar, ShaderSpecializationValue,
    specialization_constants_a_shader_declares,
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
    let _undecorated = builder.spec_constant_bit32(unsigned_32, 1);
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
                    ShaderSpecializationConstantType::UnspecializableWidth {
                        scalar: ShaderSpecializationScalar::Float,
                        width: 16,
                    },
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

#[test]
fn each_type_reads_as_the_shader_spells_it() {
    let spelled: Vec<String> = [
        ShaderSpecializationConstantType::U32,
        ShaderSpecializationConstantType::F64,
        ShaderSpecializationConstantType::UnspecializableWidth {
            scalar: ShaderSpecializationScalar::SignedInteger,
            width: 16,
        },
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(spelled, vec!["uint32", "float64", "int16"]);
}

#[test]
fn a_spec_id_applied_through_a_decoration_group_reaches_its_constant() {
    let mut builder = Builder::new();
    let unsigned_32 = builder.type_int(32, 0);
    let grouped = builder.spec_constant_bit32(unsigned_32, 8);
    let group = builder.decoration_group();
    builder.decorate(group, Decoration::SpecId, [Operand::LiteralBit32(5)]);
    builder.group_decorate(group, [grouped]);

    let declared = specialization_constants_a_shader_declares(&builder.module());
    assert_eq!(
        declared.get(&5).map(|constant| constant.constant_type),
        Some(ShaderSpecializationConstantType::U32)
    );
}
