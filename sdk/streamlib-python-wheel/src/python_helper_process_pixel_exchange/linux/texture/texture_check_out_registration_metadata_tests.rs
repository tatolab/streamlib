use super::*;
use crate::python_helper_process_pixel_exchange::linux::export_staging::{
    CpuReadbackCopyDirection, memory_type_index_stated_by_a_staging_registration,
};

/// Every recipe value deliberately non-default, so a parse that stops
/// reading the wire and serves its absent-defaults fails on the first
/// field rather than passing on a coincidence.
fn opaque_fd_check_out_response() -> serde_json::Value {
    serde_json::json!({
        "width": 64,
        "height": 32,
        "format": "rgba8_unorm",
        "handle_type": "opaque_fd",
        "vk_image_allocation_size": 8192u64,
        "vk_image_tiling": 1_000_158_000i64, // DRM_FORMAT_MODIFIER_EXT
        "vk_image_usage": 0x4Fu32,           // default set | TRANSIENT_ATTACHMENT (1 << 6)
        "vk_image_mip_levels": 9u32,
        "vk_image_array_layers": 6u32,
        "vk_image_samples": 4i64,
        "vk_memory_type_index": 7u32,
        "exporting_device_uuid": "00112233445566778899aabbccddeeff",
    })
}

#[test]
fn an_opaque_fd_checkout_parses_its_export_contract() {
    let metadata = TextureCheckOutRegistrationMetadata::from_check_out_response(
        "surface#1",
        &opaque_fd_check_out_response(),
    )
    .expect("a complete OPAQUE_FD registration parses");
    let export_contract = metadata
        .opaque_fd_export_contract
        .expect("the contract fields ride every OPAQUE_FD checkout");
    assert_eq!(export_contract.vk_memory_type_index, 7);
    assert_eq!(
        export_contract.exporting_device_uuid,
        [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff
        ]
    );
}

/// The recipe travels the wire, never the defaults: each assertion
/// fails if its field falls back to the documented absent-default.
#[test]
fn the_image_creation_recipe_parses_off_the_wire_not_the_defaults() {
    let metadata = TextureCheckOutRegistrationMetadata::from_check_out_response(
        "surface#1",
        &opaque_fd_check_out_response(),
    )
    .expect("a complete OPAQUE_FD registration parses");
    let recipe = metadata.vk_image_creation_recipe;
    assert_eq!(recipe.vk_image_tiling, 1_000_158_000);
    assert_eq!(recipe.vk_image_usage_flags, 0x4F);
    assert_eq!(recipe.vk_image_mip_levels, 9);
    assert_eq!(recipe.vk_image_array_layers, 6);
    assert_eq!(recipe.vk_image_samples, 4);
}

/// The absent-defaults themselves, pinned to the service's documented
/// values (`new_opaque_fd_export`'s hardcoded shape).
#[test]
fn absent_recipe_fields_fall_back_to_the_documented_defaults() {
    let response = serde_json::json!({
        "width": 64,
        "height": 32,
        "format": "rgba8_unorm",
        "handle_type": "opaque_fd",
        "vk_image_allocation_size": 8192u64,
        "vk_memory_type_index": 7u32,
        "exporting_device_uuid": "00112233445566778899aabbccddeeff",
    });
    let metadata =
        TextureCheckOutRegistrationMetadata::from_check_out_response("surface#1", &response)
            .expect("recipe-less registrations parse with defaults");
    let recipe = metadata.vk_image_creation_recipe;
    assert_eq!(recipe.vk_image_tiling, VK_IMAGE_TILING_DEFAULT);
    assert_eq!(recipe.vk_image_usage_flags, VK_IMAGE_USAGE_DEFAULT);
    assert_eq!(recipe.vk_image_mip_levels, VK_IMAGE_MIP_LEVELS_DEFAULT);
    assert_eq!(recipe.vk_image_array_layers, VK_IMAGE_ARRAY_LAYERS_DEFAULT);
    assert_eq!(recipe.vk_image_samples, VK_IMAGE_SAMPLES_DEFAULT);
}

