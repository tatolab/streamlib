// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The refusals a kernel constructor raises for a capability the device's
//! driver does not serve — the ray-tracing tier, and subgroup operations.

use rspirv_reflect::rspirv;
use rspirv_reflect::rspirv::dr::Operand;
use rspirv_reflect::spirv::{Capability, Op};
use vulkanalia::vk;

use crate::core::{Error, Result};

/// What one device's driver serves of subgroup operations, read once at
/// device construction from `VkPhysicalDeviceSubgroupProperties`.
#[derive(Debug, Clone)]
pub(crate) struct VulkanSubgroupOperationSupport {
    pub(crate) driver_name: String,
    pub(crate) supported_stages: vk::ShaderStageFlags,
    pub(crate) supported_operations: vk::SubgroupFeatureFlags,
}

impl VulkanSubgroupOperationSupport {
    /// Refuse a shader stage whose declared subgroup operations — or whose
    /// stage, if it declares any — this driver does not serve, naming the
    /// driver and what is missing.
    pub(crate) fn refuse_a_shader_the_driver_cannot_serve(
        &self,
        kernel_kind_label: &str,
        stage: vk::ShaderStageFlags,
        shader_module: &rspirv::dr::Module,
    ) -> Result<()> {
        let required = subgroup_operations_a_spirv_module_declares(shader_module);
        if required.is_empty() {
            return Ok(());
        }
        if !self.supported_stages.contains(stage) {
            return Err(Error::GpuError(format!(
                "{kernel_kind_label}: its {stage:?} stage uses subgroup operations ({required:?}), \
                 and the {} driver serves subgroup operations only in {:?} stages",
                self.driver_name, self.supported_stages
            )));
        }
        let unserved = required.difference(self.supported_operations);
        if !unserved.is_empty() {
            return Err(Error::GpuError(format!(
                "{kernel_kind_label}: its {stage:?} stage uses subgroup operations ({unserved:?}) \
                 the {} driver does not serve on this device",
                self.driver_name
            )));
        }
        Ok(())
    }

    /// A driver serving every subgroup operation in every stage, for tests
    /// that validate a kernel without a device.
    #[cfg(test)]
    pub(crate) fn serving_every_operation_in_every_stage() -> Self {
        Self {
            driver_name: "every-operation test driver".to_string(),
            supported_stages: vk::ShaderStageFlags::ALL,
            supported_operations: vk::SubgroupFeatureFlags::all(),
        }
    }
}

/// The refusal every ray-tracing constructor answers on a device without the
/// ray-tracing tier.
pub(crate) fn ray_tracing_tier_absent_refusal(
    constructor_description: &str,
    device_name: &str,
) -> Error {
    Error::GpuError(format!(
        "{constructor_description}: the ray-tracing tier is absent on this device ({device_name}) \
         — it exposes no VK_KHR_ray_tracing_pipeline / VK_KHR_acceleration_structure chain, so it \
         can build neither acceleration structures nor ray-tracing pipelines"
    ))
}

/// The subgroup operation categories a SPIR-V module declares through its
/// `OpCapability` instructions, plus `ROTATE_CLUSTERED` when a rotate carries
/// the optional `ClusterSize` operand — the capability alone does not say so.
fn subgroup_operations_a_spirv_module_declares(
    shader_module: &rspirv::dr::Module,
) -> vk::SubgroupFeatureFlags {
    let declared = shader_module
        .capabilities
        .iter()
        .flat_map(|instruction| &instruction.operands)
        .fold(
            vk::SubgroupFeatureFlags::empty(),
            |declared, operand| match operand {
                Operand::Capability(capability) => {
                    declared | subgroup_operation_of_a_spirv_capability(*capability)
                }
                _ => declared,
            },
        );
    if a_spirv_module_rotates_within_clusters(shader_module) {
        declared | vk::SubgroupFeatureFlags::ROTATE_CLUSTERED
    } else {
        declared
    }
}

/// Whether any `OpGroupNonUniformRotateKHR` carries its fourth operand,
/// `ClusterSize` (after `Execution`, `Value` and `Delta`).
fn a_spirv_module_rotates_within_clusters(shader_module: &rspirv::dr::Module) -> bool {
    const ROTATE_OPERAND_INDEX_OF_CLUSTER_SIZE: usize = 3;
    shader_module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .any(|instruction| {
            instruction.class.opcode == Op::GroupNonUniformRotateKHR
                && instruction.operands.len() > ROTATE_OPERAND_INDEX_OF_CLUSTER_SIZE
        })
}

