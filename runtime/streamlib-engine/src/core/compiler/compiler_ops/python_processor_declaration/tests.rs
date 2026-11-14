// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn image_formats_spell_every_engine_texture_format() {
    let mut engine_texture_formats: Vec<&str> = (0..=u32::from(u8::MAX))
        .filter_map(crate::core::rhi::TextureFormat::from_repr_value)
        .map(|texture_format| texture_format.wire_name())
        .collect();
    engine_texture_formats.sort_unstable();
    let mut image_formats = crate::core::descriptors::IMAGE_TEXTURE_FORMAT_WIRE_NAMES.to_vec();
    image_formats.sort_unstable();
    assert_eq!(engine_texture_formats, image_formats);
}

fn requested(import_path: &str) -> ProcessorClassImportPath {
    ProcessorClassImportPath::new(import_path).expect("the test path names a class")
}

/// A described node type carrying what `@tatolab.stream.node.process` stamps,
/// with `input_ports` as given.
fn described_node_type_with_input_ports(input_ports: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "import_path": "my_app.audio:AudioConsumer",
        "short_name": "AudioConsumer",
        "description": "consumes audio",
        "kind": "process",
        "execution": {"mode": "continuous", "interval_ms": 20},
        "scheduling_priority": "high",
        "paced_by_real_time": false,
        "config_schema": {"type": "object"},
        "input_ports": input_ports,
        "output_ports": [{"name": "levels", "family": {"family": "channels"}, "description": "the levels it measured"}],
    })
}

fn described_node_type_with_output_ports(output_ports: serde_json::Value) -> serde_json::Value {
    let mut described = described_node_type_with_input_ports(serde_json::json!([]));
    described["output_ports"] = output_ports;
    described
}

fn read(described_node_type: &serde_json::Value) -> Result<PythonProcessorDeclaration, String> {
    PythonProcessorDeclaration::read_from_described_node_type(
        described_node_type,
        &requested("my_app.audio:AudioConsumer"),
    )
}

fn refusal_of(described_node_type: &serde_json::Value) -> String {
    match read(described_node_type) {
        Ok(_) => panic!("the described node type was accepted; a refusal was expected"),
        Err(refusal) => refusal,
    }
}

/// An audio window marker with `fields`, on a port declared `ordered`.
fn described_with_audio_window(fields: serde_json::Value) -> serde_json::Value {
    described_node_type_with_input_ports(serde_json::json!([{
        "name": "audio", "family": {"family": "channels"},
        "description": "",
        "delivery_profile": "ordered",
        "audio_window": fields,
    }]))
}

fn whole_declared_audio_window() -> serde_json::Map<String, serde_json::Value> {
    serde_json::json!({
        "resolved_from": "declaration",
        "sample_rate": 48_000,
        "channels": 2,
        "dtype": "f32",
        "window_size": 960,
        "hop": 960,
    })
    .as_object()
    .cloned()
    .expect("an object")
}

#[test]
fn a_described_process_reads_as_a_process_node_and_a_function_as_a_python_function_node() {
    let mut described = described_node_type_with_input_ports(serde_json::json!([]));
    let process = read(&described).expect("reads");
    assert_eq!(process.descriptor.node_kind, NodeKind::Process);
    assert!(matches!(
        process.how_a_python_node_of_the_type_runs,
        HowAPythonNodeOfTheTypeRuns::InItsOwnProcessorInterpreter(_)
    ));

    described["kind"] = serde_json::json!("function");
    described["language"] = serde_json::json!("python");
    described["state_fields"] = serde_json::json!([]);
    described["runs_as_python"] = serde_json::json!(false);
    for process_only_field in ["execution", "scheduling_priority", "paced_by_real_time"] {
        described
            .as_object_mut()
            .expect("a described node type is an object")
            .remove(process_only_field);
    }
    let function = read(&described).expect("reads");
    assert_eq!(
        function.descriptor.node_kind,
        NodeKind::Function {
            language: FunctionNodeLanguage::Python
        }
    );
    assert_eq!(
        function.how_a_python_node_of_the_type_runs,
        HowAPythonNodeOfTheTypeRuns::InsideTheReadOfEachNodeItFeeds
    );
}