/// The refusal an OPAQUE_FD checkout earns when `absent_field` is
/// missing, rendered — shared by the two named tests below, which
/// differ only in the field and the phrase that must name it.
fn refusal_for_an_opaque_fd_check_out_missing(absent_field: &str) -> String {
    let mut response = opaque_fd_check_out_response();
    response.as_object_mut().unwrap().remove(absent_field);
    Python::initialize();
    let refusal =
        TextureCheckOutRegistrationMetadata::from_check_out_response("surface#1", &response)
            .err()
            .expect("a missing contract field refuses the checkout")
            .to_string();
    assert!(
        !refusal.contains("  "),
        "the rendered refusal must be one clean sentence: {refusal:?}"
    );
    refusal
}

#[test]
fn an_opaque_fd_checkout_without_a_memory_type_index_is_refused_naming_it() {
    let refusal = refusal_for_an_opaque_fd_check_out_missing("vk_memory_type_index");
    assert!(
        refusal.contains("memory type index"),
        "the refusal must name the missing field: {refusal:?}"
    );
}

#[test]
fn an_opaque_fd_checkout_without_a_device_uuid_is_refused_naming_it() {
    let refusal = refusal_for_an_opaque_fd_check_out_missing("exporting_device_uuid");
    assert!(
        refusal.contains("exporting device UUID"),
        "the refusal must name the missing field: {refusal:?}"
    );
}

#[test]
fn a_dma_buf_checkout_never_carries_the_export_contract() {
    let response = serde_json::json!({
        "width": 64,
        "height": 32,
        "format": "rgba8_unorm",
        "handle_type": "dma_buf",
        "vk_image_allocation_size": 8192u64,
        "drm_format_modifier": 0x0300000000606014u64,
    });
    let metadata =
        TextureCheckOutRegistrationMetadata::from_check_out_response("surface#1", &response)
            .expect("a DMA-BUF registration parses without the contract fields");
    assert!(metadata.opaque_fd_export_contract.is_none());
}

/// The direction is the only thing separating a read-in from a
/// publish on one wire op, so a token typo would quietly copy the
/// wrong way — over a frame the author meant to read.
#[test]
fn each_readback_direction_spells_the_wire_token_its_copy_runs() {
    assert_eq!(
        CpuReadbackCopyDirection::SurfaceIntoStaging.wire_name(),
        "image_to_buffer"
    );
    assert_eq!(
        CpuReadbackCopyDirection::StagingBackIntoSurface.wire_name(),
        "buffer_to_image"
    );
}

#[test]
fn a_readback_staging_registration_states_the_exporters_memory_type_index() {
    let registration = serde_json::json!({ "vk_memory_type_index": 3u32 });
    assert_eq!(
        memory_type_index_stated_by_a_staging_registration("readback", "staging#1", &registration)
            .expect("a stated index parses"),
        3
    );
}

#[test]
fn a_readback_staging_registration_without_a_memory_type_index_is_refused_naming_it() {
    Python::initialize();
    for unusable in [
        serde_json::json!({}),
        serde_json::json!({ "vk_memory_type_index": serde_json::Value::Null }),
        serde_json::json!({ "vk_memory_type_index": "7" }),
        serde_json::json!({ "vk_memory_type_index": u64::from(u32::MAX) + 1 }),
    ] {
        let refusal =
            memory_type_index_stated_by_a_staging_registration("readback", "staging#1", &unusable)
                .err()
                .expect("an unusable index refuses the import")
                .to_string();
        assert!(
            refusal.contains("vk_memory_type_index"),
            "the refusal must name the field it could not read: {refusal:?}"
        );
        assert!(
            refusal.contains("staging#1"),
            "the refusal must name the staging it is about: {refusal:?}"
        );
    }
}
