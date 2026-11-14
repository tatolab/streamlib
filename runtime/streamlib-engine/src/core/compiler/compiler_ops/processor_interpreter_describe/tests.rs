// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use serial_test::serial;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::Instant;

const GOOD_TYPE: &str = "my_app.filters:BlurProcessor";

fn a_stream_whose_shutdown_is_not_requested() -> LoadedStreamAHelperProcessBelongsTo {
    LoadedStreamAHelperProcessBelongsTo {
        stream_tag: crate::core::runtime::LoadedStreamTag::next_in_this_process()
            .expect("a fresh stream tag"),
        shutdown_escalation: crate::core::runtime::ShutdownEscalationOfOneStream::default(),
    }
}

fn import_path(path: &str) -> ProcessorClassImportPath {
    ProcessorClassImportPath::new(path).expect("the test path names a class")
}

/// A stand-in for a venv interpreter: a shell script that ignores its
/// arguments, runs `body`, and exits however `body` does.
struct StubProcessorInterpreter {
    project_directory: tempfile::TempDir,
    stream_environment: StreamEnvironment,
}

impl StubProcessorInterpreter {
    fn running(body: &str) -> Self {
        let project_directory = tempfile::tempdir().expect("a project directory");
        let interpreter = project_directory.path().join("stub-python");
        crate::core::test_support::write_an_executable_script_from_a_child_process(
            &interpreter,
            &format!("#!/bin/sh\n{body}\n"),
        );
        let stream_environment = StreamEnvironment {
            project_directory: project_directory.path().to_path_buf(),
            interpreter,
        };
        Self {
            project_directory,
            stream_environment,
        }
    }

    fn describe(
        &self,
        import_paths: &[&str],
        describe_bound: Duration,
    ) -> Result<Vec<PythonProcessorDeclaration>> {
        self.describe_in_the_stream(
            import_paths,
            describe_bound,
            &a_stream_whose_shutdown_is_not_requested(),
        )
    }

    fn describe_in_the_stream(
        &self,
        import_paths: &[&str],
        describe_bound: Duration,
        the_stream_the_describe_belongs_to: &LoadedStreamAHelperProcessBelongsTo,
    ) -> Result<Vec<PythonProcessorDeclaration>> {
        let import_paths: Vec<_> = import_paths.iter().map(|path| import_path(path)).collect();
        describe_node_types_in_a_processor_interpreter_within(
            &import_paths,
            &self.stream_environment,
            Path::new("/opt/tatolab/lib/tatolab/lend"),
            describe_bound,
            the_stream_the_describe_belongs_to,
            &|| false,
        )
    }

    fn refusal_of(&self, import_paths: &[&str]) -> (Vec<ProcessorClassImportPath>, String) {
        match self.describe(import_paths, PROCESSOR_INTERPRETER_DESCRIBE_BOUND) {
            Err(Error::NodeTypesNotDescribed {
                node_types,
                refusal,
            }) => (node_types, refusal),
            Err(other) => panic!("expected NodeTypesNotDescribed, got {other:?}"),
            Ok(_) => panic!("the describe was accepted; a refusal was expected"),
        }
    }
}

/// A shell line printing `document` on standard output.
fn print_on_standard_output(document: &serde_json::Value) -> String {
    format!("cat <<'DESCRIBED'\n{document}\nDESCRIBED")
}

fn a_described_good_type() -> serde_json::Value {
    serde_json::json!({
        "import_path": GOOD_TYPE,
        "short_name": "BlurProcessor",
        "description": "blurs",
        "kind": "process",
        "execution": {"mode": "reactive"},
        "scheduling_priority": null,
        "paced_by_real_time": false,
        "config_schema": {"type": "object"},
        "input_ports": [{"name": "video", "family": {"family": "records"}, "description": "", "delivery_profile": "newest"}],
        "output_ports": [{"name": "blurred", "family": {"family": "records"}, "description": ""}],
    })
}

