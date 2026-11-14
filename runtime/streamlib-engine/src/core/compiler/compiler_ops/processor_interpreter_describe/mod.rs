// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Asking a stream's own interpreter what its node types declare.
//!
//! The runtime process never imports a node's module. It starts the same
//! command a processor interpreter is, with `--describe` and the import paths
//! or with `--describe-installed-modules`, and reads the one JSON document the
//! bootstrap prints on its standard output.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use super::function_node_pool::{
    FUNCTION_NODE_POOL_INTERVAL_MS, FUNCTION_NODE_POOL_NODE_TYPE, the_function_node_pool_descriptor,
};
use super::processor_interpreter_shutdown_ladder::kill_the_process_group_and_reap_its_leader;
use super::processor_interpreter_spawn_host::{
    HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE, LoadedStreamAHelperProcessBelongsTo,
    PROCESSOR_INTERPRETER_ENTRYPOINT_ENVIRONMENT_VARIABLE,
    PROCESSOR_INTERPRETER_NODE_NAME_ENVIRONMENT_VARIABLE,
    PROCESSOR_INTERPRETER_PROCESSOR_ID_ENVIRONMENT_VARIABLE,
    PROCESSOR_INTERPRETER_STREAM_ID_ENVIRONMENT_VARIABLE,
    PROCESSOR_INTERPRETER_STREAM_PROJECT_DIRECTORY_ENVIRONMENT_VARIABLE,
    SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE, StreamInterpreterExitAwaited,
    processor_interpreter_bootstrap_command,
    spawn_host_for_a_function_node_promoted_to_its_own_process, spawn_host_for_processor_node,
    spawn_standard_error_reader_keeping_its_tail, spawn_standard_output_reader_keeping_its_head,
    standard_error_tail_as_a_refusal_quotes_it, standard_output_head_as_a_refusal_quotes_it,
    wait_for_a_stream_interpreter_to_exit,
};
use super::python_processor_declaration::{
    HowAPythonNodeOfTheTypeRuns, PythonProcessorDeclaration,
};
use super::subprocess_bridge::ESCALATE_FD_ENV;
use crate::core::child_process_lifetime::{
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours,
    give_the_child_no_descriptor_beyond_stdio,
};
use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::error::{Error, Result};
use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::processors::{
    DynGeneratedProcessor, HowANodeOfADescribedTypeRuns, InstalledModuleOfANodeType,
    NodeTypesOneStreamResolves,
};
use crate::core::runtime::StreamEnvironment;
use crate::iceoryx2::{
    ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE, spawn_outside_every_iceoryx2_listener_bind,
};

/// How long one describe may run before its process group is killed and every
/// type it was asked for is refused.
pub(crate) const PROCESSOR_INTERPRETER_DESCRIBE_BOUND: Duration = Duration::from_secs(60);

/// How long a describe of the installed modules may run. Shorter than a named
/// type's bound: no graph node waits on it, yet every load does.
pub(crate) const INSTALLED_MODULES_DESCRIBE_BOUND: Duration = Duration::from_secs(15);

/// The bootstrap argument that asks for a describe rather than a processor.
const DESCRIBE_ARGUMENT: &str = "--describe";

/// The bootstrap argument that asks for a describe of every class installed
/// distributions declare in the `tatolab.nodes` entry-point group.
const DESCRIBE_INSTALLED_MODULES_ARGUMENT: &str = "--describe-installed-modules";

/// The variables that name one processor to the interpreter hosting it, none
/// of which a describe carries.
const PER_PROCESSOR_ENVIRONMENT_VARIABLES: [&str; 9] = [
    PROCESSOR_INTERPRETER_ENTRYPOINT_ENVIRONMENT_VARIABLE,
    PROCESSOR_INTERPRETER_PROCESSOR_ID_ENVIRONMENT_VARIABLE,
    PROCESSOR_INTERPRETER_NODE_NAME_ENVIRONMENT_VARIABLE,
    PROCESSOR_INTERPRETER_STREAM_ID_ENVIRONMENT_VARIABLE,
    PROCESSOR_INTERPRETER_STREAM_PROJECT_DIRECTORY_ENVIRONMENT_VARIABLE,
    "STREAMLIB_RUNTIME_ID",
    ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE,
    SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE,
    ESCALATE_FD_ENV,
];

/// What a describe's standard-error lines are logged under, in place of the
/// processor id a processor interpreter's are.
const DESCRIBE_STANDARD_ERROR_LOG_LABEL: &str = "processor-interpreter-describe";

