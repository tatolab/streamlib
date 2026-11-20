// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Specialization constants: values a kernel bakes into its pipeline when it
//! is built, so one SPIR-V module serves every variant of a shader.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use rspirv_reflect::rspirv::dr::{Module, Operand};
use rspirv_reflect::spirv::{Decoration, Op};

use crate::core::{Error, Result};

/// A value for one specialization constant, in one of the scalar types Vulkan
/// specializes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShaderSpecializationValue {
    Bool(bool),
    I32(i32),
    U32(u32),
    F32(f32),
    I64(i64),
    U64(u64),
    F64(f64),
}

impl ShaderSpecializationValue {
    /// The type a shader must declare its constant with to take this value.
    pub(crate) fn constant_type(self) -> ShaderSpecializationConstantType {
        match self {
            Self::Bool(_) => ShaderSpecializationConstantType::Bool,
            Self::I32(_) => ShaderSpecializationConstantType::I32,
            Self::U32(_) => ShaderSpecializationConstantType::U32,
            Self::F32(_) => ShaderSpecializationConstantType::F32,
            Self::I64(_) => ShaderSpecializationConstantType::I64,
            Self::U64(_) => ShaderSpecializationConstantType::U64,
            Self::F64(_) => ShaderSpecializationConstantType::F64,
        }
    }

    /// Append the bytes Vulkan reads for this value: a `bool` is a 32-bit
    /// `VkBool32`.
    fn append_native_endian_bytes(self, bytes: &mut Vec<u8>) {
        match self {
            Self::Bool(value) => bytes.extend_from_slice(&u32::from(value).to_ne_bytes()),
            Self::I32(value) => bytes.extend_from_slice(&value.to_ne_bytes()),
            Self::U32(value) => bytes.extend_from_slice(&value.to_ne_bytes()),
            Self::F32(value) => bytes.extend_from_slice(&value.to_ne_bytes()),
            Self::I64(value) => bytes.extend_from_slice(&value.to_ne_bytes()),
            Self::U64(value) => bytes.extend_from_slice(&value.to_ne_bytes()),
            Self::F64(value) => bytes.extend_from_slice(&value.to_ne_bytes()),
        }
    }

    /// How many bytes Vulkan reads for this value.
    fn size(self) -> usize {
        match self {
            Self::Bool(_) | Self::I32(_) | Self::U32(_) | Self::F32(_) => 4,
            Self::I64(_) | Self::U64(_) | Self::F64(_) => 8,
        }
    }
}

/// The type a shader declares a specialization constant with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShaderSpecializationConstantType {
    Bool,
    I32,
    U32,
    F32,
    I64,
    U64,
    F64,
    /// A scalar of a width no [`ShaderSpecializationValue`] fills, such as a
    /// 16-bit integer.
    UnspecializableWidth {
        scalar: ShaderSpecializationScalar,
        width: u32,
    },
    /// A type the module does not declare as a scalar.
    Unknown,
}

/// The kind of scalar an unspecializable constant has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShaderSpecializationScalar {
    SignedInteger,
    UnsignedInteger,
    Float,
}

impl fmt::Display for ShaderSpecializationConstantType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool => formatter.write_str("bool"),
            Self::I32 => formatter.write_str("int32"),
            Self::U32 => formatter.write_str("uint32"),
            Self::F32 => formatter.write_str("float32"),
            Self::I64 => formatter.write_str("int64"),
            Self::U64 => formatter.write_str("uint64"),
            Self::F64 => formatter.write_str("float64"),
            Self::UnspecializableWidth { scalar, width } => {
                let scalar = match scalar {
                    ShaderSpecializationScalar::SignedInteger => "int",
                    ShaderSpecializationScalar::UnsignedInteger => "uint",
                    ShaderSpecializationScalar::Float => "float",
                };
                write!(formatter, "{scalar}{width}")
            }
            Self::Unknown => formatter.write_str("a type that is not a scalar"),
        }
    }
}

/// One specialization constant a kernel sets: the shader's `constant_id` for
/// it and the value baked in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShaderSpecializationConstant {
    pub constant_id: u32,
    pub value: ShaderSpecializationValue,
}

/// A specialization constant a shader declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShaderDeclaredSpecializationConstant {
    pub(crate) constant_type: ShaderSpecializationConstantType,
    /// The shader's own name for it, when its `OpName` was kept.
    pub(crate) name: Option<String>,
}

/// Where one constant's value sits in [`ShaderSpecializationData::bytes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShaderSpecializationMapEntry {
    pub(crate) constant_id: u32,
    pub(crate) offset: usize,
    pub(crate) size: usize,
}

/// The map entries and data a pipeline's specialization info is built from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ShaderSpecializationData {
    pub(crate) map_entries: Vec<ShaderSpecializationMapEntry>,
    pub(crate) bytes: Vec<u8>,
}

impl ShaderSpecializationData {
    /// Lay `constants` out one after another, each at an offset its own size
    /// divides.
    pub(crate) fn of_the_constants(constants: &[ShaderSpecializationConstant]) -> Self {
        let mut data = Self::default();
        for constant in constants {
            let size = constant.value.size();
            let aligned_offset = data.bytes.len().next_multiple_of(size);
            data.bytes.resize(aligned_offset, 0);
            data.map_entries.push(ShaderSpecializationMapEntry {
                constant_id: constant.constant_id,
                offset: aligned_offset,
                size,
            });
            constant.value.append_native_endian_bytes(&mut data.bytes);
        }
        data
    }
}

