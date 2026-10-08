// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;

use crate::core::compiler::compiler_ops::processor_interpreter_describe::{
    a_processor_interpreter_describes, describe_and_register_node_types_in_a_processor_interpreter,
};
use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::error::{Error, Result};
use crate::core::processors::PROCESSOR_REGISTRY;

use super::StreamEnvironment;

/// What a runtime starts its processor interpreters with: the lend directory
/// its host handed it, the stream environment recorded at the last load, and
/// whether its host interrupted that load's describes.
#[derive(Default)]
pub(crate) struct ProcessorInterpreterLaunchRecord {
    processor_interpreter_lend_directory: Mutex<Option<PathBuf>>,
    stream_environment_recorded_at_the_last_load: Mutex<Option<StreamEnvironment>>,
    describes_interrupted_by_the_host: AtomicBool,
}

impl ProcessorInterpreterLaunchRecord {
    pub(crate) fn set_processor_interpreter_lend_directory(&self, lend_directory: PathBuf) {
        *self.processor_interpreter_lend_directory.lock() = Some(lend_directory);
    }

    pub(crate) fn interrupt_every_describe(&self) {
        self.describes_interrupted_by_the_host
            .store(true, Ordering::SeqCst);
    }

    pub(crate) fn forget_the_interrupt_of_an_earlier_load(&self) {
        self.describes_interrupted_by_the_host
            .store(false, Ordering::SeqCst);
    }

    pub(crate) fn record_the_stream_environment_of_a_load(
        &self,
        stream_environment: Option<StreamEnvironment>,
    ) {
        *self.stream_environment_recorded_at_the_last_load.lock() = stream_environment;
    }

    pub(crate) fn stream_environment_recorded_at_the_last_load(&self) -> Option<StreamEnvironment> {
        self.stream_environment_recorded_at_the_last_load
            .lock()
            .clone()
    }