#[test]
#[serial]
fn a_type_the_interpreter_describes_is_read_into_its_declaration() {
    let stub = StubProcessorInterpreter::running(&print_on_standard_output(&serde_json::json!({
        "described_node_types": [a_described_good_type()],
        "refused_node_types": [],
    })));

    let declarations = stub
        .describe(&[GOOD_TYPE], PROCESSOR_INTERPRETER_DESCRIBE_BOUND)
        .unwrap_or_else(|refusal| panic!("the good type is described: {refusal}"));

    assert_eq!(declarations.len(), 1);
    let descriptor = &declarations[0].descriptor;
    assert_eq!(descriptor.processor_class_import_path.as_str(), GOOD_TYPE);
    assert_eq!(descriptor.inputs[0].name, "video");
    assert_eq!(descriptor.outputs[0].name, "blurred");
    assert!(matches!(
        declarations[0].how_a_python_node_of_the_type_runs,
        HowAPythonNodeOfTheTypeRuns::InItsOwnProcessorInterpreter(execution_config)
            if execution_config.execution == crate::core::execution::ProcessExecution::Reactive
    ));
}

/// A process a stub started in a session of its own, which the describe's
/// group kill cannot reach; killed when the test ends, however it ends.
struct ProcessThatLeftTheDescribesProcessGroup {
    process_id_file: PathBuf,
}

