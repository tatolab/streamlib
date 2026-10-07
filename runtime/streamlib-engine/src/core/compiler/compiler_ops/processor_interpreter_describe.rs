// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Asking a stream's own interpreter what its node types declare.
//!
//! The runtime process never imports a node's module. It starts the same
//! command a processor interpreter is, with `--describe` and the import paths,
//! and reads the one JSON document the bootstrap prints on its standard output.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use super::processor_interpreter_shutdown_ladder::a_helper_process_has_exited_without_being_reaped;
use super::processor_interpreter_spawn_host::{
    PROCESSOR_INTERPRETER_ENTRYPOINT_ENVIRONMENT_VARIABLE,
    PROCESSOR_INTERPRETER_PROCESSOR_ID_ENVIRONMENT_VARIABLE, STANDARD_ERROR_TAIL_BYTES,
    SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE,
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours,
    give_the_child_no_descriptor_beyond_stdio, processor_interpreter_bootstrap_command,
    spawn_host_for_processor_node,
};
use super::python_processor_declaration::PythonProcessorDeclaration;
use super::subprocess_bridge::ESCALATE_FD_ENV;
use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::error::{Error, Result};
use crate::core::processors::{DynGeneratedProcessor, PROCESSOR_REGISTRY};
use crate::core::runtime::StreamEnvironment;
use crate::iceoryx2::{
    ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE, spawn_outside_every_iceoryx2_listener_bind,
};

/// How long one describe may run before its process group is killed and every
/// type it was asked for is refused.
pub(crate) const PROCESSOR_INTERPRETER_DESCRIBE_BOUND: Duration = Duration::from_secs(60);

/// The bootstrap argument that asks for a describe rather than a processor.
const DESCRIBE_ARGUMENT: &str = "--describe";

/// The variables that name one processor to the interpreter hosting it, none
/// of which a describe carries.
const PER_PROCESSOR_ENVIRONMENT_VARIABLES: [&str; 6] = [
    PROCESSOR_INTERPRETER_ENTRYPOINT_ENVIRONMENT_VARIABLE,
    PROCESSOR_INTERPRETER_PROCESSOR_ID_ENVIRONMENT_VARIABLE,
    "STREAMLIB_RUNTIME_ID",
    ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE,
    SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE,
    ESCALATE_FD_ENV,
];

/// How often the wait for a describe re-checks whether it has exited.
const DESCRIBE_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How long the readers of a finished describe's standard streams are waited
/// for once its process group is down.
const DESCRIBE_STREAM_CLOSE_DEADLINE: Duration = Duration::from_secs(1);

/// The document a describe prints on its standard output.
#[derive(serde::Deserialize)]
struct ProcessorInterpreterDescribeDocument {
    described_node_types: Vec<serde_json::Value>,
    refused_import_paths: Vec<String>,
}

/// Whether `node_type` is one a processor interpreter describes: a
/// `module:qualname` path that names no built-in.
///
/// A path in another grammar — a Rust `crate::module::Type`, or one with no
/// module at all — names nothing an interpreter could import, so it is left
/// for the registry to report as unknown.
pub(crate) fn a_processor_interpreter_describes(node_type: &ProcessorClassImportPath) -> bool {
    if node_type.names_a_built_in_node() {
        return false;
    }
    let path = node_type.as_str();
    match path.split_once(':') {
        Some((module_name, qualname)) => {
            !path.contains("::") && !module_name.is_empty() && !qualname.is_empty()
        }
        None => false,
    }
}

/// The command that describes `import_paths` in `stream_environment`.
///
/// Separate from the run so what a describe carries is assertable without
/// starting one.
pub(crate) fn processor_interpreter_describe_command(
    import_paths: &[ProcessorClassImportPath],
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
) -> Command {
    let mut command = processor_interpreter_bootstrap_command(stream_environment, lend_directory);
    command
        .arg(DESCRIBE_ARGUMENT)
        .args(import_paths.iter().map(ProcessorClassImportPath::as_str))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for per_processor_variable in PER_PROCESSOR_ENVIRONMENT_VARIABLES {
        command.env_remove(per_processor_variable);
    }
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
    give_the_child_no_descriptor_beyond_stdio(&mut command);
    command
}