#[test]
fn a_described_python_function_node_says_whether_its_body_runs_as_python() {
    let mut described = described_node_type_with_input_ports(serde_json::json!([]));
    described["kind"] = serde_json::json!("function");
    described["language"] = serde_json::json!("python");
    described["state_fields"] = serde_json::json!([]);
    described["runs_as_python"] = serde_json::json!(true);
    assert!(
        read(&described)
            .expect("reads")
            .descriptor
            .function_node_runs_as_python
    );
    described["runs_as_python"] = serde_json::json!("sometimes");
    assert!(refusal_of(&described).contains("runs_as_python"));
    described
        .as_object_mut()
        .expect("a described node type is an object")
        .remove("runs_as_python");
    assert!(refusal_of(&described).contains("runs_as_python"));
}

#[test]
fn a_described_function_nodes_state_fields_read_into_its_descriptor_up_to_what_an_entry_holds() {
    let mut described = described_node_type_with_input_ports(serde_json::json!([]));
    described["kind"] = serde_json::json!("function");
    described["language"] = serde_json::json!("python");
    described["state_fields"] = serde_json::json!(["phase", "cycles"]);
    described["runs_as_python"] = serde_json::json!(false);
    assert_eq!(
        read(&described)
            .expect("reads")
            .descriptor
            .function_node_state_fields,
        vec!["phase".to_string(), "cycles".to_string()]
    );

    let one_field_too_many: Vec<String> = (0
        ..=STATE_VALUES_ONE_FUNCTION_NODE_STATE_BOARD_ENTRY_HOLDS)
        .map(|field_index| format!("field_{field_index}"))
        .collect();
    described["state_fields"] = serde_json::json!(one_field_too_many);
    assert!(refusal_of(&described).contains("at most"));

    described
        .as_object_mut()
        .expect("an object")
        .remove("state_fields");
    assert!(refusal_of(&described).contains("state_fields"));
}

#[test]
fn a_described_rust_function_node_reads_as_a_function_node_run_inside_its_readers() {
    let mut described = described_node_type_with_input_ports(serde_json::json!([]));
    described["kind"] = serde_json::json!("function");
    described["language"] = serde_json::json!("rust");
    described["state_fields"] = serde_json::json!(["total"]);
    let function = read(&described).expect("reads");
    assert_eq!(
        function.descriptor.node_kind,
        NodeKind::Function {
            language: FunctionNodeLanguage::Rust
        }
    );
    assert_eq!(
        function.how_a_python_node_of_the_type_runs,
        HowAPythonNodeOfTheTypeRuns::InsideTheReadOfEachNodeItFeeds
    );
}

#[test]
fn a_described_shader_reads_as_a_shader_node_on_its_native_evaluator() {
    let mut described = described_node_type_with_input_ports(serde_json::json!([]));
    described["kind"] = serde_json::json!("shader");
    described["shader_source"] = serde_json::json!(
        "float4 tint(ImageInput image, float amount) { return image.pixel * amount; }"
    );
    described["shader_signature"] = serde_json::json!({
        "function_name": "tint",
        "inputs": [
            {"port": "image", "value_type": "image_frame"},
            {"port": "amount", "value_type": "float"},
        ],
        "output_port": "result",
    });
    let shader = read(&described).expect("reads");
    assert_eq!(shader.descriptor.node_kind, NodeKind::Shader);
    let HowAPythonNodeOfTheTypeRuns::OnItsNativeShaderEvaluator(program) =
        shader.how_a_python_node_of_the_type_runs
    else {
        panic!("a shader runs on its native evaluator");
    };
    assert_eq!(program.function_name, "tint");
    assert_eq!(program.output_port, "result");

    described
        .as_object_mut()
        .expect("an object")
        .remove("shader_source");
    assert!(refusal_of(&described).contains("shader_source"));
}

