// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::core::compiler::compiler_ops::processor_interpreter_describe::{
    a_processor_interpreter_describes, describe_and_register_node_types_in_a_processor_interpreter,
};
use crate::core::compiler::compiler_ops::processor_interpreter_spawn_host::LoadedStreamAHelperProcessBelongsTo;
use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::error::{Error, Result};
use crate::core::processors::NodeTypesOneStreamResolves;
use crate::core::pubsub::{LoadedStreamIdentity, RuntimeEvent};

use super::StreamEnvironment;

/// What one loaded stream starts its processor interpreters with: its stream
/// environment, the node types described into it, the tag and shutdown its
/// helper processes belong to, and whether its host interrupted its describes.
pub(crate) struct ProcessorInterpreterLaunchRecordOfOneStream {
    stream_identity: LoadedStreamIdentity,
    stream_environment: Option<StreamEnvironment>,
    node_types_this_stream_resolves: Arc<NodeTypesOneStreamResolves>,
    the_stream_its_helpers_belong_to: LoadedStreamAHelperProcessBelongsTo,
    describes_interrupted_by_the_host: AtomicBool,
}

impl ProcessorInterpreterLaunchRecordOfOneStream {
    pub(crate) fn new(
        stream_identity: LoadedStreamIdentity,
        stream_environment: Option<StreamEnvironment>,
        node_types_this_stream_resolves: Arc<NodeTypesOneStreamResolves>,
        the_stream_its_helpers_belong_to: LoadedStreamAHelperProcessBelongsTo,
    ) -> Self {
        Self {
            stream_identity,
            stream_environment,
            node_types_this_stream_resolves,
            the_stream_its_helpers_belong_to,
            describes_interrupted_by_the_host: AtomicBool::new(false),
        }
    }

    pub(crate) fn stream_environment(&self) -> Option<&StreamEnvironment> {
        self.stream_environment.as_ref()
    }

    pub(crate) fn interrupt_every_describe(&self) {
        self.describes_interrupted_by_the_host
            .store(true, Ordering::SeqCst);
    }

    /// Describe, in one interpreter start, every type in `node_types` a
    /// processor interpreter describes and no natively compiled type holds, and
    /// register each into this stream.
    pub(crate) fn describe_and_register_every_type_a_load_names<'a>(
        &self,
        node_types: impl IntoIterator<Item = &'a ProcessorClassImportPath>,
        lend_directory: Option<&Path>,
    ) -> Result<()> {
        let mut node_types_to_describe: Vec<ProcessorClassImportPath> = Vec::new();
        for node_type in node_types {
            if a_processor_interpreter_describes(node_type)
                && !self
                    .node_types_this_stream_resolves
                    .is_a_natively_compiled_node_type(node_type)
                && !node_types_to_describe.contains(node_type)
            {
                node_types_to_describe.push(node_type.clone());
            }
        }
        self.describe_and_register(&node_types_to_describe, lend_directory)
    }

    /// Whether a live add of `node_type` has to describe it first: a type a
    /// processor interpreter describes that this stream does not resolve yet.
    pub(crate) fn a_live_add_must_describe(&self, node_type: &ProcessorClassImportPath) -> bool {
        a_processor_interpreter_describes(node_type)
            && self
                .node_types_this_stream_resolves
                .descriptor(node_type)
                .is_none()
    }

    /// Describe and register `node_type` into this stream for a live add.
    pub(crate) fn describe_and_register_a_type_a_live_add_names(
        &self,
        node_type: &ProcessorClassImportPath,
        lend_directory: Option<&Path>,
    ) -> Result<()> {
        self.describe_and_register(std::slice::from_ref(node_type), lend_directory)
    }

    fn describe_and_register(
        &self,
        node_types: &[ProcessorClassImportPath],
        lend_directory: Option<&Path>,
    ) -> Result<()> {
        if node_types.is_empty() {
            return Ok(());
        }
        let Some(stream_environment) = self.stream_environment.as_ref() else {
            return Err(Error::NodeTypesNotDescribed {
                node_types: node_types.to_vec(),
                refusal: "a type that is not a built-in is described in the stream's own \
                          interpreter, and this stream was loaded with no stream environment — \
                          load the stream with its project directory and interpreter"
                    .to_string(),
            });
        };
        let Some(lend_directory) = lend_directory else {
            return Err(Error::NodeTypesNotDescribed {
                node_types: node_types.to_vec(),
                refusal: "a type that is not a built-in is described in the stream's own \
                          interpreter, and this runtime's host gave it no lend directory to \
                          start one from"
                    .to_string(),
            });
        };
        describe_and_register_node_types_in_a_processor_interpreter(
            &self.node_types_this_stream_resolves,
            node_types,
            stream_environment,
            lend_directory,
            &self.the_stream_its_helpers_belong_to,
            &|| {
                self.describes_interrupted_by_the_host
                    .load(Ordering::SeqCst)
            },
        )?;
        for node_type in node_types {
            self.stream_identity.publish_on_this_streams_topic(
                RuntimeEvent::RuntimeDidRegisterProcessorType {
                    processor_type: node_type.clone(),
                },
            );
        }
        Ok(())
    }
}

