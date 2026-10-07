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
    "STREAMLIB_ENTRYPOINT",
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