#[test]
fn a_kind_a_processor_interpreter_never_describes_is_refused_naming_it() {
    let mut described = described_node_type_with_input_ports(serde_json::json!([]));
    described["kind"] = serde_json::json!("function");
    described["language"] = serde_json::json!("cobol");
    assert!(refusal_of(&described).contains("\"cobol\""));

    described["kind"] = serde_json::json!("sorcery");
    assert!(refusal_of(&described).contains("\"sorcery\""));

    described.as_object_mut().expect("an object").remove("kind");
    assert!(refusal_of(&described).contains("kind"));
}

#[test]
fn a_node_paced_by_real_time_reads_into_its_descriptor() {
    let mut described = described_node_type_with_input_ports(serde_json::json!([]));
    assert!(
        !read(&described)
            .expect("reads")
            .descriptor
            .paced_by_real_time
    );
    described["paced_by_real_time"] = serde_json::json!(true);
    assert!(
        read(&described)
            .expect("reads")
            .descriptor
            .paced_by_real_time
    );
    described["paced_by_real_time"] = serde_json::json!("yes");
    assert!(read(&described).is_err());
}

#[test]
fn a_ports_family_reads_into_its_descriptor_and_a_key_its_family_does_not_take_is_refused() {
    let declaration = read(&described_node_type_with_input_ports(serde_json::json!([{
        "name": "signals",
        "description": "",
        "delivery_profile": "ordered",
        "family": {"family": "channels", "requires": ["rpm"]},
    }])))
    .unwrap_or_else(|refusal| panic!("a Channels input requiring a name reads: {refusal}"));
    let family = &declaration.descriptor.inputs[0].family;
    assert_eq!(
        family.family,
        crate::core::descriptors::PortFamily::Channels
    );
    assert_eq!(family.requires, vec!["rpm".to_string()]);

    let camera = read(&described_node_type_with_output_ports(serde_json::json!([{
            "name": "video",
            "description": "",
            "family": {"family": "image", "frame_rate": {"from_config": {"config_key": "frames_per_second"}}},
        }])))
        .unwrap_or_else(|refusal| panic!("an Image output declaring its frame rate reads: {refusal}"));
    assert_eq!(
        camera.descriptor.outputs[0].family.frame_rate,
        Some(
            crate::core::descriptors::ImageOutputPortFrameRate::FromConfig {
                config_key: "frames_per_second".into()
            }
        )
    );

    let refusal = refusal_of(&described_node_type_with_input_ports(serde_json::json!([{
        "name": "video",
        "description": "",
        "delivery_profile": "newest",
        "family": {"family": "image", "requires": ["luma"]},
    }])));
    assert!(
        refusal.contains("\"video\"") && refusal.contains("`requires=`"),
        "got {refusal}"
    );
}

#[test]
fn an_output_following_an_input_the_node_does_not_declare_is_refused_naming_both() {
    let refusal = refusal_of(&described_node_type_with_output_ports(serde_json::json!([{
        "name": "result",
        "description": "",
        "family": {"family": "image", "family_follows_input_port": "value"},
    }])));
    assert!(
        refusal.contains("\"result\"") && refusal.contains("input port \"value\""),
        "got {refusal}"
    );
}

#[test]
fn a_port_declaring_no_family_is_refused_naming_the_port() {
    let refusal = refusal_of(&described_node_type_with_input_ports(serde_json::json!([{
        "name": "frames",
        "description": "",
        "delivery_profile": "ordered",
    }])));
    assert!(
        refusal.contains("\"frames\" declares no family"),
        "got {refusal}"
    );
}

#[test]
fn a_declared_borrow_quota_reaches_the_descriptor_and_one_past_the_most_is_refused() {
    let declaration = read(&described_node_type_with_input_ports(serde_json::json!([{
        "name": "frames", "family": {"family": "records"},
        "description": "",
        "delivery_profile": "ordered",
        "borrow_quota": 5,
    }])))
    .unwrap_or_else(|refusal| panic!("a quota within the most reads: {refusal}"));
    assert_eq!(declaration.descriptor.inputs[0].borrow_quota, Some(5));

    let refusal = refusal_of(&described_node_type_with_input_ports(serde_json::json!([{
        "name": "frames", "family": {"family": "records"},
        "description": "",
        "delivery_profile": "ordered",
        "borrow_quota": MAX_INPUT_PORT_BORROW_QUOTA + 1,
    }])));
    assert!(
        refusal.contains("\"frames\"") && refusal.contains("borrow_quota"),
        "got {refusal}"
    );
}