/// Describe `import_paths` in one start of the stream's interpreter and
/// register each with a constructor that starts its processor interpreter in
/// `stream_environment`, replacing whatever an earlier describe registered.
pub(crate) fn describe_and_register_node_types_in_a_processor_interpreter(
    import_paths: &[ProcessorClassImportPath],
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
) -> Result<()> {
    if import_paths.is_empty() {
        return Ok(());
    }
    let declarations = describe_node_types_in_a_processor_interpreter_within(
        import_paths,
        stream_environment,
        lend_directory,
        PROCESSOR_INTERPRETER_DESCRIBE_BOUND,
    )?;
    for declaration in declarations {
        register_the_described_node_type(declaration, stream_environment, lend_directory)?;
    }
    Ok(())
}

fn register_the_described_node_type(
    declaration: PythonProcessorDeclaration,
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
) -> Result<()> {
    let PythonProcessorDeclaration {
        descriptor,
        execution_config: child_execution_config,
    } = declaration;
    let processor_class_import_path = descriptor.processor_class_import_path.as_str().to_string();
    let descriptor_for_constructor = descriptor.clone();
    let stream_environment = stream_environment.clone();
    let lend_directory = Arc::new(lend_directory.to_path_buf());
    PROCESSOR_REGISTRY.register_a_type_described_in_a_processor_interpreter(
        descriptor,
        Box::new(move |node| {
            Ok(Box::new(spawn_host_for_processor_node(
                &processor_class_import_path,
                &descriptor_for_constructor,
                child_execution_config,
                node,
                &stream_environment,
                &lend_directory,
            )) as Box<dyn DynGeneratedProcessor + Send>)
        }),
    )
}

/// Describe `import_paths` in one start of the stream's interpreter, bounded
/// by `describe_bound`, returning their declarations in the order asked.
pub(crate) fn describe_node_types_in_a_processor_interpreter_within(
    import_paths: &[ProcessorClassImportPath],
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
    describe_bound: Duration,
) -> Result<Vec<PythonProcessorDeclaration>> {
    let refuse_every_requested_type = |refusal: String| Error::NodeTypesNotDescribed {
        node_types: import_paths.to_vec(),
        refusal,
    };
    let interpreter = stream_environment.interpreter.display();

    let mut command =
        processor_interpreter_describe_command(import_paths, stream_environment, lend_directory);
    let mut child =
        spawn_outside_every_iceoryx2_listener_bind(&mut command).map_err(|spawn_failure| {
            refuse_every_requested_type(format!(
                "the stream's interpreter `{interpreter}` could not be started in `{}`: \
                 {spawn_failure}",
                stream_environment.project_directory.display()
            ))
        })?;
    let standard_output = read_on_a_thread(child.stdout.take(), usize::MAX);
    let standard_error = read_on_a_thread(child.stderr.take(), STANDARD_ERROR_TAIL_BYTES);

    let exited_within_the_bound = wait_for_the_describe_to_exit(&child, describe_bound);
    let exit_status = take_the_describe_process_group_down_and_reap(&mut child);
    let standard_error_tail = String::from_utf8_lossy(
        &standard_error
            .recv_timeout(DESCRIBE_STREAM_CLOSE_DEADLINE)
            .unwrap_or_default(),
    )
    .trim()
    .to_string();
    let quoted_standard_error = quote_standard_error(&standard_error_tail);

    if !exited_within_the_bound {
        return Err(refuse_every_requested_type(format!(
            "the stream's interpreter `{interpreter}` did not finish describing them within \
             {}s and was killed. Work a module does at import time runs here — a module that \
             blocks at import blocks its describe.{quoted_standard_error}",
            describe_bound.as_secs_f64()
        )));
    }

    let standard_output_bytes = standard_output
        .recv_timeout(DESCRIBE_STREAM_CLOSE_DEADLINE)
        .unwrap_or_default();
    let exit_status_rendered = exit_status
        .map(|exit_status| exit_status.to_string())
        .unwrap_or_else(|| "an exit status that could not be collected".to_string());
    let Ok(describe_document) =
        serde_json::from_slice::<ProcessorInterpreterDescribeDocument>(&standard_output_bytes)
    else {
        return Err(refuse_every_requested_type(format!(
            "the stream's interpreter `{interpreter}` printed no describe document and exited \
             with {exit_status_rendered}.{quoted_standard_error}"
        )));
    };

    let refused_import_paths: Vec<ProcessorClassImportPath> = import_paths
        .iter()
        .filter(|import_path| {
            describe_document
                .refused_import_paths
                .iter()
                .any(|refused| refused == import_path.as_str())
        })
        .cloned()
        .collect();
    if !refused_import_paths.is_empty() {
        return Err(Error::NodeTypesNotDescribed {
            node_types: refused_import_paths,
            refusal: format!(
                "the stream's interpreter `{interpreter}` could not import it, or found no \
                 `@node` class there.{quoted_standard_error}"
            ),
        });
    }

    import_paths
        .iter()
        .map(|import_path| {
            let refuse_this_type = |refusal: String| Error::NodeTypesNotDescribed {
                node_types: vec![import_path.clone()],
                refusal,
            };
            let described_node_type = describe_document
                .described_node_types
                .iter()
                .find(|described| {
                    described.get("import_path").and_then(|path| path.as_str())
                        == Some(import_path.as_str())
                })
                .ok_or_else(|| {
                    refuse_this_type(format!(
                        "the stream's interpreter `{interpreter}` neither described nor \
                         refused it.{quoted_standard_error}"
                    ))
                })?;
            PythonProcessorDeclaration::read_from_described_node_type(
                described_node_type,
                import_path,
            )
            .map_err(refuse_this_type)
        })
        .collect()
}

