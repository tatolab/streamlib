use super::*;

fn wire_entries<'py>(wire: &Bound<'py, PyList>) -> Vec<Bound<'py, PyAny>> {
    wire.iter().collect()
}

fn wire_text(entry: &Bound<'_, PyAny>, field: &str) -> String {
    entry.get_item(field).unwrap().extract().unwrap()
}

fn wire_number(entry: &Bound<'_, PyAny>, field: &str) -> u32 {
    entry.get_item(field).unwrap().extract().unwrap()
}

fn bottom_level_structure(python: Python<'_>) -> Py<PythonAccelerationStructureHandle> {
    Py::new(
        python,
        PythonAccelerationStructureHandle {
            acceleration_structure_id: "blas-under-test".to_string(),
            is_top_level: false,
            structure_label: "floor".to_string(),
            helper_process_exchange_client: None,
        },
    )
    .unwrap()
}

/// A declaration asserts the kind; naming stages is optional, and naming
/// none of them asserts nothing so reflection stands.
#[test]
fn a_binding_declaration_carries_the_stages_it_names_and_no_others() {
    Python::initialize();
    Python::attach(|python| {
        let declared = PyDict::new(python);
        declared
            .set_item("scene_texture", "sampled_texture")
            .unwrap();
        declared
            .set_item(
                "output_image",
                ("storage_image", vec!["vertex", "fragment"]),
            )
            .unwrap();

        let wire = declared_staged_kernel_bindings_to_wire(
            python,
            Some(&declared),
            "graphics binding kind",
            GRAPHICS_BINDING_KIND_WIRE_NAMES,
            "graphics stage",
            GRAPHICS_SHADER_STAGE_WIRE_BITS,
        )
        .unwrap();

        let entries = wire_entries(&wire);
        assert_eq!(wire_text(&entries[0], "name"), "scene_texture");
        assert_eq!(wire_text(&entries[0], "kind"), "sampled_texture");
        assert_eq!(
            wire_number(&entries[0], "stages"),
            0,
            "a declaration that names no stage must assert nothing about stages"
        );
        assert_eq!(wire_number(&entries[1], "stages"), 0b11);
    });
}

#[test]
fn a_binding_kind_the_pipeline_does_not_have_is_refused_naming_the_set() {
    Python::initialize();
    Python::attach(|python| {
        let declared = PyDict::new(python);
        declared.set_item("scene_texture", "sampled_image").unwrap();
        let refusal = declared_staged_kernel_bindings_to_wire(
            python,
            Some(&declared),
            "graphics binding kind",
            GRAPHICS_BINDING_KIND_WIRE_NAMES,
            "graphics stage",
            GRAPHICS_SHADER_STAGE_WIRE_BITS,
        )
        .expect_err("a graphics pipeline has no samplerless-texture descriptor");
        let refusal = refusal.to_string();
        assert!(refusal.contains("sampled_image"), "{refusal}");
        assert!(refusal.contains("sampled_texture"), "{refusal}");
    });
}

#[test]
fn a_stage_no_graphics_pipeline_runs_is_refused() {
    Python::initialize();
    Python::attach(|python| {
        let declared = PyDict::new(python);
        declared
            .set_item("output_image", ("storage_image", vec!["ray_gen"]))
            .unwrap();
        let refusal = declared_staged_kernel_bindings_to_wire(
            python,
            Some(&declared),
            "graphics binding kind",
            GRAPHICS_BINDING_KIND_WIRE_NAMES,
            "graphics stage",
            GRAPHICS_SHADER_STAGE_WIRE_BITS,
        )
        .expect_err("a graphics binding cannot be read from a ray-generation stage");
        assert!(refusal.to_string().contains("ray_gen"), "{refusal}");
    });
}

/// The wire has no way to omit a stage index, so a group that names none
/// carries the sentinel — which is the wheel's job, not the author's.
#[test]
fn a_shader_group_fills_the_stages_it_does_not_name_with_the_sentinel() {
    Python::initialize();
    Python::attach(|python| {
        let hit_group = PyDict::new(python);
        hit_group.set_item("kind", "triangles_hit").unwrap();
        hit_group.set_item("closest_hit_stage", 1u32).unwrap();
        let groups = PyList::new(python, [hit_group]).unwrap();

        let wire = ray_tracing_shader_groups_to_wire(python, groups.as_any(), 2).unwrap();
        let entries = wire_entries(&wire);
        assert_eq!(wire_number(&entries[0], "closest_hit_stage"), 1);
        assert_eq!(
            wire_number(&entries[0], "any_hit_stage"),
            RAY_TRACING_STAGE_INDEX_NONE
        );
        assert_eq!(
            wire_number(&entries[0], "general_stage"),
            RAY_TRACING_STAGE_INDEX_NONE
        );
    });
}

#[test]
fn a_shader_group_naming_a_module_that_was_not_supplied_is_refused() {
    Python::initialize();
    Python::attach(|python| {
        let group = PyDict::new(python);
        group.set_item("kind", "general").unwrap();
        group.set_item("general_stage", 4u32).unwrap();
        let groups = PyList::new(python, [group]).unwrap();

        let refusal = ray_tracing_shader_groups_to_wire(python, groups.as_any(), 2)
            .expect_err("a group cannot point past the modules it was built from");
        assert!(refusal.to_string().contains("general_stage 4"), "{refusal}");
    });
}