#[test]
fn a_declared_pin_default_reaches_the_descriptor_and_one_off_a_channels_input_is_refused() {
    let declaration = read(&described_node_type_with_input_ports(serde_json::json!([
        {"name": "level", "family": {"family": "channels"}, "description": "",
         "delivery_profile": "newest", "pin_default": 0.5},
        {"name": "maybe", "family": {"family": "records"}, "description": "",
         "delivery_profile": "newest", "pin_default": null},
    ])))
    .unwrap_or_else(|refusal| panic!("a number and a null default read: {refusal}"));
    assert_eq!(
        declaration.descriptor.inputs[0].pin_default,
        Some(serde_json::json!(0.5))
    );
    assert_eq!(
        declaration.descriptor.inputs[1].pin_default,
        Some(serde_json::Value::Null)
    );

    let refusal = refusal_of(&described_node_type_with_input_ports(serde_json::json!([{
        "name": "frames", "family": {"family": "records"}, "description": "",
        "delivery_profile": "newest", "pin_default": 1,
    }])));
    assert!(
        refusal.contains("\"frames\"") && refusal.contains("Channels"),
        "got {refusal}"
    );
    let refusal = refusal_of(&described_node_type_with_input_ports(serde_json::json!([{
        "name": "level", "family": {"family": "channels"}, "description": "",
        "delivery_profile": "newest", "pin_default": "loud",
    }])));
    assert!(
        refusal.contains("\"level\"") && refusal.contains("a number or a bool"),
        "got {refusal}"
    );
}

#[test]
fn a_declared_category_reaches_the_catalog_and_one_that_is_no_string_is_refused() {
    let mut described_node_type = described_node_type_with_input_ports(serde_json::json!([]));
    assert_eq!(
        read(&described_node_type).unwrap().descriptor.category,
        None
    );

    described_node_type["category"] = serde_json::json!("audio meters");
    let descriptor = read(&described_node_type).unwrap().descriptor;
    assert_eq!(descriptor.category.as_deref(), Some("audio meters"));
    assert_eq!(
        serde_json::to_value(crate::core::json_schema::ProcessorDescriptorOutput::from(
            &descriptor
        ))
        .unwrap()["category"],
        "audio meters"
    );

    for not_a_category in [serde_json::json!(7), serde_json::json!("  ")] {
        described_node_type["category"] = not_a_category;
        let refusal = refusal_of(&described_node_type);
        assert!(refusal.contains("category"), "got {refusal}");
    }
}

#[test]
fn a_described_node_type_reads_into_a_python_descriptor_and_its_execution() {
    let declaration = read(&described_node_type_with_input_ports(serde_json::json!([
            {"name": "audio", "family": {"family": "channels"}, "description": "what it hears", "delivery_profile": "ordered"}
        ])))
        .unwrap_or_else(|refusal| panic!("the described node type reads: {refusal}"));

    let descriptor = &declaration.descriptor;
    assert_eq!(
        descriptor.processor_class_import_path.as_str(),
        "my_app.audio:AudioConsumer"
    );
    assert_eq!(
        descriptor.entrypoint.as_deref(),
        Some("my_app.audio:AudioConsumer")
    );
    assert_eq!(
        descriptor.processor_class_short_name.as_str(),
        "AudioConsumer"
    );
    assert_eq!(descriptor.runtime, ProcessorRuntime::Python);
    assert_eq!(descriptor.description, "consumes audio");
    assert_eq!(descriptor.scheduling.priority, ThreadPriority::High);
    assert_eq!(
        descriptor.config_schema,
        Some(serde_json::json!({"type": "object"}))
    );
    assert_eq!(descriptor.inputs[0].name, "audio");
    assert_eq!(
        descriptor.inputs[0].delivery_profile.as_deref(),
        Some("ordered")
    );
    assert_eq!(descriptor.outputs[0].name, "levels");
    assert_eq!(
        declaration.how_a_python_node_of_the_type_runs,
        HowAPythonNodeOfTheTypeRuns::InItsOwnProcessorInterpreter(ExecutionConfig {
            execution: ProcessExecution::Continuous { interval_ms: 20 },
        })
    );
}