/// The lend directory a runtime's host handed it, read by every stream's
/// describes and processor interpreters.
#[derive(Default)]
pub(crate) struct ProcessorInterpreterLendDirectoryOfTheEngine(std::sync::OnceLock<PathBuf>);

impl ProcessorInterpreterLendDirectoryOfTheEngine {
    /// Hold `lend_directory`, refusing a second one: a stream already loaded
    /// borrows the first.
    pub(crate) fn set(&self, lend_directory: PathBuf) -> Result<()> {
        self.0
            .set(lend_directory)
            .map_err(|refused_lend_directory| {
                Error::Runtime(format!(
                    "this runtime was already handed the lend directory {}, so {} was refused",
                    self.0
                        .get()
                        .map(|held| held.display().to_string())
                        .unwrap_or_default(),
                    refused_lend_directory.display()
                ))
            })
    }

    pub(crate) fn get(&self) -> Option<&Path> {
        self.0.get().map(PathBuf::as_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::descriptors::{PortDescriptor, ProcessorClassShortName, ProcessorDescriptor};
    use crate::core::graph_snapshot::GraphSnapshot;
    use crate::core::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
    use crate::core::runtime::{
        LoadedStreamInThisRuntime, OptionsForLoadingOneStream, Runner,
        StreamLoadObservingMachineShutdownRequests, TheMachinesShutdownEscalationClearedOnDrop,
    };
    use serial_test::serial;

    #[test]
    fn a_second_lend_directory_is_refused_and_the_first_kept() {
        let lend_directory_of_the_engine = ProcessorInterpreterLendDirectoryOfTheEngine::default();
        lend_directory_of_the_engine
            .set("/first/lend".into())
            .expect("the first lend directory");

        let refusal = lend_directory_of_the_engine
            .set("/second/lend".into())
            .expect_err("a second lend directory is refused");

        assert!(refusal.to_string().contains("/first/lend"), "{refusal}");
        assert!(refusal.to_string().contains("/second/lend"), "{refusal}");
        assert_eq!(
            lend_directory_of_the_engine.get(),
            Some(Path::new("/first/lend"))
        );
    }

    const LEND_DIRECTORY_FOR_TEST: &str = "/opt/tatolab/lib/tatolab/lend";

    fn import_path(path: &str) -> ProcessorClassImportPath {
        ProcessorClassImportPath::new(path).expect("the test path names a class")
    }

    /// A Rust-grammar type registered by its descriptor alone, which no load
    /// describes.
    fn a_rust_type_registered_as(short_name: &str) -> ProcessorClassImportPath {
        let rust_type = import_path(&format!("{}::{short_name}", module_path!()));
        let _ = PROCESSOR_REGISTRY.register_descriptor_only(
            ProcessorDescriptor::new(
                ProcessorClassShortName::new(short_name).unwrap(),
                rust_type.clone(),
                "a launch-record test double",
            )
            .with_output(PortDescriptor::new("video", "", false)),
        );
        rust_type
    }

    fn a_graph_of(node_types: &[&ProcessorClassImportPath]) -> GraphSnapshot {
        let nodes: Vec<_> = node_types
            .iter()
            .enumerate()
            .map(|(node_index, node_type)| {
                serde_json::json!({"name": format!("node-{node_index}"), "type": node_type.as_str()})
            })
            .collect();
        GraphSnapshot::from_graph_document(serde_json::json!({ "nodes": nodes }))
            .expect("the test graph reads")
    }

    /// A stream named `main` whose project is `project_directory` and which
    /// starts no processor interpreter.
    fn a_stream_with_no_environment(project_directory: &Path) -> OptionsForLoadingOneStream {
        OptionsForLoadingOneStream::in_project_directory(project_directory).named("main")
    }

    /// A project directory holding a stand-in venv interpreter that answers
    /// every describe with `described_node_types`.
    fn a_project_whose_interpreter_describes(
        described_node_types: serde_json::Value,
    ) -> (tempfile::TempDir, StreamEnvironment) {
        let project_directory = tempfile::tempdir().expect("a project directory");
        let interpreter = project_directory.path().join("stub-python");
        let describe_document = serde_json::json!({
            "described_node_types": described_node_types,
            "refused_node_types": [],
        });
        crate::core::test_support::write_an_executable_script_from_a_child_process(
            &interpreter,
            &format!("#!/bin/sh\ncat <<'DESCRIBED'\n{describe_document}\nDESCRIBED\n"),
        );
        let stream_environment = StreamEnvironment {
            project_directory: project_directory.path().to_path_buf(),
            interpreter,
        };
        (project_directory, stream_environment)
    }

    /// A project directory holding a stand-in interpreter that hangs at import.
    fn a_project_whose_interpreter_hangs() -> (tempfile::TempDir, StreamEnvironment) {
        let project_directory = tempfile::tempdir().expect("a project directory");
        let interpreter = project_directory.path().join("stub-python");
        crate::core::test_support::write_an_executable_script_from_a_child_process(
            &interpreter,
            "#!/bin/sh\nsleep 30 &\nwait\n",
        );
        let stream_environment = StreamEnvironment {
            project_directory: project_directory.path().to_path_buf(),
            interpreter,
        };
        (project_directory, stream_environment)
    }

    fn a_described_type(
        node_type: &ProcessorClassImportPath,
        input_port: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "import_path": node_type.as_str(),
            "short_name": "Blur",
            "description": "blurs",
            "execution": {"mode": "reactive"},
            "scheduling_priority": null,
            "config_schema": {"type": "object"},
            "input_ports": [{"name": input_port, "description": ""}],
            "output_ports": [],
        })
    }