/// The subgroup operation category a SPIR-V capability enables, empty for
/// every capability that is not a `GroupNonUniform*` one.
fn subgroup_operation_of_a_spirv_capability(capability: Capability) -> vk::SubgroupFeatureFlags {
    match capability {
        Capability::GroupNonUniform => vk::SubgroupFeatureFlags::BASIC,
        Capability::GroupNonUniformVote => vk::SubgroupFeatureFlags::VOTE,
        Capability::GroupNonUniformArithmetic => vk::SubgroupFeatureFlags::ARITHMETIC,
        Capability::GroupNonUniformBallot => vk::SubgroupFeatureFlags::BALLOT,
        Capability::GroupNonUniformShuffle => vk::SubgroupFeatureFlags::SHUFFLE,
        Capability::GroupNonUniformShuffleRelative => vk::SubgroupFeatureFlags::SHUFFLE_RELATIVE,
        Capability::GroupNonUniformClustered => vk::SubgroupFeatureFlags::CLUSTERED,
        Capability::GroupNonUniformQuad => vk::SubgroupFeatureFlags::QUAD,
        Capability::GroupNonUniformRotateKHR => vk::SubgroupFeatureFlags::ROTATE,
        Capability::GroupNonUniformPartitionedNV => vk::SubgroupFeatureFlags::PARTITIONED_EXT,
        _ => vk::SubgroupFeatureFlags::empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_module_declaring(capabilities: &[Capability]) -> rspirv::dr::Module {
        let mut builder = rspirv::dr::Builder::new();
        for &capability in capabilities {
            builder.capability(capability);
        }
        builder.module()
    }

    fn a_driver_serving(
        supported_stages: vk::ShaderStageFlags,
        supported_operations: vk::SubgroupFeatureFlags,
    ) -> VulkanSubgroupOperationSupport {
        VulkanSubgroupOperationSupport {
            driver_name: "MoltenVK".to_string(),
            supported_stages,
            supported_operations,
        }
    }

    #[test]
    fn a_module_declares_only_the_subgroup_capabilities_it_names() {
        let declared = subgroup_operations_a_spirv_module_declares(&a_module_declaring(&[
            Capability::Shader,
            Capability::GroupNonUniform,
            Capability::GroupNonUniformClustered,
        ]));
        assert_eq!(
            declared,
            vk::SubgroupFeatureFlags::BASIC | vk::SubgroupFeatureFlags::CLUSTERED
        );
    }

    fn a_module_rotating(cluster_size: Option<rspirv::spirv::Word>) -> rspirv::dr::Module {
        let mut shader_module = a_module_declaring(&[Capability::GroupNonUniformRotateKHR]);
        let mut operands = vec![Operand::IdRef(10), Operand::IdRef(11), Operand::IdRef(12)];
        operands.extend(cluster_size.map(Operand::IdRef));
        let mut block = rspirv::dr::Block::new();
        block.instructions.push(rspirv::dr::Instruction::new(
            Op::GroupNonUniformRotateKHR,
            Some(1),
            Some(2),
            operands,
        ));
        let mut function = rspirv::dr::Function::new();
        function.blocks.push(block);
        shader_module.functions.push(function);
        shader_module
    }

    #[test]
    fn rotate_and_partitioned_capabilities_declare_their_operations() {
        assert_eq!(
            subgroup_operations_a_spirv_module_declares(&a_module_declaring(&[
                Capability::GroupNonUniformRotateKHR,
                Capability::GroupNonUniformPartitionedNV,
            ])),
            vk::SubgroupFeatureFlags::ROTATE | vk::SubgroupFeatureFlags::PARTITIONED_EXT
        );
    }

    #[test]
    fn a_rotate_within_clusters_declares_clustered_rotation_and_a_plain_one_does_not() {
        assert_eq!(
            subgroup_operations_a_spirv_module_declares(&a_module_rotating(None)),
            vk::SubgroupFeatureFlags::ROTATE
        );
        assert_eq!(
            subgroup_operations_a_spirv_module_declares(&a_module_rotating(Some(13))),
            vk::SubgroupFeatureFlags::ROTATE | vk::SubgroupFeatureFlags::ROTATE_CLUSTERED
        );
    }

    #[test]
    fn a_clustered_rotate_on_a_driver_serving_only_plain_rotation_is_refused() {
        let driver = a_driver_serving(
            vk::ShaderStageFlags::COMPUTE,
            vk::SubgroupFeatureFlags::BASIC | vk::SubgroupFeatureFlags::ROTATE,
        );
        let refusal = driver
            .refuse_a_shader_the_driver_cannot_serve(
                "Compute kernel 'clustered-rotate'",
                vk::ShaderStageFlags::COMPUTE,
                &a_module_rotating(Some(13)),
            )
            .expect_err("clustered rotation the driver does not serve must be refused")
            .to_string();
        assert!(refusal.contains("ROTATE_CLUSTERED"), "{refusal}");
    }

    #[test]
    fn a_shader_with_no_subgroup_operations_passes_on_any_driver() {
        let driver = a_driver_serving(
            vk::ShaderStageFlags::empty(),
            vk::SubgroupFeatureFlags::empty(),
        );
        driver
            .refuse_a_shader_the_driver_cannot_serve(
                "Compute kernel 'plain'",
                vk::ShaderStageFlags::COMPUTE,
                &a_module_declaring(&[Capability::Shader]),
            )
            .expect("a shader declaring no subgroup capability needs nothing from the driver");
    }

    #[test]
    fn a_served_subgroup_operation_passes() {
        let driver = a_driver_serving(
            vk::ShaderStageFlags::COMPUTE,
            vk::SubgroupFeatureFlags::BASIC | vk::SubgroupFeatureFlags::ARITHMETIC,
        );
        driver
            .refuse_a_shader_the_driver_cannot_serve(
                "Compute kernel 'reduce'",
                vk::ShaderStageFlags::COMPUTE,
                &a_module_declaring(&[
                    Capability::GroupNonUniform,
                    Capability::GroupNonUniformArithmetic,
                ]),
            )
            .expect("every declared operation is served");
    }

    #[test]
    fn an_unserved_subgroup_operation_is_refused_naming_the_driver_and_the_operation() {
        let driver = a_driver_serving(
            vk::ShaderStageFlags::COMPUTE,
            vk::SubgroupFeatureFlags::BASIC | vk::SubgroupFeatureFlags::ARITHMETIC,
        );
        let refusal = driver
            .refuse_a_shader_the_driver_cannot_serve(
                "Compute kernel 'clustered'",
                vk::ShaderStageFlags::COMPUTE,
                &a_module_declaring(&[
                    Capability::GroupNonUniform,
                    Capability::GroupNonUniformClustered,
                ]),
            )
            .expect_err("a clustered operation the driver does not serve must be refused")
            .to_string();
        assert!(refusal.contains("Compute kernel 'clustered'"), "{refusal}");
        assert!(refusal.contains("MoltenVK"), "{refusal}");
        assert!(refusal.contains("CLUSTERED"), "{refusal}");
        assert!(!refusal.contains("BASIC"), "{refusal}");
    }

    #[test]
    fn a_subgroup_operation_in_a_stage_the_driver_does_not_serve_is_refused() {
        let driver = a_driver_serving(
            vk::ShaderStageFlags::COMPUTE | vk::ShaderStageFlags::FRAGMENT,
            vk::SubgroupFeatureFlags::BASIC,
        );
        let refusal = driver
            .refuse_a_shader_the_driver_cannot_serve(
                "Graphics kernel 'vertex-ballot'",
                vk::ShaderStageFlags::VERTEX,
                &a_module_declaring(&[Capability::GroupNonUniform]),
            )
            .expect_err("a subgroup operation in an unserved stage must be refused")
            .to_string();
        assert!(refusal.contains("MoltenVK"), "{refusal}");
        assert!(refusal.contains("VERTEX"), "{refusal}");
    }

    #[test]
    fn the_ray_tracing_refusal_names_the_tier_and_the_extension() {
        let refusal = ray_tracing_tier_absent_refusal("Ray-tracing kernel 'scene'", "Apple M1 Max")
            .to_string();
        assert!(refusal.contains("Ray-tracing kernel 'scene'"), "{refusal}");
        assert!(refusal.contains("ray-tracing tier is absent"), "{refusal}");
        assert!(refusal.contains("VK_KHR_ray_tracing_pipeline"), "{refusal}");
        assert!(refusal.contains("Apple M1 Max"), "{refusal}");
    }
}
