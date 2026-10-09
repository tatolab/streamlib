// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A port a Rust processor declares is registered under its cast, the way
//! `@node` declares a Python port, and `connect` reaches it by the spelling the
//! author declared or by the cast.

#![deny(non_camel_case_types)]

use serial_test::serial;
use streamlib::sdk::descriptors::{
    PortDescriptor, ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor,
};
use streamlib::sdk::error::Error;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
use streamlib::sdk::runtime::Runner;
use streamlib_engine::core::{OutputPortMarker, Result, RuntimeContextFullAccess};

/// An empty stream loaded into `runner`, its project in `project_directory`.
fn an_empty_stream_loaded_into(
    runner: &Runner,
    project_directory: &std::path::Path,
) -> std::sync::Arc<streamlib::sdk::runtime::LoadedStreamInThisRuntime> {
    runner
        .load_an_empty_stream(
            streamlib::sdk::runtime::OptionsForLoadingOneStream::in_project_directory(
                project_directory,
            )
            .named("main"),
        )
        .expect("an empty stream loads")
}

#[streamlib::sdk::processor(
    execution = manual,
    output("videoOut"),
)]
pub struct CamelCaseVideoSource;

impl streamlib_engine::ManualProcessor for CamelCaseVideoSource::Processor {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

fn camel_case_video_source_registered() -> ProcessorClassImportPath {
    PROCESSOR_REGISTRY.register::<CamelCaseVideoSource::Processor>();
    CamelCaseVideoSource::processor_class_import_path()
}

/// A descriptor-only sink with one input, `video_in`. Idempotent under
/// `serial_test`.
fn video_sink_registered() -> ProcessorClassImportPath {
    let import_path =
        ProcessorClassImportPath::new(format!("{}::VideoSink", module_path!())).unwrap();
    let descriptor = ProcessorDescriptor::new(
        ProcessorClassShortName::new("VideoSink").unwrap(),
        import_path.clone(),
        "rust port name cast test",
    )
    .with_input(PortDescriptor::new("video_in", "", false));
    match PROCESSOR_REGISTRY.register_descriptor_only(descriptor) {
        // Registered by an earlier test in this binary.
        Ok(()) | Err(Error::Configuration(_)) => {}
        Err(refusal) => panic!("the sink fixture is refused: {refusal}"),
    }
    import_path
}

#[test]
#[serial]
fn a_camel_case_rust_port_registers_its_cast_and_its_marker_carries_it() {
    let source = camel_case_video_source_registered();

    let descriptor = PROCESSOR_REGISTRY
        .descriptor(&source)
        .expect("the macro-declared processor registers");
    let output_port_names: Vec<&str> = descriptor
        .outputs
        .iter()
        .map(|port| port.name.as_str())
        .collect();
    assert_eq!(output_port_names, ["videoout"]);
    assert_eq!(
        <CamelCaseVideoSource::OutputLink::videoOut as OutputPortMarker>::PORT_NAME,
        "videoout"
    );
}

#[test]
#[serial]
fn connect_reaches_a_camel_case_rust_port_by_its_spelling_or_its_cast() {
    let source = camel_case_video_source_registered();
    let sink = video_sink_registered();

    for port_spelling in ["videoOut", "videoout"] {
        let project_directory = tempfile::tempdir().expect("a project directory");
        let runner = Runner::new().unwrap();
        let stream = an_empty_stream_loaded_into(&runner, project_directory.path());
        let source_id = stream
            .add_processor(ProcessorSpec::new(source.clone(), serde_json::json!({})))
            .unwrap();
        let sink_id = stream
            .add_processor(ProcessorSpec::new(sink.clone(), serde_json::json!({})))
            .unwrap();

        stream
            .connect(
                OutputLinkPortRef::new(&source_id, port_spelling),
                InputLinkPortRef::new(&sink_id, "video_in"),
            )
            .unwrap_or_else(|refusal| panic!("{port_spelling:?} names the port: {refusal}"));

        let graph_document = stream.to_json().expect("the graph renders");
        assert_eq!(
            graph_document["links"][0]["source"]["port"], "videoout",
            "the link holds the cast whichever spelling connected it"
        );
    }
}

#[test]
#[serial]
fn a_hand_built_descriptor_with_a_port_name_not_cast_is_refused_by_name() {
    let import_path =
        ProcessorClassImportPath::new(format!("{}::HandBuiltSource", module_path!())).unwrap();
    let descriptor = ProcessorDescriptor::new(
        ProcessorClassShortName::new("HandBuiltSource").unwrap(),
        import_path.clone(),
        "rust port name cast test",
    )
    .with_output(PortDescriptor::new("videoOut", "", false));

    match PROCESSOR_REGISTRY.register_descriptor_only(descriptor) {
        Err(Error::DescriptorPortNameNotCast {
            processor_class_import_path,
            port_name,
        }) => {
            assert_eq!(processor_class_import_path, import_path);
            assert_eq!(port_name, "videoOut");
        }
        other => panic!("expected DescriptorPortNameNotCast, got {other:?}"),
    }
    assert!(PROCESSOR_REGISTRY.descriptor(&import_path).is_none());
}
