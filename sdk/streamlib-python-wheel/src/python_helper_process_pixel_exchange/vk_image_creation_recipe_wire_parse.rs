// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The `VkImageCreateInfo` recipe a texture checkout carries, parsed the
//! same way on both floors.

/// Absent-defaults for the `vk_image_*` recipe fields, mirroring the
/// surface-share wire's documented defaults
/// (`core/context/surfaces_and_their_stores/surface_share_wire_verbs.rs`).
pub(super) const VK_IMAGE_TILING_DEFAULT: i32 = 0; // VK_IMAGE_TILING_OPTIMAL
pub(super) const VK_IMAGE_MIP_LEVELS_DEFAULT: u32 = 1;
pub(super) const VK_IMAGE_ARRAY_LAYERS_DEFAULT: u32 = 1;
pub(super) const VK_IMAGE_SAMPLES_DEFAULT: i32 = 1; // VK_SAMPLE_COUNT_1_BIT
/// `TRANSFER_SRC (0x01) | TRANSFER_DST (0x02) | SAMPLED (0x04) | STORAGE (0x08)`.
pub(super) const VK_IMAGE_USAGE_DEFAULT: u32 = 0x0F;

/// One `i32` recipe field of a checkout's registration metadata,
/// absent-or-unrepresentable defaulting to the service's documented value.
fn defaulted_i32_check_out_metadata_field(
    response: &serde_json::Value,
    field: &str,
    default: i32,
) -> i32 {
    response
        .get(field)
        .and_then(|value| value.as_i64())
        .and_then(|value| i32::try_from(value).ok())
        .unwrap_or(default)
}

/// The `u32` twin of [`defaulted_i32_check_out_metadata_field`].
fn defaulted_u32_check_out_metadata_field(
    response: &serde_json::Value,
    field: &str,
    default: u32,
) -> u32 {
    response
        .get(field)
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(default)
}

/// The `VkImageCreateInfo` recipe a texture checkout's registration carries,
/// each field absent-defaulting to the service's documented value.
pub(super) fn vk_image_creation_recipe_of_check_out(
    response: &serde_json::Value,
) -> crate::python_node_context::ExportedVkImageCreationRecipe {
    crate::python_node_context::ExportedVkImageCreationRecipe {
        vk_image_tiling: defaulted_i32_check_out_metadata_field(
            response,
            "vk_image_tiling",
            VK_IMAGE_TILING_DEFAULT,
        ),
        vk_image_usage_flags: defaulted_u32_check_out_metadata_field(
            response,
            "vk_image_usage",
            VK_IMAGE_USAGE_DEFAULT,
        ),
        vk_image_mip_levels: defaulted_u32_check_out_metadata_field(
            response,
            "vk_image_mip_levels",
            VK_IMAGE_MIP_LEVELS_DEFAULT,
        ),
        vk_image_array_layers: defaulted_u32_check_out_metadata_field(
            response,
            "vk_image_array_layers",
            VK_IMAGE_ARRAY_LAYERS_DEFAULT,
        ),
        vk_image_samples: defaulted_i32_check_out_metadata_field(
            response,
            "vk_image_samples",
            VK_IMAGE_SAMPLES_DEFAULT,
        ),
    }
}