    fn input_port_names_the_stream_resolves_for(
        stream: &LoadedStreamInThisRuntime,
        node_type: &ProcessorClassImportPath,
    ) -> Vec<String> {
        stream
            .node_types_this_stream_resolves()
            .descriptor(node_type)
            .expect("the stream resolves the type")
            .inputs
            .into_iter()
            .map(|port| port.name)
            .collect()
    }

    fn the_refusal_of(
        load: Result<Arc<LoadedStreamInThisRuntime>>,
        why_it_is_refused: &str,
    ) -> Error {
        match load {
            Ok(stream) => panic!(
                "the stream `{}` loaded, and {why_it_is_refused}",
                stream.stream_name()
            ),
            Err(refusal) => refusal,
        }
    }

    fn refused_node_types(refusal: Error) -> (Vec<ProcessorClassImportPath>, String) {
        match refusal {
            Error::NodeTypesNotDescribed {
                node_types,
                refusal,
            } => (node_types, refusal),
            other => panic!("expected NodeTypesNotDescribed, got {other:?}"),
        }
    }

    #[test]
    #[serial]
    fn a_loaded_stream_keeps_the_stream_environment_it_was_given() {
        let rust_type = a_rust_type_registered_as("RecordedEnvironmentSource");
        let project_directory = tempfile::tempdir().expect("a project directory");
        let stream_environment = StreamEnvironment {
            project_directory: project_directory.path().to_path_buf(),
            interpreter: project_directory.path().join(".venv/bin/python"),
        };
        let project_directory_of_the_stream_with_no_environment =
            tempfile::tempdir().expect("a project directory");
        let runner = Runner::new().unwrap();

        let stream = runner
            .load_stream_from_graph_snapshot(
                &a_graph_of(&[&rust_type]),
                OptionsForLoadingOneStream::in_stream_environment(stream_environment.clone())
                    .named("main"),
            )
            .expect("a graph of Rust types loads");

        assert_eq!(stream.stream_environment(), Some(&stream_environment));
        assert_eq!(
            stream.project_directory(),
            stream_environment.project_directory
        );
        let stream_with_no_environment = runner
            .load_an_empty_stream(
                OptionsForLoadingOneStream::in_project_directory(
                    project_directory_of_the_stream_with_no_environment.path(),
                )
                .named("no-environment"),
            )
            .expect("an empty stream loads");
        assert_eq!(stream_with_no_environment.stream_environment(), None);
    }