/// Wait up to `describe_bound` for the describe to exit, leaving it unreaped.
fn wait_for_the_describe_to_exit(child: &Child, describe_bound: Duration) -> bool {
    let deadline = Instant::now() + describe_bound;
    while !a_helper_process_has_exited_without_being_reaped(child.id()) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(DESCRIBE_EXIT_POLL_INTERVAL);
    }
    true
}

/// Kill the describe's whole process group and reap its leader.
///
/// Killed whether or not the leader already exited: nothing a describe starts
/// outlives it, and a descendant left holding its standard streams would hold
/// the readers open. The leader is still unreaped, so the group id is still
/// its own.
fn take_the_describe_process_group_down_and_reap(
    child: &mut Child,
) -> Option<std::process::ExitStatus> {
    // SAFETY: the pid is this process's own unreaped child's, which leads its
    // own process group.
    unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
    child.wait().ok()
}

/// Read `stream` to its end on a thread of its own, keeping at most the last
/// `kept_byte_count` bytes.
fn read_on_a_thread<R: Read + Send + 'static>(
    stream: Option<R>,
    kept_byte_count: usize,
) -> Receiver<Vec<u8>> {
    let (read_sender, read_receiver) = std::sync::mpsc::channel();
    let Some(mut stream) = stream else {
        let _ = read_sender.send(Vec::new());
        return read_receiver;
    };
    std::thread::spawn(move || {
        let mut kept_bytes: Vec<u8> = Vec::new();
        let mut read_buffer = [0u8; 8192];
        loop {
            match stream.read(&mut read_buffer) {
                Ok(0) | Err(_) => break,
                Ok(read_byte_count) => {
                    kept_bytes.extend_from_slice(&read_buffer[..read_byte_count]);
                    let overflow_byte_count = kept_bytes.len().saturating_sub(kept_byte_count);
                    kept_bytes.drain(..overflow_byte_count);
                }
            }
        }
        let _ = read_sender.send(kept_bytes);
    });
    read_receiver
}