/// Why every type a describe the host interrupted was asked for is refused.
const DESCRIBE_INTERRUPTED_BY_THE_HOST_REFUSAL: &str =
    "the runtime's host interrupted the describe";

/// How much of a describe's standard output is kept; the rest is read and
/// dropped, so the describe never blocks on a full pipe.
const DESCRIBE_STANDARD_OUTPUT_KEPT_BYTES: usize = 16 * 1024 * 1024;

/// The document a describe prints on its standard output.
#[derive(serde::Deserialize)]
struct ProcessorInterpreterDescribeDocument {
    described_node_types: Vec<serde_json::Value>,
    refused_node_types: Vec<NodeTypeTheDescribeRefused>,
    #[serde(default)]
    installed_modules: Vec<NodeClassAnInstalledDistributionDeclares>,
}

/// One `tatolab.nodes` entry point an installed-modules describe found.
#[derive(serde::Deserialize)]
struct NodeClassAnInstalledDistributionDeclares {
    import_path: String,
    entry_point: String,
    distribution: String,
    distribution_version: String,
}

/// What a describe of the installed modules found: each class it described
/// with the distribution declaring it, and each it refused with the reason.
pub(crate) struct NodeTypesTheInstalledModulesDeclare {
    pub(crate) described: Vec<(PythonProcessorDeclaration, InstalledModuleOfANodeType)>,
    pub(crate) refused: Vec<(String, InstalledModuleOfANodeType, String)>,
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
    processor_interpreter_describe_command_with_arguments(
        std::iter::once(DESCRIBE_ARGUMENT)
            .chain(import_paths.iter().map(ProcessorClassImportPath::as_str)),
        stream_environment,
        lend_directory,
    )
}

/// The command that describes every class installed distributions declare in
/// `tatolab.nodes`, in `stream_environment`.
pub(crate) fn processor_interpreter_describe_installed_modules_command(
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
) -> Result<Command> {
    processor_interpreter_describe_command_with_arguments(
        [DESCRIBE_INSTALLED_MODULES_ARGUMENT],
        stream_environment,
        lend_directory,
    )
}

fn processor_interpreter_describe_command_with_arguments<'a>(
    describe_arguments: impl IntoIterator<Item = &'a str>,
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
) -> Result<Command> {
    let mut command = processor_interpreter_bootstrap_command(stream_environment, lend_directory)?;
    // A pipe this process holds open for the describe's whole life: the
    // bootstrap reads its end-of-file as the app gone, which is the only
    // parent-death signal a describe has where `PR_SET_PDEATHSIG` does not exist.
    command
        .args(describe_arguments)
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
/// register each into `node_types_of_the_stream` with a constructor that
/// starts its processor interpreter in `stream_environment`, replacing
/// whatever an earlier describe into that stream registered.
///
/// The describe's process group is registered under the stream's tag, and the
/// stream's shutdown, or `is_interrupted_by_the_host` reporting true, at any
/// point refuses them, killing a describe already started.
pub(crate) fn describe_and_register_node_types_in_a_processor_interpreter(
    node_types_of_the_stream: &NodeTypesOneStreamResolves,
    import_paths: &[ProcessorClassImportPath],
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
    the_stream_the_describe_belongs_to: &LoadedStreamAHelperProcessBelongsTo,
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
        the_stream_the_describe_belongs_to,
        is_interrupted_by_the_host,
    )?;
    for declaration in declarations {
        register_the_described_node_type(
            node_types_of_the_stream,
            declaration,
            stream_environment,
            lend_directory,
            the_stream_the_describe_belongs_to,
        )?;
    }
    Ok(())
}