    /// A Rust type is never described, so a graph naming only Rust types
    /// loads with no environment and starts no interpreter.
    #[test]
    #[serial]
    fn a_graph_of_rust_types_needs_no_stream_environment() {
        let rust_type = a_rust_type_registered_as("NoEnvironmentSource");
        let project_directory = tempfile::tempdir().expect("a project directory");

        Runner::new()
            .unwrap()
            .load_stream_from_graph_snapshot(
                &a_graph_of(&[&rust_type]),
                a_stream_with_no_environment(project_directory.path()),
            )
            .expect("a graph of Rust types loads with no environment");
    }

    #[test]
    #[serial]
    fn a_load_naming_a_type_to_describe_with_no_environment_refuses_it_by_name() {
        let rust_type = a_rust_type_registered_as("BesideAnUndescribedType");
        let python_type = import_path("my_app.load_with_no_environment:Blur");
        let project_directory = tempfile::tempdir().expect("a project directory");
        let runner = Runner::new().unwrap();

        let refusal = the_refusal_of(
            runner.load_stream_from_graph_snapshot(
                &a_graph_of(&[&rust_type, &python_type]),
                a_stream_with_no_environment(project_directory.path()),
            ),
            "a type to describe needs an environment",
        );

        let (node_types, refusal) = refused_node_types(refusal);
        assert_eq!(node_types, [python_type]);
        assert!(refusal.contains("no stream environment"), "{refusal}");
        assert!(
            runner.names_of_the_loaded_streams().is_empty(),
            "a refused load adds no stream"
        );
    }