#[test]
fn a_general_shader_group_that_names_no_module_is_refused() {
    Python::initialize();
    Python::attach(|python| {
        let group = PyDict::new(python);
        group.set_item("kind", "general").unwrap();
        let groups = PyList::new(python, [group]).unwrap();

        let refusal = ray_tracing_shader_groups_to_wire(python, groups.as_any(), 2)
            .expect_err("a general group is the module it points at");
        assert!(refusal.to_string().contains("general_stage"), "{refusal}");
    });
}

#[test]
fn a_misspelled_group_key_is_refused_rather_than_silently_dropped() {
    Python::initialize();
    Python::attach(|python| {
        let group = PyDict::new(python);
        group.set_item("kind", "general").unwrap();
        group.set_item("general_stag", 0u32).unwrap();
        let groups = PyList::new(python, [group]).unwrap();

        let refusal = ray_tracing_shader_groups_to_wire(python, groups.as_any(), 1)
            .expect_err("a misspelled key would otherwise read as an absent one");
        assert!(refusal.to_string().contains("general_stag"), "{refusal}");
    });
}

/// An instance that names only its structure sits at the origin, visible
/// to every cull mask — the placement a caller means by saying nothing.
#[test]
fn a_tlas_instance_that_names_only_its_structure_gets_the_conventional_placement() {
    Python::initialize();
    Python::attach(|python| {
        let instance = PyDict::new(python);
        instance
            .set_item("blas", bottom_level_structure(python))
            .unwrap();
        let instances = PyList::new(python, [instance]).unwrap();

        let wire = tlas_instances_to_wire(python, instances.as_any()).unwrap();
        let entries = wire_entries(&wire);
        assert_eq!(wire_text(&entries[0], "blas_id"), "blas-under-test");
        assert_eq!(wire_number(&entries[0], "mask"), 0xff);
        assert_eq!(wire_number(&entries[0], "custom_index"), 0);
        assert_eq!(wire_number(&entries[0], "flags"), 0);
        let transform: Vec<f32> = entries[0].get_item("transform").unwrap().extract().unwrap();
        assert_eq!(transform, IDENTITY_TLAS_INSTANCE_TRANSFORM.to_vec());
    });
}

#[test]
fn a_tlas_instance_transform_that_is_not_a_three_by_four_affine_is_refused() {
    Python::initialize();
    Python::attach(|python| {
        let instance = PyDict::new(python);
        instance
            .set_item("blas", bottom_level_structure(python))
            .unwrap();
        instance.set_item("transform", vec![1.0f32; 16]).unwrap();
        let instances = PyList::new(python, [instance]).unwrap();

        let refusal = tlas_instances_to_wire(python, instances.as_any())
            .expect_err("a 4×4 transform is not what VkTransformMatrixKHR carries");
        assert!(refusal.to_string().contains("16 floats"), "{refusal}");
    });
}

/// The host masks the high byte off a custom index without saying so, so a
/// value that would arrive truncated is refused where it was written.
#[test]
fn a_tlas_instance_custom_index_wider_than_its_24_bits_is_refused() {
    Python::initialize();
    Python::attach(|python| {
        let instance = PyDict::new(python);
        instance
            .set_item("blas", bottom_level_structure(python))
            .unwrap();
        instance.set_item("custom_index", 0x0100_0000u32).unwrap();
        let instances = PyList::new(python, [instance]).unwrap();

        let refusal = tlas_instances_to_wire(python, instances.as_any())
            .expect_err("a 25-bit custom index cannot reach a hit shader intact");
        assert!(refusal.to_string().contains("truncated"), "{refusal}");
    });
}

#[test]
fn a_tlas_instance_naming_a_top_level_structure_is_refused() {
    Python::initialize();
    Python::attach(|python| {
        let top_level = Py::new(
            python,
            PythonAccelerationStructureHandle {
                acceleration_structure_id: "tlas-under-test".to_string(),
                is_top_level: true,
                structure_label: "scene".to_string(),
                helper_process_exchange_client: None,
            },
        )
        .unwrap();
        let instance = PyDict::new(python);
        instance.set_item("blas", top_level).unwrap();
        let instances = PyList::new(python, [instance]).unwrap();

        let refusal = tlas_instances_to_wire(python, instances.as_any())
            .expect_err("a scene cannot instance itself");
        assert!(refusal.to_string().contains("top-level"), "{refusal}");
    });
}

/// The other half of the same rule: a trace binds the top-level structure,
/// and the bottom-level one it was built from is not a scene.
#[test]
fn binding_a_bottom_level_structure_at_a_trace_is_refused() {
    Python::initialize();
    Python::attach(|python| {
        let bottom_level = bottom_level_structure(python);
        let refusal = bound_acceleration_structure_id("scene", bottom_level.bind(python).as_any())
            .expect_err("a trace binds the structure `build_tlas` returned");
        assert!(refusal.to_string().contains("bottom-level"), "{refusal}");
    });
}

#[test]
fn a_colour_write_mask_names_its_channels() {
    assert_eq!(color_write_channels_to_wire("rgba").unwrap(), 0b1111);
    assert_eq!(color_write_channels_to_wire("rg").unwrap(), 0b0011);
    assert_eq!(color_write_channels_to_wire("").unwrap(), 0);
    let refusal = color_write_channels_to_wire("rgbx")
        .expect_err("a colour write mask names only rgba channels");
    assert!(refusal.to_string().contains('x'), "{refusal}");
}