fn register_the_described_node_type(
    node_types_of_the_stream: &NodeTypesOneStreamResolves,
    declaration: PythonProcessorDeclaration,
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
    the_stream_its_helpers_belong_to: &LoadedStreamAHelperProcessBelongsTo,
) -> Result<()> {
    let PythonProcessorDeclaration {
        descriptor,
        how_a_python_node_of_the_type_runs,
    } = declaration;
    let processor_class_import_path = descriptor.processor_class_import_path.as_str().to_string();
    let descriptor_for_constructor = descriptor.clone();
    let stream_environment = stream_environment.clone();
    let lend_directory = Arc::new(lend_directory.to_path_buf());
    let the_stream_its_helpers_belong_to = the_stream_its_helpers_belong_to.clone();
    let child_execution_config = match how_a_python_node_of_the_type_runs {
        HowAPythonNodeOfTheTypeRuns::InItsOwnProcessorInterpreter(execution_config) => {
            node_types_of_the_stream.record_the_execution_a_described_class_declares(
                descriptor.processor_class_import_path.clone(),
                execution_config.execution,
            );
            execution_config
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        HowAPythonNodeOfTheTypeRuns::OnItsNativeShaderEvaluator(program) => {
            return node_types_of_the_stream
                    .register_a_type_described_in_this_streams_processor_interpreter(
                        descriptor,
                        HowANodeOfADescribedTypeRuns::OnTheNativeShaderEvaluatorOfItsIsland {
                            shader_node_program: program.clone(),
                            evaluator_of_the_island_it_hosts: Box::new(move |node| {
                                Ok(Box::new(
                                    super::shader_node_native_evaluator::ShaderNodeNativeEvaluatorProcessor::for_the_node(
                                        node,
                                        descriptor_for_constructor.clone(),
                                        program.clone(),
                                    ),
                                )
                                    as Box<dyn DynGeneratedProcessor + Send>)
                            }),
                        },
                    );
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        HowAPythonNodeOfTheTypeRuns::OnItsNativeShaderEvaluator(_) => {
            return Err(Error::NotSupported(format!(
                "shader node `{processor_class_import_path}` runs on the GPU through Vulkan, \
                     which this platform's runtime does not drive"
            )));
        }
        HowAPythonNodeOfTheTypeRuns::InsideTheReadOfEachNodeItFeeds => {
            register_the_function_node_pool_once(
                node_types_of_the_stream,
                &stream_environment,
                &lend_directory,
                &the_stream_its_helpers_belong_to,
            )?;
            return node_types_of_the_stream
                .register_a_type_described_in_this_streams_processor_interpreter(
                    descriptor,
                    HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds {
                        processor_once_promoted_to_its_own_process: Box::new(move |node| {
                            Ok(
                                Box::new(
                                    spawn_host_for_a_function_node_promoted_to_its_own_process(
                                        &processor_class_import_path,
                                        &descriptor_for_constructor,
                                        node,
                                        &stream_environment,
                                        &lend_directory,
                                        the_stream_its_helpers_belong_to.clone(),
                                    )?,
                                )
                                    as Box<dyn DynGeneratedProcessor + Send>,
                            )
                        }),
                    },
                );
        }
    };
    node_types_of_the_stream.register_a_type_described_in_this_streams_processor_interpreter(
        descriptor,
        HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(Box::new(move |node| {
            Ok(Box::new(spawn_host_for_processor_node(
                &processor_class_import_path,
                &descriptor_for_constructor,
                child_execution_config,
                node,
                &stream_environment,
                &lend_directory,
                the_stream_its_helpers_belong_to.clone(),
            )) as Box<dyn DynGeneratedProcessor + Send>)
        })),
    )
}

/// Register the stream's function node pool, whose helper evaluates each
/// island no reader of it can evaluate, once the stream describes a function
/// node type; its ports are the ones each carried island adds to its node.
fn register_the_function_node_pool_once(
    node_types_of_the_stream: &NodeTypesOneStreamResolves,
    stream_environment: &StreamEnvironment,
    lend_directory: &Arc<std::path::PathBuf>,
    the_stream_its_helpers_belong_to: &LoadedStreamAHelperProcessBelongsTo,
) -> Result<()> {
    let pool_descriptor = the_function_node_pool_descriptor(None)?;
    if node_types_of_the_stream.was_described_in_this_streams_processor_interpreter(
        &pool_descriptor.processor_class_import_path,
    ) {
        return Ok(());
    }
    let stream_environment = stream_environment.clone();
    let lend_directory = Arc::clone(lend_directory);
    let the_stream_its_helpers_belong_to = the_stream_its_helpers_belong_to.clone();
    node_types_of_the_stream.register_a_type_described_in_this_streams_processor_interpreter(
        pool_descriptor,
        HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(Box::new(move |node| {
            let descriptor =
                the_function_node_pool_descriptor(Some((&node.ports.inputs, &node.ports.outputs)))?;
            Ok(Box::new(spawn_host_for_processor_node(
                FUNCTION_NODE_POOL_NODE_TYPE,
                &descriptor,
                ExecutionConfig::new(ProcessExecution::Continuous {
                    interval_ms: FUNCTION_NODE_POOL_INTERVAL_MS,
                }),
                node,
                &stream_environment,
                &lend_directory,
                the_stream_its_helpers_belong_to.clone(),
            )) as Box<dyn DynGeneratedProcessor + Send>)
        })),
    )
}

/// Describe `import_paths` in one start of the stream's interpreter, bounded
/// by `describe_bound` and cut short by the shutdown of the stream it belongs
/// to or by `is_interrupted_by_the_host` reporting true, returning their
/// declarations in the order asked.
pub(crate) fn describe_node_types_in_a_processor_interpreter_within(
    import_paths: &[ProcessorClassImportPath],
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
    describe_bound: Duration,
    the_stream_the_describe_belongs_to: &LoadedStreamAHelperProcessBelongsTo,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
) -> Result<Vec<PythonProcessorDeclaration>> {
    let refuse_every_requested_type = |refusal: String| Error::NodeTypesNotDescribed {
        node_types: import_paths.to_vec(),
        refusal,
    };
    let interpreter = stream_environment.interpreter.display();
    let (describe_document, quoted_standard_error) = run_a_describe_reading_its_document(
        || processor_interpreter_describe_command(import_paths, stream_environment, lend_directory),
        stream_environment,
        describe_bound,
        the_stream_the_describe_belongs_to,
        is_interrupted_by_the_host,
        &refuse_every_requested_type,
    )?;

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

/// Start the describe `describe_command` builds, bounded by `describe_bound`
/// and cut short by the stream's shutdown or the host's interrupt, and read
/// the document it printed, every failure refused through `refuse`.
fn run_a_describe_reading_its_document(
    describe_command: impl FnOnce() -> Result<Command>,
    stream_environment: &StreamEnvironment,
    describe_bound: Duration,
    the_stream_the_describe_belongs_to: &LoadedStreamAHelperProcessBelongsTo,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
    refuse_every_requested_type: &dyn Fn(String) -> Error,
) -> Result<(ProcessorInterpreterDescribeDocument, String)> {
    let interpreter = stream_environment.interpreter.display();
    if is_interrupted_by_the_host() {
        return Err(refuse_every_requested_type(
            DESCRIBE_INTERRUPTED_BY_THE_HOST_REFUSAL.to_string(),
        ));
    }

    let mut command =
        describe_command().map_err(|refusal| refuse_every_requested_type(refusal.to_string()))?;
    let mut child =
        spawn_outside_every_iceoryx2_listener_bind(&mut command).map_err(|spawn_failure| {
            refuse_every_requested_type(format!(
                "the stream's interpreter `{interpreter}` could not be started in `{}`: \
                 {spawn_failure}",
                stream_environment.project_directory.display()
            ))
        })?;
    // `pre_exec` made the describe the leader of a group whose id is its pid.
    if !crate::core::runtime::register_a_helper_process_group(
        child.id() as i32,
        the_stream_the_describe_belongs_to.stream_tag,
    ) {
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

    let describe_exit = wait_for_a_stream_interpreter_to_exit(
        &child,
        describe_bound,
        &|| {
            the_stream_the_describe_belongs_to
                .shutdown_escalation
                .is_requested()
        },
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
        StreamInterpreterExitAwaited::Exited => {}
        StreamInterpreterExitAwaited::BoundElapsed => {
            return Err(refuse_every_requested_type(format!(
                "the stream's interpreter `{interpreter}` did not finish describing them within \
                 {}s and was killed. Work a module does at import time runs here — a module that \
                 blocks at import blocks its describe. {quoted_standard_error}",
                describe_bound.as_secs_f64()
            )));
        }
        StreamInterpreterExitAwaited::ItsStreamsShutdownRequested => {
            return Err(refuse_every_requested_type(format!(
                "the stream's shutdown began while its interpreter `{interpreter}` was still \
                 describing them, and it was killed. {quoted_standard_error}"
            )));
        }
        StreamInterpreterExitAwaited::InterruptedByTheHost => {
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
    Ok((describe_document, quoted_standard_error))
}

/// Describe every class installed distributions declare in `tatolab.nodes`, in
/// one start of the stream's interpreter, bounded and cut short like
/// [`describe_node_types_in_a_processor_interpreter_within`].
pub(crate) fn describe_the_node_types_installed_modules_declare_within(
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
    describe_bound: Duration,
    the_stream_the_describe_belongs_to: &LoadedStreamAHelperProcessBelongsTo,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
) -> Result<NodeTypesTheInstalledModulesDeclare> {
    let refuse_the_installed_modules = |refusal: String| Error::Runtime(refusal);
    let (describe_document, _) = run_a_describe_reading_its_document(
        || {
            processor_interpreter_describe_installed_modules_command(
                stream_environment,
                lend_directory,
            )
        },
        stream_environment,
        describe_bound,
        the_stream_the_describe_belongs_to,
        is_interrupted_by_the_host,
        &refuse_the_installed_modules,
    )?;
    let mut node_types_the_installed_modules_declare = NodeTypesTheInstalledModulesDeclare {
        described: Vec::new(),
        refused: Vec::new(),
    };
    for declared in describe_document.installed_modules {
        let installed_module = InstalledModuleOfANodeType {
            distribution: declared.distribution,
            distribution_version: declared.distribution_version,
            entry_point: declared.entry_point,
        };
        let refused = describe_document
            .refused_node_types
            .iter()
            .find(|refused| refused.import_path == declared.import_path);
        if let Some(refused) = refused {
            node_types_the_installed_modules_declare.refused.push((
                declared.import_path,
                installed_module,
                refused.refusal.trim_end().to_string(),
            ));
            continue;
        }
        let read_declaration = ProcessorClassImportPath::new(&declared.import_path)
            .map_err(|not_an_import_path| not_an_import_path.to_string())
            .and_then(|import_path| {
                let described_node_type = describe_document
                    .described_node_types
                    .iter()
                    .find(|described| {
                        described.get("import_path").and_then(|path| path.as_str())
                            == Some(import_path.as_str())
                    })
                    .ok_or_else(|| "the describe neither described nor refused it".to_string())?;
                PythonProcessorDeclaration::read_from_described_node_type(
                    described_node_type,
                    &import_path,
                )
            });
        match read_declaration {
            Ok(declaration) => node_types_the_installed_modules_declare
                .described
                .push((declaration, installed_module)),
            Err(refusal) => node_types_the_installed_modules_declare.refused.push((
                declared.import_path,
                installed_module,
                refusal,
            )),
        }
    }
    Ok(node_types_the_installed_modules_declare)
}

/// Register every class installed distributions declare into
/// `node_types_of_the_stream`, each recorded under the distribution declaring
/// it. A class a natively compiled type already holds is skipped, and one the
/// stream already described keeps its registration and gains the record.
pub(crate) fn register_the_node_types_installed_modules_declare(
    node_types_of_the_stream: &NodeTypesOneStreamResolves,
    node_types_the_installed_modules_declare: NodeTypesTheInstalledModulesDeclare,
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
    the_stream_its_helpers_belong_to: &LoadedStreamAHelperProcessBelongsTo,
) -> Vec<ProcessorClassImportPath> {
    for (import_path, installed_module, refusal) in node_types_the_installed_modules_declare.refused
    {
        tracing::warn!(
            processor_class_import_path = import_path.as_str(),
            distribution = installed_module.distribution.as_str(),
            "an installed module's node type was not described, so the stream cannot add it: \
             {refusal}"
        );
    }
    let mut registered = Vec::new();
    for (declaration, installed_module) in node_types_the_installed_modules_declare.described {
        let node_type = declaration.descriptor.processor_class_import_path.clone();
        if node_types_of_the_stream.is_a_natively_compiled_node_type(&node_type) {
            tracing::warn!(
                processor_class_import_path = node_type.as_str(),
                distribution = installed_module.distribution.as_str(),
                "an installed module declares a node type a natively compiled one already holds, \
                 so it is left out"
            );
            continue;
        }
        if !node_types_of_the_stream.was_described_in_this_streams_processor_interpreter(&node_type)
        {
            if let Err(refusal) = register_the_described_node_type(
                node_types_of_the_stream,
                declaration,
                stream_environment,
                lend_directory,
                the_stream_its_helpers_belong_to,
            ) {
                tracing::warn!(
                    processor_class_import_path = node_type.as_str(),
                    distribution = installed_module.distribution.as_str(),
                    "an installed module's node type was refused: {refusal}"
                );
                continue;
            }
            registered.push(node_type.clone());
        }
        node_types_of_the_stream.record_the_installed_module_declaring(node_type, installed_module);
    }
    registered
}

#[cfg(test)]
mod tests;