    /// A host whose user interrupts a load mid-describe — a module that hangs
    /// at import — with a machine shutdown gets the load back long before the
    /// describe's bound, and the stream is never loaded.
    #[test]
    #[serial]
    fn a_load_a_machine_shutdown_interrupts_mid_describe_returns_and_loads_nothing() {
        let _machine_level_cleared =
            TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop();
        let python_type = import_path("my_app.hangs_at_import:Blur");
        let (_project_directory, stream_environment) = a_project_whose_interpreter_hangs();
        let runner = Runner::new().unwrap();
        runner
            .set_processor_interpreter_lend_directory(LEND_DIRECTORY_FOR_TEST.into())
            .expect("the runner's first lend directory");
        let started = std::time::Instant::now();

        let interrupter = std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(300));
            crate::core::runtime::request_the_shutdown_of_every_loaded_stream(
                "the test interrupts the load",
            )
            .expect("the machine's shutdown is requested");
        });
        let load_outcome = runner
            .load_stream_from_graph_snapshot_unless_a_machine_shutdown_is_requested(
                &a_graph_of(&[&python_type]),
                OptionsForLoadingOneStream::in_stream_environment(stream_environment).named("main"),
            )
            .expect("a load a machine shutdown cut short is abandoned without a refusal");
        interrupter.join().unwrap();

        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the load held its host's interrupt for the describe's whole bound"
        );
        assert!(matches!(
            load_outcome,
            StreamLoadObservingMachineShutdownRequests::AbandonedForAMachineShutdownRequest
        ));
        assert!(runner.names_of_the_loaded_streams().is_empty());
    }

    /// An interrupt belongs to the stream whose load it cut short: the next
    /// load into the same runtime describes as if none had happened.
    #[test]
    #[serial]
    fn a_load_after_one_a_machine_shutdown_interrupted_describes_normally() {
        let interrupted_type = import_path("my_app.hangs_before_a_later_load:Blur");
        let later_type = import_path("my_app.described_after_an_interrupted_load:Blur");
        let (_hanging_project, hanging_environment) = a_project_whose_interpreter_hangs();
        let runner = Runner::new().unwrap();
        runner
            .set_processor_interpreter_lend_directory(LEND_DIRECTORY_FOR_TEST.into())
            .expect("the runner's first lend directory");
        {
            let _machine_level_cleared =
                TheMachinesShutdownEscalationClearedOnDrop::clear_now_and_on_drop();
            let interrupter = std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(300));
                crate::core::runtime::request_the_shutdown_of_every_loaded_stream(
                    "the test interrupts the load",
                )
                .expect("the machine's shutdown is requested");
            });
            let interrupted = runner
                .load_stream_from_graph_snapshot_unless_a_machine_shutdown_is_requested(
                    &a_graph_of(&[&interrupted_type]),
                    OptionsForLoadingOneStream::in_stream_environment(hanging_environment)
                        .named("main"),
                )
                .expect("the interrupted load is abandoned");
            interrupter.join().unwrap();
            assert!(matches!(
                interrupted,
                StreamLoadObservingMachineShutdownRequests::AbandonedForAMachineShutdownRequest
            ));
        }

        let (_later_project, later_environment) = a_project_whose_interpreter_describes(
            serde_json::json!([a_described_type(&later_type, "video")]),
        );
        let stream = runner
            .load_stream_from_graph_snapshot(
                &a_graph_of(&[&later_type]),
                OptionsForLoadingOneStream::in_stream_environment(later_environment).named("main"),
            )
            .expect("a later load describes its types");

        assert_eq!(
            input_port_names_the_stream_resolves_for(&stream, &later_type),
            ["video"]
        );
    }

    #[test]
    #[serial]
    fn a_load_with_no_lend_directory_refuses_a_type_to_describe_by_name() {
        let python_type = import_path("my_app.load_with_no_lend_directory:Blur");
        let (_project_directory, stream_environment) =
            a_project_whose_interpreter_describes(serde_json::json!([]));

        let refusal = the_refusal_of(
            Runner::new().unwrap().load_stream_from_graph_snapshot(
                &a_graph_of(&[&python_type]),
                OptionsForLoadingOneStream::in_stream_environment(stream_environment).named("main"),
            ),
            "a type to describe needs a lend directory",
        );

        let (node_types, refusal) = refused_node_types(refusal);
        assert_eq!(node_types, [python_type]);
        assert!(refusal.contains("lend directory"), "{refusal}");
    }

    #[test]
    #[serial]
    fn a_live_add_of_an_undescribed_type_into_a_stream_with_no_environment_is_refused_by_name() {
        let python_type = import_path("my_app.live_add_with_no_environment:Blur");
        let project_directory = tempfile::tempdir().expect("a project directory");
        let runner = Runner::new().unwrap();
        let stream = runner
            .load_an_empty_stream(a_stream_with_no_environment(project_directory.path()))
            .expect("an empty stream loads");

        let refusal = stream
            .add_processor(ProcessorSpec::new(
                python_type.clone(),
                serde_json::json!({}),
            ))
            .expect_err("an undescribed type needs an environment");

        let (node_types, _) = refused_node_types(refusal);
        assert_eq!(node_types, [python_type]);
    }

    /// Two streams naming one class each describe it in their own interpreter
    /// and keep their own description, and a live add describes a type the
    /// stream does not resolve yet in that stream's environment alone. Nothing
    /// described reaches the machine's registry.
    #[test]
    #[serial]
    fn two_streams_describe_one_class_each_in_their_own_interpreter() {
        let loaded_type = import_path("my_app.described_by_two_streams:Blur");
        let added_live_type = import_path("my_app.described_at_a_live_add:Blur");

        let runner = Runner::new().unwrap();
        runner
            .set_processor_interpreter_lend_directory(LEND_DIRECTORY_FOR_TEST.into())
            .expect("the runner's first lend directory");
        let (_first_project, first_environment) = a_project_whose_interpreter_describes(
            serde_json::json!([a_described_type(&loaded_type, "video")]),
        );
        let (_second_project, second_environment) =
            a_project_whose_interpreter_describes(serde_json::json!([
                a_described_type(&loaded_type, "frames"),
                a_described_type(&added_live_type, "audio"),
            ]));
        let first_stream = runner
            .load_stream_from_graph_snapshot(
                &a_graph_of(&[&loaded_type]),
                OptionsForLoadingOneStream::in_stream_environment(first_environment).named("first"),
            )
            .expect("the first stream describes the type");
        let second_stream = runner
            .load_stream_from_graph_snapshot(
                &a_graph_of(&[&loaded_type]),
                OptionsForLoadingOneStream::in_stream_environment(second_environment)
                    .named("second"),
            )
            .expect("the second stream describes the type");

        assert_eq!(
            input_port_names_the_stream_resolves_for(&first_stream, &loaded_type),
            ["video"]
        );
        assert_eq!(
            input_port_names_the_stream_resolves_for(&second_stream, &loaded_type),
            ["frames"]
        );

        second_stream
            .add_processor(ProcessorSpec::new(
                added_live_type.clone(),
                serde_json::json!({}),
            ))
            .expect("a live add describes the type in the second stream's environment");
        assert_eq!(
            input_port_names_the_stream_resolves_for(&second_stream, &added_live_type),
            ["audio"]
        );
        assert!(
            first_stream
                .node_types_this_stream_resolves()
                .descriptor(&added_live_type)
                .is_none(),
            "a live add into one stream described its type into another"
        );
        assert!(PROCESSOR_REGISTRY.descriptor(&loaded_type).is_none());
        assert!(PROCESSOR_REGISTRY.descriptor(&added_live_type).is_none());
    }

    /// Whether the process `process_id` has ended — gone, or a zombie no
    /// longer running.
    fn the_process_has_ended(process_id: libc::pid_t) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        if unsafe { libc::kill(process_id, 0) } != 0 {
            return true;
        }
        std::fs::read_to_string(format!("/proc/{process_id}/stat"))
            .ok()
            .and_then(|stat| {
                stat.rsplit_once(')').map(|(_, after_the_command_name)| {
                    after_the_command_name.trim_start().starts_with('Z')
                })
            })
            .unwrap_or(false)
    }

    /// A load refused at a link — after its interpreter described its Python
    /// type and every node is in — adds no stream, and nothing it spawned is
    /// left running: what the describe's interpreter left behind in its
    /// process group is gone with the describe.
    #[test]
    #[serial]
    fn a_load_refused_at_a_link_after_a_describe_adds_no_stream_and_leaves_nothing_running() {
        let python_type = import_path("my_app.refused_at_a_link:Camera");
        let port_name_too_long_for_a_channel = "v".repeat(60);
        let display_type = import_path(&format!(
            "{}::LinkRefusedAfterADescribeDisplay",
            module_path!()
        ));
        let _ = PROCESSOR_REGISTRY.register_descriptor_only(
            ProcessorDescriptor::new(
                ProcessorClassShortName::new("LinkRefusedAfterADescribeDisplay").unwrap(),
                display_type.clone(),
                "a launch-record test double",
            )
            .with_input(PortDescriptor::new("video_in", "", false)),
        );

        let project_directory = tempfile::tempdir().expect("a project directory");
        let interpreter = project_directory.path().join("stub-python");
        let left_behind_process_id_file = project_directory.path().join("left-behind.pid");
        let describe_document = serde_json::json!({
            "described_node_types": [{
                "import_path": python_type.as_str(),
                "short_name": "Camera",
                "description": "captures",
                "execution": {"mode": "reactive"},
                "scheduling_priority": null,
                "config_schema": {"type": "object"},
                "input_ports": [],
                "output_ports": [{"name": port_name_too_long_for_a_channel, "description": ""}],
            }],
            "refused_node_types": [],
        });
        crate::core::test_support::write_an_executable_script_from_a_child_process(
            &interpreter,
            &format!(
                "#!/bin/sh\nsleep 30 </dev/null >/dev/null 2>&1 &\necho $! > '{}'\n\
                 cat <<'DESCRIBED'\n{describe_document}\nDESCRIBED\n",
                left_behind_process_id_file.display()
            ),
        );
        let stream_environment = StreamEnvironment {
            project_directory: project_directory.path().to_path_buf(),
            interpreter,
        };
        let runner = Runner::new().unwrap();
        runner
            .set_processor_interpreter_lend_directory(LEND_DIRECTORY_FOR_TEST.into())
            .expect("the runner's first lend directory");

        let refusal = the_refusal_of(
            runner.load_stream_from_graph_snapshot(
                &GraphSnapshot::from_graph_document(serde_json::json!({
                    "nodes": [
                        {"name": "camera", "type": python_type.as_str()},
                        {"name": "display", "type": display_type.as_str()}
                    ],
                    "links": [{
                        "source": {"node": "camera", "port": port_name_too_long_for_a_channel},
                        "target": {"node": "display", "port": "video_in"}
                    }]
                }))
                .expect("the test graph reads"),
                OptionsForLoadingOneStream::in_stream_environment(stream_environment).named("main"),
            ),
            "the engine refuses the link",
        );

        assert!(
            matches!(refusal, Error::InvalidLink(_)),
            "the load must be refused at the link, after the describe: {refusal:?}"
        );
        assert!(runner.names_of_the_loaded_streams().is_empty());
        let left_behind_process_id: libc::pid_t =
            std::fs::read_to_string(&left_behind_process_id_file)
                .expect("the describe's interpreter ran")
                .trim()
                .parse()
                .expect("a process id");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !the_process_has_ended(left_behind_process_id) {
            assert!(
                std::time::Instant::now() < deadline,
                "a process the refused load's describe left behind is still running"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