/// Every specialization constant `shader_module` declares, by `constant_id`.
pub(crate) fn specialization_constants_a_shader_declares(
    shader_module: &Module,
) -> BTreeMap<u32, ShaderDeclaredSpecializationConstant> {
    let constant_id_of_each_result_id: BTreeMap<u32, u32> = shader_module
        .annotations
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::Decorate)
        .filter_map(|instruction| match instruction.operands.as_slice() {
            [
                Operand::IdRef(result_id),
                Operand::Decoration(Decoration::SpecId),
                Operand::LiteralBit32(constant_id),
            ] => Some((*result_id, *constant_id)),
            _ => None,
        })
        .collect();
    let name_of_each_result_id: BTreeMap<u32, &str> = shader_module
        .debug_names
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::Name)
        .filter_map(|instruction| match instruction.operands.as_slice() {
            [Operand::IdRef(result_id), Operand::LiteralString(name), ..] => {
                Some((*result_id, name.as_str()))
            }
            _ => None,
        })
        .collect();
    let type_of_each_type_id: BTreeMap<u32, ShaderSpecializationConstantType> = shader_module
        .types_global_values
        .iter()
        .filter_map(|instruction| {
            let constant_type = match (instruction.class.opcode, instruction.operands.as_slice()) {
                (Op::TypeBool, _) => ShaderSpecializationConstantType::Bool,
                (
                    Op::TypeInt,
                    [
                        Operand::LiteralBit32(width),
                        Operand::LiteralBit32(signedness),
                    ],
                ) => match (width, signedness) {
                    (32, 1) => ShaderSpecializationConstantType::I32,
                    (32, _) => ShaderSpecializationConstantType::U32,
                    (64, 1) => ShaderSpecializationConstantType::I64,
                    (64, _) => ShaderSpecializationConstantType::U64,
                    (width, 1) => ShaderSpecializationConstantType::UnspecializableWidth {
                        scalar: ShaderSpecializationScalar::SignedInteger,
                        width: *width,
                    },
                    (width, _) => ShaderSpecializationConstantType::UnspecializableWidth {
                        scalar: ShaderSpecializationScalar::UnsignedInteger,
                        width: *width,
                    },
                },
                (Op::TypeFloat, [Operand::LiteralBit32(width), ..]) => match width {
                    32 => ShaderSpecializationConstantType::F32,
                    64 => ShaderSpecializationConstantType::F64,
                    width => ShaderSpecializationConstantType::UnspecializableWidth {
                        scalar: ShaderSpecializationScalar::Float,
                        width: *width,
                    },
                },
                _ => return None,
            };
            Some((instruction.result_id?, constant_type))
        })
        .collect();
    shader_module
        .types_global_values
        .iter()
        .filter(|instruction| {
            matches!(
                instruction.class.opcode,
                Op::SpecConstant | Op::SpecConstantTrue | Op::SpecConstantFalse
            )
        })
        .filter_map(|instruction| {
            let result_id = instruction.result_id?;
            let constant_id = *constant_id_of_each_result_id.get(&result_id)?;
            let constant_type = instruction
                .result_type
                .and_then(|type_id| type_of_each_type_id.get(&type_id).copied())
                .unwrap_or(ShaderSpecializationConstantType::Unknown);
            let declared = ShaderDeclaredSpecializationConstant {
                constant_type,
                name: name_of_each_result_id
                    .get(&result_id)
                    .map(|name| (*name).to_string()),
            };
            Some((constant_id, declared))
        })
        .collect()
}

/// Refuse a set of specialization constants the shader does not take: one set
/// twice, one the shader does not declare, or one of another type than the
/// shader's. A constant the shader declares and the kernel leaves unset keeps
/// the shader's default.
pub(crate) fn refuse_specialization_constants_the_shader_does_not_take(
    kernel_label: &str,
    constants: &[ShaderSpecializationConstant],
    shader_module: &Module,
) -> Result<()> {
    if constants.is_empty() {
        return Ok(());
    }
    let declared = specialization_constants_a_shader_declares(shader_module);
    let mut set_constant_ids = BTreeSet::new();
    for constant in constants {
        if !set_constant_ids.insert(constant.constant_id) {
            return Err(Error::GpuError(format!(
                "{kernel_label}: specialization constant {} is set twice",
                constant.constant_id
            )));
        }
        let Some(shader_constant) = declared.get(&constant.constant_id) else {
            return Err(Error::GpuError(format!(
                "{kernel_label}: sets specialization constant {}, which the shader does not \
                 declare; the shader declares {}",
                constant.constant_id,
                quote_the_declared_constants(&declared)
            )));
        };
        let value_type = constant.value.constant_type();
        if shader_constant.constant_type != value_type {
            return Err(Error::GpuError(format!(
                "{kernel_label}: sets specialization constant {}{} as {value_type}, but the \
                 shader declares it {}",
                constant.constant_id,
                shader_constant
                    .name
                    .as_deref()
                    .map(|name| format!(" (`{name}`)"))
                    .unwrap_or_default(),
                shader_constant.constant_type
            )));
        }
    }
    Ok(())
}

fn quote_the_declared_constants(
    declared: &BTreeMap<u32, ShaderDeclaredSpecializationConstant>,
) -> String {
    if declared.is_empty() {
        return "none".to_string();
    }
    declared
        .iter()
        .map(|(constant_id, constant)| match constant.name.as_deref() {
            Some(name) => format!("{constant_id} (`{name}`, {})", constant.constant_type),
            None => format!("{constant_id} ({})", constant.constant_type),
        })
        .collect::<Vec<_>>()
        .join(", ")
}
