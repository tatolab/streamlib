// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The refusals a kernel constructor raises for a capability the device's
//! driver does not serve — the ray-tracing tier, and subgroup operations.

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
        kernel_description: &str,
        stage: vk::ShaderStageFlags,
        spirv_words: &[u32],
    ) -> Result<()> {
        let required = subgroup_operations_a_spirv_module_declares(spirv_words);
        if required.is_empty() {
            return Ok(());
        }
        if !self.supported_stages.contains(stage) {
            return Err(Error::GpuError(format!(
                "{kernel_description}: its {stage:?} stage uses subgroup operations ({required:?}), \
                 and the {} driver serves subgroup operations only in {:?} stages",
                self.driver_name, self.supported_stages
            )));
        }
        let unserved = required.difference(self.supported_operations);
        if !unserved.is_empty() {
            return Err(Error::GpuError(format!(
                "{kernel_description}: its {stage:?} stage uses subgroup operations ({unserved:?}) \
                 the {} driver does not serve on this device",
                self.driver_name
            )));
        }
        Ok(())
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

const SPIRV_HEADER_WORD_COUNT: usize = 5;
const SPIRV_OP_CAPABILITY: u32 = 17;

/// The subgroup operation categories a SPIR-V module declares through its
/// `OpCapability` instructions. SPIR-V's logical layout puts every capability
/// first, so the scan stops at the first instruction that is not one.
fn subgroup_operations_a_spirv_module_declares(spirv_words: &[u32]) -> vk::SubgroupFeatureFlags {
    let mut declared = vk::SubgroupFeatureFlags::empty();
    let mut cursor = SPIRV_HEADER_WORD_COUNT;
    while let Some(&first_word) = spirv_words.get(cursor) {
        let word_count = (first_word >> 16) as usize;
        let opcode = first_word & 0xffff;
        if opcode != SPIRV_OP_CAPABILITY || word_count < 2 {
            break;
        }
        if let Some(&capability) = spirv_words.get(cursor + 1) {
            declared |= subgroup_operation_of_a_spirv_capability(capability);
        }
        cursor += word_count;
    }
    declared
}

/// The subgroup operation category a SPIR-V `Capability` enumerant enables,
/// empty for every capability that is not a `GroupNonUniform*` one.
fn subgroup_operation_of_a_spirv_capability(capability: u32) -> vk::SubgroupFeatureFlags {
    match capability {
        61 => vk::SubgroupFeatureFlags::BASIC,
        62 => vk::SubgroupFeatureFlags::VOTE,
        63 => vk::SubgroupFeatureFlags::ARITHMETIC,
        64 => vk::SubgroupFeatureFlags::BALLOT,
        65 => vk::SubgroupFeatureFlags::SHUFFLE,
        66 => vk::SubgroupFeatureFlags::SHUFFLE_RELATIVE,
        67 => vk::SubgroupFeatureFlags::CLUSTERED,
        68 => vk::SubgroupFeatureFlags::QUAD,
        _ => vk::SubgroupFeatureFlags::empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPIRV_MAGIC: u32 = 0x0723_0203;
    const CAPABILITY_SHADER: u32 = 1;
    const CAPABILITY_GROUP_NON_UNIFORM: u32 = 61;
    const CAPABILITY_GROUP_NON_UNIFORM_ARITHMETIC: u32 = 63;
    const CAPABILITY_GROUP_NON_UNIFORM_CLUSTERED: u32 = 67;
    const OP_MEMORY_MODEL: u32 = 14;

    fn spirv_declaring(capabilities: &[u32]) -> Vec<u32> {
        let mut words = vec![SPIRV_MAGIC, 0x0001_0300, 0, 16, 0];
        for &capability in capabilities {
            words.push((2 << 16) | SPIRV_OP_CAPABILITY);
            words.push(capability);
        }
        words.push((3 << 16) | OP_MEMORY_MODEL);
        words.extend([0, 1]);
        words
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
        let declared = subgroup_operations_a_spirv_module_declares(&spirv_declaring(&[
            CAPABILITY_SHADER,
            CAPABILITY_GROUP_NON_UNIFORM,
            CAPABILITY_GROUP_NON_UNIFORM_CLUSTERED,
        ]));
        assert_eq!(
            declared,
            vk::SubgroupFeatureFlags::BASIC | vk::SubgroupFeatureFlags::CLUSTERED
        );
    }

    #[test]
    fn a_capability_after_the_preamble_is_not_read_as_one() {
        let mut words = spirv_declaring(&[CAPABILITY_SHADER]);
        words.push((2 << 16) | SPIRV_OP_CAPABILITY);
        words.push(CAPABILITY_GROUP_NON_UNIFORM_CLUSTERED);
        assert!(subgroup_operations_a_spirv_module_declares(&words).is_empty());
    }

    #[test]
    fn a_truncated_module_declares_what_it_carries_and_no_more() {
        let words = spirv_declaring(&[CAPABILITY_GROUP_NON_UNIFORM]);
        assert!(subgroup_operations_a_spirv_module_declares(&words[..3]).is_empty());
        assert_eq!(
            subgroup_operations_a_spirv_module_declares(&words[..7]),
            vk::SubgroupFeatureFlags::BASIC
        );
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
                &spirv_declaring(&[CAPABILITY_SHADER]),
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
                &spirv_declaring(&[
                    CAPABILITY_GROUP_NON_UNIFORM,
                    CAPABILITY_GROUP_NON_UNIFORM_ARITHMETIC,
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
                &spirv_declaring(&[
                    CAPABILITY_GROUP_NON_UNIFORM,
                    CAPABILITY_GROUP_NON_UNIFORM_CLUSTERED,
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
                &spirv_declaring(&[CAPABILITY_GROUP_NON_UNIFORM]),
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