impl Drop for ProcessThatLeftTheDescribesProcessGroup {
    fn drop(&mut self) {
        let gives_up_at = Instant::now() + Duration::from_secs(5);
        loop {
            let recorded_process_id = std::fs::read_to_string(&self.process_id_file)
                .ok()
                .and_then(|text| text.trim().parse::<libc::pid_t>().ok());
            if let Some(process_id) = recorded_process_id {
                // SAFETY: a plain signal to a pid this test's stub started.
                unsafe { libc::kill(process_id, libc::SIGKILL) };
                return;
            }
            if Instant::now() >= gives_up_at {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// A module that daemonizes at import leaves a process outside the
/// describe's group holding its standard output open, so the pipe never
/// closes; the document the interpreter printed before exiting is still read.
#[test]
#[serial]
fn a_document_printed_before_exiting_is_read_while_an_escaped_descendant_holds_standard_output() {
    let stub = StubProcessorInterpreter::running(&format!(
        "python3 -c 'import os; os.setsid(); \
             open(\"escaped-process-id\", \"w\").write(str(os.getpid())); \
             os.execvp(\"sleep\", [\"sleep\", \"30\"])' &\n\
             while [ ! -s escaped-process-id ]; do sleep 0.01; done\n{}\nexit 0",
        print_on_standard_output(&serde_json::json!({
            "described_node_types": [a_described_good_type()],
            "refused_node_types": [],
        }))
    ));
    let _escaped_descendant = ProcessThatLeftTheDescribesProcessGroup {
        process_id_file: stub.project_directory.path().join("escaped-process-id"),
    };
    let started = Instant::now();

    let declarations = stub
        .describe(&[GOOD_TYPE], PROCESSOR_INTERPRETER_DESCRIBE_BOUND)
        .unwrap_or_else(|refusal| panic!("the printed document is read: {refusal}"));

    assert_eq!(declarations.len(), 1);
    assert_eq!(
        declarations[0]
            .descriptor
            .processor_class_import_path
            .as_str(),
        GOOD_TYPE
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the describe waited on the escaped descendant"
    );
}

#[test]
#[serial]
fn the_describe_holds_a_pipe_on_the_interpreters_standard_input() {
    let stub = StubProcessorInterpreter::running(&format!(
        "if [ -p /dev/stdin ]; then\n{}\nelse\necho 'standard input is not a pipe' >&2\nexit 1\nfi",
        print_on_standard_output(&serde_json::json!({
            "described_node_types": [a_described_good_type()],
            "refused_node_types": [],
        }))
    ));

    stub.describe(&[GOOD_TYPE], PROCESSOR_INTERPRETER_DESCRIBE_BOUND)
        .unwrap_or_else(|refusal| panic!("the describe's standard input is a pipe: {refusal}"));
}

#[test]
#[serial]
fn a_type_whose_module_will_not_import_is_refused_by_name_quoting_the_reason_it_was_given() {
    let stub = StubProcessorInterpreter::running(&format!(
        "{}\nexit 1",
        print_on_standard_output(&serde_json::json!({
            "described_node_types": [a_described_good_type()],
            "refused_node_types": [{
                "import_path": "my_app.missing:Blur",
                "refusal": "its module `my_app.missing` did not import:\n\
                            ModuleNotFoundError: No module named 'my_app.missing'\n",
            }],
        }))
    ));

    let (node_types, refusal) = stub.refusal_of(&[GOOD_TYPE, "my_app.missing:Blur"]);

    assert_eq!(node_types, [import_path("my_app.missing:Blur")]);
    assert!(
        refusal.contains("ModuleNotFoundError: No module named 'my_app.missing'"),
        "{refusal}"
    );
}

#[test]
#[serial]
fn a_class_carrying_no_node_stamp_is_refused_by_name() {
    let stub = StubProcessorInterpreter::running(&format!(
        "{}\nexit 1",
        print_on_standard_output(&serde_json::json!({
            "described_node_types": [],
            "refused_node_types": [{
                "import_path": "my_app.nodes:Plain",
                "refusal": "the class `Plain` carries no `@node.process` declaration, so it is not a node",
            }],
        }))
    ));

    let (node_types, refusal) = stub.refusal_of(&["my_app.nodes:Plain"]);

    assert_eq!(node_types, [import_path("my_app.nodes:Plain")]);
    assert!(
        refusal.contains("the class `Plain` carries no `@node.process` declaration"),
        "{refusal}"
    );
}

/// The reasons travel in the document, so a module imported after a
/// refused one cannot push them out of the standard-error tail.
#[test]
#[serial]
fn each_refused_type_is_quoted_with_its_own_reason_however_much_a_later_import_writes_to_standard_error()
 {
    let stub = StubProcessorInterpreter::running(&format!(
        "head -c 65536 /dev/zero | tr '\\0' x >&2\n{}\nexit 1",
        print_on_standard_output(&serde_json::json!({
            "described_node_types": [],
            "refused_node_types": [
                {"import_path": "my_app.broken:Blur", "refusal": "the reason Blur was refused"},
                {"import_path": "my_app.nodes:Plain", "refusal": "the reason Plain was refused"},
            ],
        }))
    ));

    let (node_types, refusal) = stub.refusal_of(&["my_app.broken:Blur", "my_app.nodes:Plain"]);

    assert_eq!(
        node_types,
        [
            import_path("my_app.broken:Blur"),
            import_path("my_app.nodes:Plain")
        ]
    );
    assert!(
        refusal.contains("`my_app.broken:Blur`: the reason Blur was refused"),
        "{refusal}"
    );
    assert!(
        refusal.contains("`my_app.nodes:Plain`: the reason Plain was refused"),
        "{refusal}"
    );
}

/// An interpreter that cannot load the lent runtime, refuses the build id,
/// or crashes prints no document, so every type asked for is refused,
/// naming the interpreter and quoting what it wrote.
#[test]
#[serial]
fn an_interpreter_that_printed_no_document_refuses_every_type_naming_the_interpreter() {
    let stub = StubProcessorInterpreter::running(
        "printf '[streamlib] this interpreter cannot load the lent runtime: PyPy 3.9\\n' >&2\n\
             exit 1",
    );

    let (node_types, refusal) = stub.refusal_of(&[GOOD_TYPE, "my_app.nodes:Sharpen"]);

    assert_eq!(
        node_types,
        [import_path(GOOD_TYPE), import_path("my_app.nodes:Sharpen")]
    );
    assert!(
        refusal.contains(&stub.stream_environment.interpreter.display().to_string()),
        "{refusal}"
    );
    assert!(
        refusal.contains("cannot load the lent runtime: PyPy 3.9"),
        "{refusal}"
    );
}

/// A site hook that prints before the bootstrap reserves standard output
/// leaves the document unparseable; the refusal quotes what was printed.
#[test]
#[serial]
fn a_standard_output_that_is_not_a_document_is_refused_quoting_its_head_and_the_parse_error() {
    let stub = StubProcessorInterpreter::running(&format!(
        "echo 'sitecustomize says hello'\n{}",
        print_on_standard_output(&serde_json::json!({
            "described_node_types": [a_described_good_type()],
            "refused_node_types": [],
        }))
    ));

    let (node_types, refusal) = stub.refusal_of(&[GOOD_TYPE]);

    assert_eq!(node_types, [import_path(GOOD_TYPE)]);
    assert!(
        refusal.contains("is not a describe document ("),
        "{refusal}"
    );
    assert!(refusal.contains("sitecustomize says hello"), "{refusal}");
    assert!(
        !refusal.contains("printed no describe document"),
        "{refusal}"
    );
}

#[test]
#[serial]
fn a_describe_past_its_bound_is_killed_with_its_group_and_refuses_every_type_naming_the_bound() {
    let stub = StubProcessorInterpreter::running("sleep 30 &\necho $! > worker-pid\nwait");
    let started = Instant::now();

    let refusal = match stub.describe(&[GOOD_TYPE], Duration::from_millis(300)) {
        Err(Error::NodeTypesNotDescribed {
            node_types,
            refusal,
        }) => {
            assert_eq!(node_types, [import_path(GOOD_TYPE)]);
            refusal
        }
        Err(other) => panic!("expected NodeTypesNotDescribed, got {other:?}"),
        Ok(_) => panic!("a describe past its bound was accepted"),
    };

    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the bound was not kept"
    );
    assert!(refusal.contains("within 0.3s"), "{refusal}");
    let worker_pid: libc::pid_t =
        std::fs::read_to_string(stub.project_directory.path().join("worker-pid"))
            .expect("the stub recorded its worker")
            .trim()
            .parse()
            .expect("a pid");
    // Polled, because the killed worker is reaped by init rather than here.
    let gone_by = Instant::now() + Duration::from_secs(5);
    // SAFETY: signal 0 delivers nothing; it only reports reachability.
    while unsafe { libc::kill(worker_pid, 0) } == 0 {
        assert!(
            Instant::now() < gone_by,
            "a worker the describe started outlived it"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[serial]
fn a_shutdown_requested_during_a_describe_kills_it_and_refuses_every_type_saying_so() {
    let stub = StubProcessorInterpreter::running("sleep 30 &\nwait");
    let the_stream = a_stream_whose_shutdown_is_not_requested();
    let shutdown_requested_once_the_describe_has_started = {
        let shutdown_escalation = the_stream.shutdown_escalation.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            shutdown_escalation.raise_to_graceful();
        })
    };
    let started = Instant::now();

    let refusal = match stub.describe_in_the_stream(
        &[GOOD_TYPE],
        PROCESSOR_INTERPRETER_DESCRIBE_BOUND,
        &the_stream,
    ) {
        Err(Error::NodeTypesNotDescribed {
            node_types,
            refusal,
        }) => {
            assert_eq!(node_types, [import_path(GOOD_TYPE)]);
            refusal
        }
        Err(other) => panic!("expected NodeTypesNotDescribed, got {other:?}"),
        Ok(_) => panic!("a describe interrupted by shutdown was accepted"),
    };

    shutdown_requested_once_the_describe_has_started
        .join()
        .expect("the requesting thread returns");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the describe held shutdown for its whole bound"
    );
    assert!(refusal.contains("shutdown began"), "{refusal}");
}

#[test]
#[serial]
fn a_describe_its_host_interrupts_is_killed_and_refuses_every_type_saying_so() {
    let stub = StubProcessorInterpreter::running("sleep 30 &\nwait");
    let started = Instant::now();
    let interrupted_after = Duration::from_millis(200);

    let refusal = match describe_node_types_in_a_processor_interpreter_within(
        &[import_path(GOOD_TYPE)],
        &stub.stream_environment,
        Path::new("/opt/tatolab/lib/tatolab/lend"),
        PROCESSOR_INTERPRETER_DESCRIBE_BOUND,
        &a_stream_whose_shutdown_is_not_requested(),
        &|| started.elapsed() >= interrupted_after,
    ) {
        Err(Error::NodeTypesNotDescribed {
            node_types,
            refusal,
        }) => {
            assert_eq!(node_types, [import_path(GOOD_TYPE)]);
            refusal
        }
        Err(other) => panic!("expected NodeTypesNotDescribed, got {other:?}"),
        Ok(_) => panic!("a describe its host interrupted was accepted"),
    };

    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the describe held its host's interrupt for its whole bound"
    );
    assert!(
        refusal.contains(DESCRIBE_INTERRUPTED_BY_THE_HOST_REFUSAL),
        "{refusal}"
    );
}

#[test]
fn a_project_directory_holding_the_path_list_separator_refuses_every_type_naming_it() {
    let refusal = describe_node_types_in_a_processor_interpreter_within(
        &[import_path(GOOD_TYPE), import_path("my_app.nodes:Sharpen")],
        &StreamEnvironment {
            project_directory: PathBuf::from("/home/someone/my:app"),
            interpreter: PathBuf::from("/home/someone/my:app/.venv/bin/python"),
        },
        Path::new("/opt/tatolab/lib/tatolab/lend"),
        PROCESSOR_INTERPRETER_DESCRIBE_BOUND,
        &a_stream_whose_shutdown_is_not_requested(),
        &|| false,
    );

    match refusal {
        Err(Error::NodeTypesNotDescribed {
            node_types,
            refusal,
        }) => {
            assert_eq!(
                node_types,
                [import_path(GOOD_TYPE), import_path("my_app.nodes:Sharpen")]
            );
            assert!(refusal.contains("`/home/someone/my:app`"), "{refusal}");
        }
        Err(other) => panic!("expected NodeTypesNotDescribed, got {other:?}"),
        Ok(_) => panic!("a project directory holding `:` was described from"),
    }
}

#[test]
fn the_describe_command_carries_the_stream_environment_and_no_processors_variables() {
    let stream_environment = StreamEnvironment {
        project_directory: PathBuf::from("/home/someone/my_app"),
        interpreter: PathBuf::from("/home/someone/my_app/.venv/bin/python"),
    };
    let command = processor_interpreter_describe_command(
        &[import_path(GOOD_TYPE), import_path("my_app.nodes:Sharpen")],
        &stream_environment,
        Path::new("/opt/tatolab/lib/tatolab/lend"),
    )
    .expect("the test directories hold no path-list separator");

    assert_eq!(
        command.get_program(),
        OsStr::new("/home/someone/my_app/.venv/bin/python")
    );
    let arguments: Vec<_> = command.get_args().collect();
    assert_eq!(
        arguments,
        [
            "/opt/tatolab/lib/tatolab/lend/tatolab/runtime/_processor_interpreter_bootstrap.py",
            "--describe",
            GOOD_TYPE,
            "my_app.nodes:Sharpen",
        ]
    );
    assert_eq!(
        command.get_current_dir(),
        Some(Path::new("/home/someone/my_app"))
    );
    let environment: Vec<(&OsStr, Option<&OsStr>)> = command.get_envs().collect();
    let value_of = |name: &str| {
        environment
            .iter()
            .find(|(entry_name, _)| *entry_name == OsStr::new(name))
            .map(|(_, value)| *value)
    };
    assert_eq!(
        value_of("PYTHONPATH"),
        Some(Some(OsStr::new(
            "/opt/tatolab/lib/tatolab/lend:/home/someone/my_app"
        )))
    );
    assert_eq!(
        value_of(super::super::subprocess_bridge::ENGINE_BUILD_ID_ENVIRONMENT_VARIABLE),
        Some(Some(OsStr::new(
            super::super::subprocess_bridge::ENGINE_BUILD_ID
        )))
    );
    for per_processor_variable in PER_PROCESSOR_ENVIRONMENT_VARIABLES {
        assert_eq!(
            value_of(per_processor_variable),
            Some(None),
            "{per_processor_variable} names one processor, which a describe is not"
        );
    }
}

/// What the command carries is what the interpreter it starts receives.
#[test]
#[serial]
fn a_started_describe_receives_its_arguments_working_directory_and_python_path() {
    let stub = StubProcessorInterpreter::running(&format!(
        "printf '%s\\n' \"$@\" > \"$PWD/arguments\"\nprintf '%s' \"$PYTHONPATH\" > \
             \"$PWD/python-path\"\n{}",
        print_on_standard_output(&serde_json::json!({
            "described_node_types": [a_described_good_type()],
            "refused_node_types": [],
        }))
    ));

    stub.describe(&[GOOD_TYPE], PROCESSOR_INTERPRETER_DESCRIBE_BOUND)
        .unwrap_or_else(|refusal| panic!("the good type is described: {refusal}"));

    let project_directory = stub.project_directory.path();
    assert_eq!(
        std::fs::read_to_string(project_directory.join("arguments")).unwrap(),
        format!(
            "/opt/tatolab/lib/tatolab/lend/tatolab/runtime/_processor_interpreter_bootstrap.py\n\
                 --describe\n{GOOD_TYPE}\n"
        )
    );
    assert_eq!(
        std::fs::read_to_string(project_directory.join("python-path")).unwrap(),
        format!(
            "/opt/tatolab/lib/tatolab/lend:{}",
            project_directory.display()
        )
    );
}

#[test]
fn only_a_module_and_qualname_that_names_no_built_in_is_described() {
    assert!(a_processor_interpreter_describes(&import_path(GOOD_TYPE)));
    assert!(!a_processor_interpreter_describes(&import_path(
        "tatolab.stream:ChannelsToArray"
    )));
    assert!(!a_processor_interpreter_describes(&import_path(
        "my_crate::processors::Blur"
    )));
    assert!(!a_processor_interpreter_describes(&import_path("Blur")));
}
