// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Asking a stream's own interpreter what its node types declare.
//!
//! The runtime process never imports a node's module. It starts the same
//! command a processor interpreter is, with `--describe` and the import paths,
//! and reads the one JSON document the bootstrap prints on its standard output.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::processor_interpreter_shutdown_ladder::{
    kill_the_process_group_and_reap_its_leader, wait_for_a_child_to_become_collectable_within,
};
use super::processor_interpreter_spawn_host::{
    HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE,
    PROCESSOR_INTERPRETER_ENTRYPOINT_ENVIRONMENT_VARIABLE,
    PROCESSOR_INTERPRETER_PROCESSOR_ID_ENVIRONMENT_VARIABLE,
    SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE,
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours,
    give_the_child_no_descriptor_beyond_stdio, processor_interpreter_bootstrap_command,
    spawn_host_for_processor_node, spawn_standard_error_reader_keeping_its_tail,
    spawn_standard_output_reader_keeping_its_head, standard_error_tail_as_a_refusal_quotes_it,
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

/// How long the wait for a describe parks before it re-reads whether shutdown
/// has begun.
const DESCRIBE_SHUTDOWN_OBSERVATION_INTERVAL: Duration = Duration::from_millis(50);

/// What a describe's standard-error lines are logged under, in place of the
/// processor id a processor interpreter's are.
const DESCRIBE_STANDARD_ERROR_LOG_LABEL: &str = "processor-interpreter-describe";

/// Why every type a describe the host interrupted was asked for is refused.
const DESCRIBE_INTERRUPTED_BY_THE_HOST_REFUSAL: &str =
    "the runtime's host interrupted the describe";

/// How much of a describe's standard output is kept; the rest is read and
/// dropped, so the describe never blocks on a full pipe.
const DESCRIBE_STANDARD_OUTPUT_KEPT_BYTES: usize = 16 * 1024 * 1024;

/// How much of a standard output that is not a describe document a refusal
/// quotes.
const DESCRIBE_STANDARD_OUTPUT_QUOTED_BYTES: usize = 512;

/// The document a describe prints on its standard output.
#[derive(serde::Deserialize)]
struct ProcessorInterpreterDescribeDocument {
    described_node_types: Vec<serde_json::Value>,
    refused_node_types: Vec<NodeTypeTheDescribeRefused>,
}

/// One type a describe refused, with the reason the bootstrap gave for it.
#[derive(serde::Deserialize)]
struct NodeTypeTheDescribeRefused {
    import_path: String,
    refusal: String,
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
) -> Result<Command> {
    let mut command = processor_interpreter_bootstrap_command(stream_environment, lend_directory)?;
    // A pipe this process holds open for the describe's whole life: the
    // bootstrap reads its end-of-file as the app gone, which is the only
    // parent-death signal a describe has where `PR_SET_PDEATHSIG` does not exist.
    command
        .arg(DESCRIBE_ARGUMENT)
        .args(import_paths.iter().map(ProcessorClassImportPath::as_str))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for per_processor_variable in PER_PROCESSOR_ENVIRONMENT_VARIABLES {
        command.env_remove(per_processor_variable);
    }
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
    give_the_child_no_descriptor_beyond_stdio(&mut command);
    Ok(command)
}

/// Describe `import_paths` in one start of the stream's interpreter and
/// register each with a constructor that starts its processor interpreter in
/// `stream_environment`, replacing whatever an earlier describe registered.
///
/// `is_interrupted_by_the_host` reporting true at any point refuses them,
/// killing a describe already started.
pub(crate) fn describe_and_register_node_types_in_a_processor_interpreter(
    import_paths: &[ProcessorClassImportPath],
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
) -> Result<()> {
    if import_paths.is_empty() {
        return Ok(());
    }
    let declarations = describe_node_types_in_a_processor_interpreter_within(
        import_paths,
        stream_environment,
        lend_directory,
        PROCESSOR_INTERPRETER_DESCRIBE_BOUND,
        crate::core::runtime::is_runtime_shutdown_requested,
        is_interrupted_by_the_host,
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
/// by `describe_bound` and cut short by a shutdown `is_shutdown_requested`
/// reports during it or by `is_interrupted_by_the_host` reporting true,
/// returning their declarations in the order asked.
pub(crate) fn describe_node_types_in_a_processor_interpreter_within(
    import_paths: &[ProcessorClassImportPath],
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
    describe_bound: Duration,
    is_shutdown_requested: fn() -> bool,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
) -> Result<Vec<PythonProcessorDeclaration>> {
    let refuse_every_requested_type = |refusal: String| Error::NodeTypesNotDescribed {
        node_types: import_paths.to_vec(),
        refusal,
    };
    let interpreter = stream_environment.interpreter.display();
    if is_interrupted_by_the_host() {
        return Err(refuse_every_requested_type(
            DESCRIBE_INTERRUPTED_BY_THE_HOST_REFUSAL.to_string(),
        ));
    }

    let mut command =
        processor_interpreter_describe_command(import_paths, stream_environment, lend_directory)
            .map_err(|refusal| refuse_every_requested_type(refusal.to_string()))?;
    let mut child =
        spawn_outside_every_iceoryx2_listener_bind(&mut command).map_err(|spawn_failure| {
            refuse_every_requested_type(format!(
                "the stream's interpreter `{interpreter}` could not be started in `{}`: \
                 {spawn_failure}",
                stream_environment.project_directory.display()
            ))
        })?;
    // `pre_exec` made the describe the leader of a group whose id is its pid.
    if !crate::core::runtime::register_a_helper_process_group(child.id() as i32) {
        tracing::warn!(
            "the describe's process group could not be registered, so a third interrupt will not \
             kill it; the describe still ends itself when the app exits"
        );
    }
    let standard_output_head = child.stdout.take().map(|standard_output| {
        spawn_standard_output_reader_keeping_its_head(
            standard_output,
            DESCRIBE_STANDARD_OUTPUT_KEPT_BYTES,
        )
    });
    let standard_error_tail = child.stderr.take().map(|standard_error| {
        spawn_standard_error_reader_keeping_its_tail(
            standard_error,
            DESCRIBE_STANDARD_ERROR_LOG_LABEL,
        )
    });

    let describe_exit = wait_for_the_describe_to_exit(
        &child,
        describe_bound,
        is_shutdown_requested,
        is_interrupted_by_the_host,
    );
    // Killed whether or not the leader already exited: nothing a describe
    // starts outlives it, and a descendant left holding its standard streams
    // would hold the readers open.
    let exit_status = kill_the_process_group_and_reap_its_leader(&mut child, "the describe");
    let quoted_standard_error = standard_error_tail_as_a_refusal_quotes_it(
        &standard_error_tail
            .map(|tail| {
                tail.text_once_closed_or_after(HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE)
            })
            .unwrap_or_default(),
    );

    match describe_exit {
        DescribeExitAwaited::Exited => {}
        DescribeExitAwaited::BoundElapsed => {
            return Err(refuse_every_requested_type(format!(
                "the stream's interpreter `{interpreter}` did not finish describing them within \
                 {}s and was killed. Work a module does at import time runs here — a module that \
                 blocks at import blocks its describe. {quoted_standard_error}",
                describe_bound.as_secs_f64()
            )));
        }
        DescribeExitAwaited::ShutdownRequested => {
            return Err(refuse_every_requested_type(format!(
                "shutdown began while the stream's interpreter `{interpreter}` was still \
                 describing them, and it was killed. {quoted_standard_error}"
            )));
        }
        DescribeExitAwaited::InterruptedByTheHost => {
            return Err(refuse_every_requested_type(format!(
                "{DESCRIBE_INTERRUPTED_BY_THE_HOST_REFUSAL}: the stream's interpreter \
                 `{interpreter}` was killed. {quoted_standard_error}"
            )));
        }
    }

    // What arrived before the leader exited, whether or not a descendant that
    // left its group still holds the pipe open.
    let standard_output_bytes = standard_output_head
        .map(|head| head.bytes_once_closed_or_after(HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE))
        .unwrap_or_default();
    let exit_status_rendered = exit_status
        .map(|exit_status| exit_status.to_string())
        .unwrap_or_else(|| "an exit status that could not be collected".to_string());
    if standard_output_bytes.is_empty() {
        return Err(refuse_every_requested_type(format!(
            "the stream's interpreter `{interpreter}` printed no describe document and exited \
             with {exit_status_rendered}. {quoted_standard_error}"
        )));
    }
    let describe_document = match serde_json::from_slice::<ProcessorInterpreterDescribeDocument>(
        &standard_output_bytes,
    ) {
        Ok(describe_document) => describe_document,
        Err(not_a_describe_document) => {
            return Err(refuse_every_requested_type(format!(
                "the stream's interpreter `{interpreter}` exited with {exit_status_rendered}, \
                 and its standard output is not a describe document \
                 ({not_a_describe_document}); it began {}. What a `sitecustomize`, \
                 `usercustomize` or `.pth` hook in the stream's environment prints lands ahead \
                 of the document. {quoted_standard_error}",
                standard_output_head_as_a_refusal_quotes_it(&standard_output_bytes)
            )));
        }
    };

    let refused_node_types: Vec<(ProcessorClassImportPath, &str)> = import_paths
        .iter()
        .filter_map(|import_path| {
            describe_document
                .refused_node_types
                .iter()
                .find(|refused| refused.import_path == import_path.as_str())
                .map(|refused| (import_path.clone(), refused.refusal.trim_end()))
        })
        .collect();
    if !refused_node_types.is_empty() {
        let refusal = match refused_node_types.as_slice() {
            [(_, the_only_refusal)] => {
                format!("the stream's interpreter `{interpreter}` refused it: {the_only_refusal}")
            }
            several_refused_node_types => format!(
                "the stream's interpreter `{interpreter}` refused them:\n{}",
                several_refused_node_types
                    .iter()
                    .map(|(import_path, refusal)| format!("`{}`: {refusal}", import_path.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        };
        return Err(Error::NodeTypesNotDescribed {
            node_types: refused_node_types
                .into_iter()
                .map(|(import_path, _)| import_path)
                .collect(),
            refusal,
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
                         refused it. {quoted_standard_error}"
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

/// How the wait for a describe to exit ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DescribeExitAwaited {
    Exited,
    BoundElapsed,
    ShutdownRequested,
    InterruptedByTheHost,
}

/// Wait up to `describe_bound` for the describe to exit, leaving it unreaped.
///
/// Only a shutdown requested *during* the wait cuts it short: the escalation is
/// process-global and taken only when a run ends, so one already raised belongs
/// to a run that has not taken it yet. A host interrupt cuts it short whenever
/// it is read.
fn wait_for_the_describe_to_exit(
    child: &Child,
    describe_bound: Duration,
    is_shutdown_requested: fn() -> bool,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
) -> DescribeExitAwaited {
    let shutdown_was_already_requested = is_shutdown_requested();
    let deadline = Instant::now() + describe_bound;
    loop {
        let observation_slice = deadline
            .saturating_duration_since(Instant::now())
            .min(DESCRIBE_SHUTDOWN_OBSERVATION_INTERVAL);
        if wait_for_a_child_to_become_collectable_within(child, observation_slice) {
            return DescribeExitAwaited::Exited;
        }
        if !shutdown_was_already_requested && is_shutdown_requested() {
            return DescribeExitAwaited::ShutdownRequested;
        }
        if is_interrupted_by_the_host() {
            return DescribeExitAwaited::InterruptedByTheHost;
        }
        if Instant::now() >= deadline {
            return DescribeExitAwaited::BoundElapsed;
        }
    }
}

/// The first [`DESCRIBE_STANDARD_OUTPUT_QUOTED_BYTES`] of a standard output, quoted.
fn standard_output_head_as_a_refusal_quotes_it(standard_output_bytes: &[u8]) -> String {
    let head = &standard_output_bytes[..standard_output_bytes
        .len()
        .min(DESCRIBE_STANDARD_OUTPUT_QUOTED_BYTES)];
    let ellipsis = if head.len() < standard_output_bytes.len() {
        "…"
    } else {
        ""
    };
    format!("{:?}{ellipsis}", String::from_utf8_lossy(head))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
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
            self.describe_reading_a_shutdown_from(import_paths, describe_bound, || false)
        }

        fn describe_reading_a_shutdown_from(
            &self,
            import_paths: &[&str],
            describe_bound: Duration,
            is_shutdown_requested: fn() -> bool,
        ) -> Result<Vec<PythonProcessorDeclaration>> {
            let import_paths: Vec<_> = import_paths.iter().map(|path| import_path(path)).collect();
            describe_node_types_in_a_processor_interpreter_within(
                &import_paths,
                &self.stream_environment,
                Path::new("/opt/tatolab/lib/tatolab/lend"),
                describe_bound,
                is_shutdown_requested,
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
            "execution": {"mode": "reactive"},
            "scheduling_priority": null,
            "config_schema": {"type": "object"},
            "input_ports": [{"name": "video", "description": "", "delivery_profile": "newest"}],
            "output_ports": [{"name": "blurred", "description": ""}],
        })
    }

    #[test]
    #[serial]
    fn a_type_the_interpreter_describes_is_read_into_its_declaration() {
        let stub =
            StubProcessorInterpreter::running(&print_on_standard_output(&serde_json::json!({
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
        assert_eq!(
            declarations[0].execution_config.execution,
            crate::core::execution::ProcessExecution::Reactive
        );
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
    fn a_document_printed_before_exiting_is_read_while_an_escaped_descendant_holds_standard_output()
    {
        let stub = StubProcessorInterpreter::running(&format!(
            "python3 -c 'import os; os.setsid(); \
             open(\"escaped-process-id\", \"w\").write(str(os.getpid())); \
             os.execvp(\"sleep\", [\"sleep\", \"30\"])' &\n{}\nexit 0",
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
                    "refusal": "the class `Plain` carries no `@node` declaration, so it is not a node",
                }],
            }))
        ));

        let (node_types, refusal) = stub.refusal_of(&["my_app.nodes:Plain"]);

        assert_eq!(node_types, [import_path("my_app.nodes:Plain")]);
        assert!(
            refusal.contains("the class `Plain` carries no `@node` declaration"),
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

    /// How many times the shutdown predicate of the test below has been read.
    static SHUTDOWN_READS_OF_THE_INTERRUPTED_DESCRIBE: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    /// Reports no shutdown when the describe starts and one at every read after.
    fn shutdown_requested_once_the_describe_has_started() -> bool {
        SHUTDOWN_READS_OF_THE_INTERRUPTED_DESCRIBE.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            > 0
    }

    #[test]
    #[serial]
    fn a_shutdown_requested_during_a_describe_kills_it_and_refuses_every_type_saying_so() {
        let stub = StubProcessorInterpreter::running("sleep 30 &\nwait");
        let started = Instant::now();

        let refusal = match stub.describe_reading_a_shutdown_from(
            &[GOOD_TYPE],
            PROCESSOR_INTERPRETER_DESCRIBE_BOUND,
            shutdown_requested_once_the_describe_has_started,
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
            || false,
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
            || false,
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
            "tatolab.stream:CameraSource"
        )));
        assert!(!a_processor_interpreter_describes(&import_path(
            "my_crate::processors::Blur"
        )));
        assert!(!a_processor_interpreter_describes(&import_path("Blur")));
    }
}
