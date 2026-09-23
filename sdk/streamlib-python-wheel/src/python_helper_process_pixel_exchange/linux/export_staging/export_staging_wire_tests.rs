use super::*;

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