    /// Describe, in one interpreter start, every type in `node_types` a
    /// processor interpreter describes and no other registration holds —
    /// re-describing one an earlier describe registered — and register each.
    pub(crate) fn describe_and_register_every_type_a_load_names<'a>(
        &self,
        node_types: impl IntoIterator<Item = &'a ProcessorClassImportPath>,
    ) -> Result<()> {
        let mut node_types_to_describe: Vec<ProcessorClassImportPath> = Vec::new();
        for node_type in node_types {
            if a_processor_interpreter_describes(node_type)
                && !PROCESSOR_REGISTRY.is_registered_other_than_by_a_describe(node_type)
                && !node_types_to_describe.contains(node_type)
            {
                node_types_to_describe.push(node_type.clone());
            }
        }
        self.describe_and_register(&node_types_to_describe)
    }

    /// Whether a live add of `node_type` has to describe it first: a type a
    /// processor interpreter describes that nothing has registered yet.
    pub(crate) fn a_live_add_must_describe(&self, node_type: &ProcessorClassImportPath) -> bool {
        a_processor_interpreter_describes(node_type)
            && PROCESSOR_REGISTRY.descriptor(node_type).is_none()
    }

    /// Describe and register `node_type` for a live add, in the environment
    /// recorded at the last load.
    pub(crate) fn describe_and_register_a_type_a_live_add_names(
        &self,
        node_type: &ProcessorClassImportPath,
    ) -> Result<()> {
        self.describe_and_register(std::slice::from_ref(node_type))
    }

    fn describe_and_register(&self, node_types: &[ProcessorClassImportPath]) -> Result<()> {
        if node_types.is_empty() {
            return Ok(());
        }
        let Some(stream_environment) = self.stream_environment_recorded_at_the_last_load() else {
            return Err(Error::NodeTypesNotDescribed {
                node_types: node_types.to_vec(),
                refusal: "a type that is not a built-in is described in the stream's own \
                          interpreter, and this runtime was given no stream environment — \
                          load the stream with its project directory and interpreter"
                    .to_string(),
            });
        };
        let Some(lend_directory) = self.processor_interpreter_lend_directory.lock().clone() else {
            return Err(Error::NodeTypesNotDescribed {
                node_types: node_types.to_vec(),
                refusal: "a type that is not a built-in is described in the stream's own \
                          interpreter, and this runtime's host gave it no lend directory to \
                          start one from"
                    .to_string(),
            });
        };
        describe_and_register_node_types_in_a_processor_interpreter(
            node_types,
            &stream_environment,
            &lend_directory,
            &|| {
                self.describes_interrupted_by_the_host
                    .load(Ordering::SeqCst)
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::core::descriptors::{PortDescriptor, ProcessorClassShortName, ProcessorDescriptor};
    use crate::core::graph_snapshot::GraphSnapshot;
    use crate::core::processors::ProcessorSpec;
    use crate::core::runtime::Runner;
    use serial_test::serial;

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

    fn input_port_names_registered_for(node_type: &ProcessorClassImportPath) -> Vec<String> {
        PROCESSOR_REGISTRY
            .descriptor(node_type)
            .expect("the type is registered")
            .inputs
            .into_iter()
            .map(|port| port.name)
            .collect()
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
    fn a_load_records_the_stream_environment_it_was_given() {
        let rust_type = a_rust_type_registered_as("RecordedEnvironmentSource");
        let stream_environment = StreamEnvironment {
            project_directory: "/home/someone/my_app".into(),
            interpreter: "/home/someone/my_app/.venv/bin/python".into(),
        };
        let runtime = Runner::new().unwrap();
        assert_eq!(runtime.stream_environment_recorded_at_the_last_load(), None);

        runtime
            .load_graph_snapshot(&a_graph_of(&[&rust_type]), Some(stream_environment.clone()))
            .expect("a graph of Rust types loads");

        assert_eq!(
            runtime.stream_environment_recorded_at_the_last_load(),
            Some(stream_environment)
        );
    }

    /// A Rust type is never described, so a graph naming only Rust types
    /// loads with no environment and starts no interpreter.
    #[test]
    fn a_graph_of_rust_types_needs_no_stream_environment() {
        let rust_type = a_rust_type_registered_as("NoEnvironmentSource");

        Runner::new()
            .unwrap()
            .load_graph_snapshot(&a_graph_of(&[&rust_type]), None)
            .expect("a graph of Rust types loads with no environment");
    }

    #[test]
    fn a_load_naming_a_type_to_describe_with_no_environment_refuses_it_by_name() {
        let rust_type = a_rust_type_registered_as("BesideAnUndescribedType");
        let python_type = import_path("my_app.load_with_no_environment:Blur");
        let runtime = Runner::new().unwrap();

        let refusal = runtime
            .load_graph_snapshot(&a_graph_of(&[&rust_type, &python_type]), None)
            .expect_err("a type to describe needs an environment");

        let (node_types, refusal) = refused_node_types(refusal);
        assert_eq!(node_types, [python_type]);
        assert!(refusal.contains("no stream environment"), "{refusal}");
        assert_eq!(
            runtime.to_json().unwrap()["nodes"],
            serde_json::json!([]),
            "a refused load adds nothing"
        );
    }

    /// A host whose user interrupts a load mid-describe — a module that hangs
    /// at import — gets the load back long before the describe's bound.
    #[test]
    #[serial]
    fn a_load_whose_host_interrupts_its_describe_returns_refusing_the_type() {
        let python_type = import_path("my_app.hangs_at_import:Blur");
        let project_directory = tempfile::tempdir().expect("a project directory");
        let interpreter = project_directory.path().join("stub-python");
        crate::core::test_support::write_an_executable_script_from_a_child_process(
            &interpreter,
            "#!/bin/sh\nsleep 30 &\nwait\n",
        );
        let runtime = std::sync::Arc::new(Runner::new().unwrap());
        runtime.set_processor_interpreter_lend_directory("/opt/tatolab/lib/tatolab/lend".into());
        let started = std::time::Instant::now();

        let interrupting_runtime = std::sync::Arc::clone(&runtime);
        let interrupter = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            interrupting_runtime.interrupt_every_processor_interpreter_describe();
        });
        let refusal = runtime
            .load_graph_snapshot(
                &a_graph_of(&[&python_type]),
                Some(StreamEnvironment {
                    project_directory: project_directory.path().to_path_buf(),
                    interpreter,
                }),
            )
            .expect_err("an interrupted describe registers nothing");
        interrupter.join().unwrap();

        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the load held its host's interrupt for the describe's whole bound"
        );
        let (node_types, refusal) = refused_node_types(refusal);
        assert_eq!(node_types, [python_type]);
        assert!(refusal.contains("interrupted"), "{refusal}");
    }

    /// An interrupt belongs to the load it cut short: the next load on the
    /// same runtime describes as if none had happened.
    #[test]
    #[serial]
    fn a_load_after_one_its_host_interrupted_describes_normally() {
        let interrupted_type = import_path("my_app.hangs_before_a_later_load:Blur");
        let later_type = import_path("my_app.described_after_an_interrupted_load:Blur");
        let hanging_project_directory = tempfile::tempdir().expect("a project directory");
        let hanging_interpreter = hanging_project_directory.path().join("stub-python");
        crate::core::test_support::write_an_executable_script_from_a_child_process(
            &hanging_interpreter,
            "#!/bin/sh\nsleep 30 &\nwait\n",
        );
        let runtime = std::sync::Arc::new(Runner::new().unwrap());
        runtime.set_processor_interpreter_lend_directory("/opt/tatolab/lib/tatolab/lend".into());
        let interrupting_runtime = std::sync::Arc::clone(&runtime);
        let interrupter = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            interrupting_runtime.interrupt_every_processor_interpreter_describe();
        });
        runtime
            .load_graph_snapshot(
                &a_graph_of(&[&interrupted_type]),
                Some(StreamEnvironment {
                    project_directory: hanging_project_directory.path().to_path_buf(),
                    interpreter: hanging_interpreter,
                }),
            )
            .expect_err("the interrupted describe registers nothing");
        interrupter.join().unwrap();

        let (_later_project, later_environment) = a_project_whose_interpreter_describes(
            serde_json::json!([a_described_type(&later_type, "video")]),
        );
        runtime
            .load_graph_snapshot(&a_graph_of(&[&later_type]), Some(later_environment))
            .expect("a later load describes its types");

        assert_eq!(input_port_names_registered_for(&later_type), ["video"]);
    }

    #[test]
    fn a_load_with_no_lend_directory_refuses_a_type_to_describe_by_name() {
        let python_type = import_path("my_app.load_with_no_lend_directory:Blur");
        let (_project_directory, stream_environment) =
            a_project_whose_interpreter_describes(serde_json::json!([]));

        let refusal = Runner::new()
            .unwrap()
            .load_graph_snapshot(&a_graph_of(&[&python_type]), Some(stream_environment))
            .expect_err("a type to describe needs a lend directory");

        let (node_types, refusal) = refused_node_types(refusal);
        assert_eq!(node_types, [python_type]);
        assert!(refusal.contains("lend directory"), "{refusal}");
    }

    #[test]
    fn a_live_add_of_an_undescribed_type_with_no_environment_recorded_is_refused_by_name() {
        let python_type = import_path("my_app.live_add_with_no_environment:Blur");

        let refusal = Runner::new()
            .unwrap()
            .add_processor(ProcessorSpec::new(
                python_type.clone(),
                serde_json::json!({}),
            ))
            .expect_err("an undescribed type needs an environment");

        let (node_types, _) = refused_node_types(refusal);
        assert_eq!(node_types, [python_type]);
    }

    /// Each load describes again what an earlier describe registered — the
    /// class's ports may have changed since — and a live add describes a type
    /// nothing has registered in the environment the load recorded.
    #[test]
    #[serial]
    fn a_load_redescribes_its_types_and_a_live_add_describes_in_the_recorded_environment() {
        let loaded_type = import_path("my_app.redescribed_at_load:Blur");
        let added_live_type = import_path("my_app.described_at_a_live_add:Blur");

        let runtime = Runner::new().unwrap();
        runtime.set_processor_interpreter_lend_directory("/opt/tatolab/lib/tatolab/lend".into());
        let (_first_project, first_environment) = a_project_whose_interpreter_describes(
            serde_json::json!([a_described_type(&loaded_type, "video")]),
        );
        runtime
            .load_graph_snapshot(&a_graph_of(&[&loaded_type]), Some(first_environment))
            .expect("the described type loads");
        assert_eq!(input_port_names_registered_for(&loaded_type), ["video"]);
        assert!(PROCESSOR_REGISTRY.was_described_in_a_processor_interpreter(&loaded_type));

        let (_second_project, second_environment) =
            a_project_whose_interpreter_describes(serde_json::json!([
                a_described_type(&loaded_type, "frames"),
                a_described_type(&added_live_type, "audio"),
            ]));
        let second_runtime = Runner::new().unwrap();
        second_runtime
            .set_processor_interpreter_lend_directory(Path::new("/opt/tatolab/lend").into());
        second_runtime
            .load_graph_snapshot(&a_graph_of(&[&loaded_type]), Some(second_environment))
            .expect("the type is described again");
        assert_eq!(input_port_names_registered_for(&loaded_type), ["frames"]);

        second_runtime
            .add_processor(ProcessorSpec::new(
                added_live_type.clone(),
                serde_json::json!({}),
            ))
            .expect("a live add describes the type in the recorded environment");
        assert_eq!(input_port_names_registered_for(&added_live_type), ["audio"]);
    }
}