/// The interpreter answers for the path it was asked; a document naming
/// another class is refused rather than registered under the wrong name.
#[test]
fn a_described_node_type_naming_another_import_path_is_refused_naming_both() {
    let mut described = described_node_type_with_input_ports(serde_json::json!([]));
    described["import_path"] = serde_json::json!("my_app.audio:SomethingElse");

    let refusal = refusal_of(&described);

    assert!(
        refusal.contains("my_app.audio:SomethingElse")
            && refusal.contains("my_app.audio:AudioConsumer"),
        "{refusal}"
    );
}

#[test]
fn an_unknown_execution_mode_is_refused_naming_it() {
    let mut described = described_node_type_with_input_ports(serde_json::json!([]));
    described["execution"] = serde_json::json!({"mode": "eventually"});

    assert!(refusal_of(&described).contains("\"eventually\""));
}

#[test]
fn a_config_schema_that_is_not_an_object_is_refused() {
    let mut described = described_node_type_with_input_ports(serde_json::json!([]));
    described["config_schema"] = serde_json::json!(["not", "an", "object"]);

    assert!(refusal_of(&described).contains("config_schema must be a JSON object"));
}

/// The decorator refuses the sentinel; this reader does not, and must not.
/// A marker the decorator never built still carries `match_device` through
/// to the compiler, where the wire-time refusal — which knows the port's
/// placement, as nothing here does — is the guard that speaks.
#[test]
fn a_hand_built_match_device_marker_still_reaches_the_descriptor() {
    let declaration = read(&described_with_audio_window(
        serde_json::json!({"resolved_from": "match_device"}),
    ))
    .unwrap_or_else(|refusal| panic!("a hand-built sentinel reads: {refusal}"));

    assert_eq!(
        declaration.descriptor.inputs[0].audio_window,
        Some(AudioWindowContract::MatchDevice {})
    );
}

#[test]
fn a_hand_built_marker_smuggling_an_unhonourable_contract_is_refused_naming_both_numbers() {
    let mut fields = whole_declared_audio_window();
    fields.insert("window_size".to_string(), serde_json::json!(512));
    fields.insert("hop".to_string(), serde_json::json!(4096));

    let refusal = refusal_of(&described_with_audio_window(fields.into()));

    assert!(
        refusal.contains("4096") && refusal.contains("512"),
        "{refusal}"
    );
}

#[test]
fn a_contract_beside_a_skipping_profile_is_refused_naming_both_knobs() {
    let described = described_node_type_with_input_ports(serde_json::json!([{
        "name": "audio", "family": {"family": "channels"},
        "description": "",
        "delivery_profile": "newest",
        "audio_window": serde_json::Value::Object(whole_declared_audio_window()),
    }]));

    let refusal = refusal_of(&described);

    assert!(
        refusal.contains("audio_window")
            && refusal.contains("newest")
            && refusal.contains("ordered"),
        "{refusal}"
    );
}

#[test]
fn a_negative_count_is_refused_naming_the_field_and_the_value() {
    let mut fields = whole_declared_audio_window();
    fields.insert("sample_rate".to_string(), serde_json::json!(-1));

    let refusal = refusal_of(&described_with_audio_window(fields.into()));

    assert!(
        refusal.contains("sample_rate") && refusal.contains("-1"),
        "{refusal}"
    );
}