/// The tail of a describe's standard error as a refusal quotes it.
fn quote_standard_error(standard_error_tail: &str) -> String {
    if standard_error_tail.is_empty() {
        return " It wrote nothing to its standard error.".to_string();
    }
    format!(" Its standard error ended with:\n{standard_error_tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    const GOOD_TYPE: &str = "my_app.filters:BlurProcessor";

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
            std::fs::write(&interpreter, format!("#!/bin/sh\n{body}\n"))
                .expect("the stub interpreter is written");
            std::fs::set_permissions(&interpreter, std::fs::Permissions::from_mode(0o755))
                .expect("the stub interpreter is executable");
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
            let import_paths: Vec<_> = import_paths.iter().map(|path| import_path(path)).collect();
            describe_node_types_in_a_processor_interpreter_within(
                &import_paths,
                &self.stream_environment,
                Path::new("/opt/tatolab/lib/tatolab/lend"),
                describe_bound,
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
            "execution": {"mode": "reactive"},
            "scheduling_priority": null,
            "config_schema": {"type": "object"},
            "input_ports": [{"name": "video", "description": "", "delivery_profile": "newest"}],
            "output_ports": [{"name": "blurred", "description": ""}],
        })
    }

    #[test]
    fn a_type_the_interpreter_describes_is_read_into_its_declaration() {
        let stub =
            StubProcessorInterpreter::running(&print_on_standard_output(&serde_json::json!({
                "described_node_types": [a_described_good_type()],
                "refused_import_paths": [],
            })));

        let declarations = stub
            .describe(&[GOOD_TYPE], PROCESSOR_INTERPRETER_DESCRIBE_BOUND)
            .unwrap_or_else(|refusal| panic!("the good type is described: {refusal}"));

        assert_eq!(declarations.len(), 1);
        let descriptor = &declarations[0].descriptor;
        assert_eq!(descriptor.processor_class_import_path.as_str(), GOOD_TYPE);
        assert_eq!(descriptor.inputs[0].name, "video");
        assert_eq!(descriptor.outputs[0].name, "blurred");
        assert_eq!(
            declarations[0].execution_config.execution,
            crate::core::execution::ProcessExecution::Reactive
        );
    }

    #[test]
    fn a_type_whose_module_will_not_import_is_refused_by_name_quoting_the_interpreters_stderr() {
        let stub = StubProcessorInterpreter::running(&format!(
            "{}\nprintf 'cannot describe my_app.missing:Blur: Traceback (most recent call last):\\n\
             ModuleNotFoundError: No module named %s\\n' \"'my_app.missing'\" >&2\nexit 1",
            print_on_standard_output(&serde_json::json!({
                "described_node_types": [a_described_good_type()],
                "refused_import_paths": ["my_app.missing:Blur"],
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
    fn a_class_carrying_no_node_stamp_is_refused_by_name() {
        let stub = StubProcessorInterpreter::running(&format!(
            "{}\nprintf 'cannot describe my_app.nodes:Plain: it carries no @node stamp\\n' >&2\n\
             exit 1",
            print_on_standard_output(&serde_json::json!({
                "described_node_types": [],
                "refused_import_paths": ["my_app.nodes:Plain"],
            }))
        ));

        let (node_types, refusal) = stub.refusal_of(&["my_app.nodes:Plain"]);

        assert_eq!(node_types, [import_path("my_app.nodes:Plain")]);
        assert!(
            refusal.contains("cannot describe my_app.nodes:Plain: it carries no @node stamp"),
            "{refusal}"
        );
    }

    /// An interpreter that cannot load the lent runtime, refuses the build id,
    /// or crashes prints no document, so every type asked for is refused,
    /// naming the interpreter and quoting what it wrote.
    #[test]
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

    #[test]
    fn a_describe_past_its_bound_is_killed_with_its_group_and_refuses_every_type_naming_the_bound()
    {
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
    fn the_describe_command_carries_the_stream_environment_and_no_processors_variables() {
        let stream_environment = StreamEnvironment {
            project_directory: PathBuf::from("/home/someone/my_app"),
            interpreter: PathBuf::from("/home/someone/my_app/.venv/bin/python"),
        };
        let command = processor_interpreter_describe_command(
            &[import_path(GOOD_TYPE), import_path("my_app.nodes:Sharpen")],
            &stream_environment,
            Path::new("/opt/tatolab/lib/tatolab/lend"),
        );

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
        assert_eq!(value_of("PYTHONHOME"), Some(None));
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
    fn a_started_describe_receives_its_arguments_working_directory_and_python_path() {
        let stub = StubProcessorInterpreter::running(&format!(
            "printf '%s\\n' \"$@\" > \"$PWD/arguments\"\nprintf '%s' \"$PYTHONPATH\" > \
             \"$PWD/python-path\"\n{}",
            print_on_standard_output(&serde_json::json!({
                "described_node_types": [a_described_good_type()],
                "refused_import_paths": [],
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
            "tatolab.stream:CameraSource"
        )));
        assert!(!a_processor_interpreter_describes(&import_path(
            "my_crate::processors::Blur"
        )));
        assert!(!a_processor_interpreter_describes(&import_path("Blur")));
    }
}