/// A Python `bool` is an `int` subclass, so a marker carrying `True` would
/// otherwise reach the stage as one channel — a plausible count nobody
/// wrote.
#[test]
fn a_bool_where_a_number_belongs_is_refused_naming_the_field_and_the_kind() {
    for field in ["channels", "sample_rate", "window_size", "hop"] {
        let mut fields = whole_declared_audio_window();
        fields.insert(field.to_string(), serde_json::json!(true));

        let refusal = refusal_of(&described_with_audio_window(fields.into()));

        assert!(
            refusal.contains(field) && refusal.contains("bool"),
            "a bool in {field:?} must be refused naming the field and the kind; got {refusal}"
        );
    }
}

/// The count is the one value a marker may leave out, and the reader
/// carries the omission through: a port that follows its source is
/// spelled by saying nothing, or by the `"source"` word.
#[test]
fn an_omitted_or_source_spelled_channel_count_follows_the_source() {
    let mut omitted = whole_declared_audio_window();
    omitted.remove("channels");
    let mut spelled = whole_declared_audio_window();
    spelled.insert("channels".to_string(), serde_json::json!("source"));

    for fields in [omitted, spelled] {
        let declaration = read(&described_with_audio_window(fields.into()))
            .unwrap_or_else(|refusal| panic!("an omitted count is a whole contract: {refusal}"));

        assert_eq!(
            declaration.descriptor.inputs[0].audio_window,
            Some(AudioWindowContract::Declaration(
                AudioWindowContractDeclaredValues {
                    sample_rate: 48_000,
                    channels: None,
                    dtype: "f32".to_string(),
                    window_size: 960,
                    hop: 960,
                }
            ))
        );
    }
}

#[test]
fn a_channel_count_naming_no_count_is_refused_offering_the_spelling() {
    let mut fields = whole_declared_audio_window();
    fields.insert("channels".to_string(), serde_json::json!("stereo"));

    let refusal = refusal_of(&described_with_audio_window(fields.into()));

    assert!(
        refusal.contains("channels") && refusal.contains("source"),
        "{refusal}"
    );
}

/// Every field the contract requires names the port and the field when it
/// is missing; `channels` is the one a port may leave to its source.
#[test]
fn a_marker_missing_any_required_contract_field_is_refused_naming_the_port_and_the_field() {
    for missing_field in [
        "resolved_from",
        "sample_rate",
        "dtype",
        "window_size",
        "hop",
    ] {
        let mut fields = whole_declared_audio_window();
        fields.remove(missing_field);

        let refusal = refusal_of(&described_with_audio_window(fields.into()));

        assert!(
            refusal.contains("input port \"audio\"") && refusal.contains(missing_field),
            "a missing {missing_field:?} must name the port and the field; got {refusal}"
        );
    }
}

/// An output port declares no contract — only a consuming input port
/// states the window it needs.
#[test]
fn an_output_port_declaring_a_contract_is_refused() {
    let mut described = described_node_type_with_input_ports(serde_json::json!([]));
    described["output_ports"] = serde_json::json!([{
        "name": "windows", "family": {"family": "channels"},
        "description": "",
        "audio_window": {"resolved_from": "match_device"},
    }]);

    let refusal = refusal_of(&described);

    assert!(
        refusal.contains("output port \"windows\"") && refusal.contains("consuming"),
        "{refusal}"
    );
}

/// The kind of a channel-count refusal travels with it, so a language host
/// raises the same mistake as the same exception on every field.
#[test]
fn a_channel_count_refusal_says_whether_the_value_was_the_wrong_kind_or_unusable() {
    assert_eq!(
        read_a_channel_count_or_the_source_spelling(&serde_json::json!(2)),
        Ok(Some(2))
    );
    assert!(matches!(
        read_a_channel_count_or_the_source_spelling(&serde_json::json!(1.5)),
        Err(AudioWindowFieldRefusal::WrongKindOfValue(_))
    ));
    assert!(matches!(
        read_a_channel_count_or_the_source_spelling(&serde_json::json!(-2)),
        Err(AudioWindowFieldRefusal::UnusableValue(_))
    ));
    assert!(matches!(
        read_a_channel_count_or_the_source_spelling(&serde_json::json!("stereo")),
        Err(AudioWindowFieldRefusal::UnusableValue(_))
    ));
}
