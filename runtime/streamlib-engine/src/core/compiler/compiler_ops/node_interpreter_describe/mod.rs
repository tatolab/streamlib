// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use super::function_node_pool::{
    FUNCTION_NODE_POOL_INTERVAL_MS, FUNCTION_NODE_POOL_NODE_TYPE, the_function_node_pool_descriptor,
};
use super::node_interpreter_shutdown_ladder::kill_the_process_group_and_reap_its_leader;
use super::node_interpreter_spawn_host::{
    HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE, LoadedStreamAHelperProcessBelongsTo,
    NODE_INTERPRETER_ENTRYPOINT_ENVIRONMENT_VARIABLE,
    NODE_INTERPRETER_NODE_NAME_ENVIRONMENT_VARIABLE,
    NODE_INTERPRETER_NODE_RUN_ID_ENVIRONMENT_VARIABLE,
    NODE_INTERPRETER_STREAM_ID_ENVIRONMENT_VARIABLE,
    NODE_INTERPRETER_STREAM_PROJECT_DIRECTORY_ENVIRONMENT_VARIABLE,
    SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE, StreamInterpreterExitAwaited,
    node_interpreter_bootstrap_command, spawn_host_for_a_function_node_promoted_to_its_own_process,
    spawn_host_for_graph_node, spawn_standard_error_reader_keeping_its_tail,
    spawn_standard_output_reader_keeping_its_head, standard_error_tail_as_a_refusal_quotes_it,
    standard_output_head_as_a_refusal_quotes_it, wait_for_a_stream_interpreter_to_exit,
};
use super::python_node_declaration::{HowAPythonNodeOfTheTypeRuns, PythonNodeDeclaration};
use super::subprocess_bridge::ESCALATE_FD_ENV;
use crate::core::child_process_lifetime::{
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours,
    give_the_child_no_descriptor_beyond_stdio,
};
use crate::core::descriptors::{NodeClassImportPath, NodeDescriptor};
use crate::core::error::{Error, Result};
use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::nodes::{
    DynGeneratedNode, HowANodeOfADescribedTypeRuns, InstalledModuleOfANodeType,
    NodeTypesOneStreamResolves,
};
use crate::core::runtime::StreamEnvironment;
use crate::iceoryx2::{
    ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE, spawn_outside_every_iceoryx2_listener_bind,
};

/// How long one describe may run before its process group is killed and every
/// type it was asked for is refused.
pub(crate) const NODE_INTERPRETER_DESCRIBE_BOUND: Duration = Duration::from_secs(60);

/// How long the describe run at a stream's launch may run. Shorter than a named
/// type's bound: no graph node waits on it, yet every load does.
pub(crate) const LAUNCH_DESCRIBE_BOUND: Duration = Duration::from_secs(15);

const DESCRIBE_ARGUMENT: &str = "--describe";

/// The bootstrap argument that asks for a describe of every class installed
/// distributions declare in the `tatolab.nodes` entry-point group.
const DESCRIBE_INSTALLED_MODULES_ARGUMENT: &str = "--describe-installed-modules";

const PER_NODE_ENVIRONMENT_VARIABLES: [&str; 9] = [
    NODE_INTERPRETER_ENTRYPOINT_ENVIRONMENT_VARIABLE,
    NODE_INTERPRETER_NODE_RUN_ID_ENVIRONMENT_VARIABLE,
    NODE_INTERPRETER_NODE_NAME_ENVIRONMENT_VARIABLE,
    NODE_INTERPRETER_STREAM_ID_ENVIRONMENT_VARIABLE,
    NODE_INTERPRETER_STREAM_PROJECT_DIRECTORY_ENVIRONMENT_VARIABLE,
    "STREAMLIB_RUNTIME_ID",
    ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE,
    SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE,
    ESCALATE_FD_ENV,
];

const DESCRIBE_STANDARD_ERROR_LOG_LABEL: &str = "node-interpreter-describe";

/// Why a run out of stream named `run_name` that the host interrupted is refused: every type
/// a describe was asked for, or the cast a cast was asked for.
fn interrupted_by_the_host_refusal(run_name: &str) -> String {
    format!("the runtime's host interrupted the {run_name}")
}

/// How much of a describe's standard output is kept; the rest is read and
/// dropped, so the describe never blocks on a full pipe.
const DESCRIBE_STANDARD_OUTPUT_KEPT_BYTES: usize = 16 * 1024 * 1024;

/// The document a describe prints on its standard output.
#[derive(serde::Deserialize)]
struct NodeInterpreterDescribeDocument {
    described_node_types: Vec<serde_json::Value>,
    refused_node_types: Vec<NodeTypeTheDescribeRefused>,
    #[serde(default)]
    installed_modules: Vec<NodeClassAnInstalledDistributionDeclares>,
    #[serde(default)]
    project_node_types: Vec<String>,
}

/// One `tatolab.nodes` entry point an installed-modules describe found.
#[derive(serde::Deserialize)]
struct NodeClassAnInstalledDistributionDeclares {
    import_path: String,
    entry_point: String,
    distribution: String,
    distribution_version: String,
}

/// What the describe run at a stream's launch found: each installed class it described
/// with the distribution declaring it, and each it refused with the reason; and
/// beside them each class of the project's own code it described or refused.
pub(crate) struct NodeTypesDescribedAtTheStreamsLaunch {
    pub(crate) described: Vec<(PythonNodeDeclaration, InstalledModuleOfANodeType)>,
    pub(crate) refused: Vec<(String, InstalledModuleOfANodeType, String)>,
    pub(crate) described_of_the_projects_own_code: Vec<PythonNodeDeclaration>,
    pub(crate) refused_of_the_projects_own_code: Vec<(String, String)>,
}

/// One type a describe refused, with the reason the bootstrap gave for it.
#[derive(serde::Deserialize)]
struct NodeTypeTheDescribeRefused {
    import_path: String,
    refusal: String,
}

pub(crate) fn a_node_interpreter_describes(node_type: &NodeClassImportPath) -> bool {
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
pub(crate) fn node_interpreter_describe_command(
    import_paths: &[NodeClassImportPath],
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
) -> Result<Command> {
    node_interpreter_out_of_stream_command_with_arguments(
        std::iter::once(DESCRIBE_ARGUMENT)
            .chain(import_paths.iter().map(NodeClassImportPath::as_str)),
        stream_environment,
        lend_directory,
    )
}

/// The command that describes every class installed distributions declare in
/// `tatolab.nodes`, in `stream_environment`.
pub(crate) fn node_interpreter_describe_installed_modules_command(
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
) -> Result<Command> {
    node_interpreter_out_of_stream_command_with_arguments(
        [DESCRIBE_INSTALLED_MODULES_ARGUMENT],
        stream_environment,
        lend_directory,
    )
}

/// The command that runs the stream's node interpreter out of stream with `bootstrap_arguments`,
/// as a describe or a cast runs it: holding a pipe on its standard input, its standard output
/// and error read, and no per-node variable or descriptor beyond its standard streams.
pub(crate) fn node_interpreter_out_of_stream_command_with_arguments<'a>(
    bootstrap_arguments: impl IntoIterator<Item = &'a str>,
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
) -> Result<Command> {
    let mut command = node_interpreter_bootstrap_command(stream_environment, lend_directory)?;
    // A pipe this process holds open for the run's whole life: the bootstrap
    // reads its end-of-file as the app gone, which is the only parent-death
    // signal a run out of stream has where `PR_SET_PDEATHSIG` does not exist.
    command
        .args(bootstrap_arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for per_node_variable in PER_NODE_ENVIRONMENT_VARIABLES {
        command.env_remove(per_node_variable);
    }
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
    give_the_child_no_descriptor_beyond_stdio(&mut command);
    Ok(command)
}

pub(crate) fn describe_and_register_node_types_in_a_node_interpreter(
    node_types_of_the_stream: &NodeTypesOneStreamResolves,
    import_paths: &[NodeClassImportPath],
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
    the_stream_the_describe_belongs_to: &LoadedStreamAHelperProcessBelongsTo,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
) -> Result<()> {
    if import_paths.is_empty() {
        return Ok(());
    }
    let declarations = describe_node_types_in_a_node_interpreter_within(
        import_paths,
        stream_environment,
        lend_directory,
        NODE_INTERPRETER_DESCRIBE_BOUND,
        the_stream_the_describe_belongs_to,
        is_interrupted_by_the_host,
    )?;
    for declaration in declarations {
        register_the_described_node_type(
            node_types_of_the_stream,
            declaration,
            None,
            stream_environment,
            lend_directory,
            the_stream_the_describe_belongs_to,
        )?;
    }
    Ok(())
}

/// Register `declaration` into `node_types_of_the_stream`: as a type `installed_module`
/// declares, or, with none, as a type a graph or a live add named.
fn register_the_described_node_type(
    node_types_of_the_stream: &NodeTypesOneStreamResolves,
    declaration: PythonNodeDeclaration,
    installed_module: Option<InstalledModuleOfANodeType>,
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
    the_stream_its_helpers_belong_to: &LoadedStreamAHelperProcessBelongsTo,
) -> Result<()> {
    let register = |descriptor: NodeDescriptor, how_its_nodes_run: HowANodeOfADescribedTypeRuns| {
        match installed_module {
            Some(installed_module) => node_types_of_the_stream
                .register_a_type_an_installed_module_declares(
                    descriptor,
                    how_its_nodes_run,
                    installed_module,
                ),
            None => node_types_of_the_stream
                .register_a_type_described_in_this_streams_node_interpreter(
                    descriptor,
                    how_its_nodes_run,
                ),
        }
    };
    let PythonNodeDeclaration {
        descriptor,
        how_a_python_node_of_the_type_runs,
    } = declaration;
    let node_class_import_path = descriptor.node_class_import_path.as_str().to_string();
    let descriptor_for_constructor = descriptor.clone();
    let stream_environment = stream_environment.clone();
    let lend_directory = Arc::new(lend_directory.to_path_buf());
    let the_stream_its_helpers_belong_to = the_stream_its_helpers_belong_to.clone();
    let child_execution_config = match how_a_python_node_of_the_type_runs {
        HowAPythonNodeOfTheTypeRuns::InItsOwnNodeInterpreter(execution_config) => {
            node_types_of_the_stream.record_the_execution_a_described_class_declares(
                descriptor.node_class_import_path.clone(),
                execution_config.execution,
            );
            execution_config
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        HowAPythonNodeOfTheTypeRuns::OnItsNativeShaderEvaluator(program) => {
            return register(
                descriptor,
                HowANodeOfADescribedTypeRuns::OnTheNativeShaderEvaluatorOfItsIsland {
                    shader_node_program: program.clone(),
                    evaluator_of_the_island_it_hosts: Box::new(move |node| {
                        Ok(Box::new(
                                    super::shader_node_native_evaluator::ShaderNodeNativeEvaluator::for_the_node(
                                        node,
                                        descriptor_for_constructor.clone(),
                                        program.clone(),
                                    ),
                                )
                                    as Box<dyn DynGeneratedNode + Send>)
                    }),
                },
            );
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        HowAPythonNodeOfTheTypeRuns::OnItsNativeShaderEvaluator(_) => {
            return Err(Error::NotSupported(format!(
                "shader node `{node_class_import_path}` runs on the GPU through Vulkan, \
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
            return register(
                descriptor,
                HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds {
                    node_once_promoted_to_its_own_process: Box::new(move |node| {
                        Ok(
                            Box::new(spawn_host_for_a_function_node_promoted_to_its_own_process(
                                &node_class_import_path,
                                &descriptor_for_constructor,
                                node,
                                &stream_environment,
                                &lend_directory,
                                the_stream_its_helpers_belong_to.clone(),
                            )?) as Box<dyn DynGeneratedNode + Send>,
                        )
                    }),
                },
            );
        }
    };
    register(
        descriptor,
        HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(Box::new(move |node| {
            Ok(Box::new(spawn_host_for_graph_node(
                &node_class_import_path,
                &descriptor_for_constructor,
                child_execution_config,
                node,
                &stream_environment,
                &lend_directory,
                the_stream_its_helpers_belong_to.clone(),
            )) as Box<dyn DynGeneratedNode + Send>)
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
    if node_types_of_the_stream
        .was_described_in_this_streams_node_interpreter(&pool_descriptor.node_class_import_path)
    {
        return Ok(());
    }
    let stream_environment = stream_environment.clone();
    let lend_directory = Arc::clone(lend_directory);
    let the_stream_its_helpers_belong_to = the_stream_its_helpers_belong_to.clone();
    node_types_of_the_stream.register_a_type_described_in_this_streams_node_interpreter(
        pool_descriptor,
        HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(Box::new(move |node| {
            let descriptor =
                the_function_node_pool_descriptor(Some((&node.ports.inputs, &node.ports.outputs)))?;
            Ok(Box::new(spawn_host_for_graph_node(
                FUNCTION_NODE_POOL_NODE_TYPE,
                &descriptor,
                ExecutionConfig::new(ProcessExecution::Continuous {
                    interval_ms: FUNCTION_NODE_POOL_INTERVAL_MS,
                }),
                node,
                &stream_environment,
                &lend_directory,
                the_stream_its_helpers_belong_to.clone(),
            )) as Box<dyn DynGeneratedNode + Send>)
        })),
    )
}

/// Describe `import_paths` in one start of the stream's interpreter, bounded
/// by `describe_bound` and cut short by the shutdown of the stream it belongs
/// to or by `is_interrupted_by_the_host` reporting true, returning their
/// declarations in the order asked.
pub(crate) fn describe_node_types_in_a_node_interpreter_within(
    import_paths: &[NodeClassImportPath],
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
    describe_bound: Duration,
    the_stream_the_describe_belongs_to: &LoadedStreamAHelperProcessBelongsTo,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
) -> Result<Vec<PythonNodeDeclaration>> {
    let refuse_every_requested_type = |refusal: String| Error::NodeTypesNotDescribed {
        node_types: import_paths.to_vec(),
        refusal,
    };
    let interpreter = stream_environment.interpreter.display();
    let (describe_document, quoted_standard_error): (NodeInterpreterDescribeDocument, String) =
        run_the_node_interpreter_out_of_stream_reading_its_document(
            || node_interpreter_describe_command(import_paths, stream_environment, lend_directory),
            stream_environment,
            describe_bound,
            the_stream_the_describe_belongs_to,
            is_interrupted_by_the_host,
            &refuse_every_requested_type,
            &A_DESCRIBE,
        )?;

    let refused_node_types: Vec<(NodeClassImportPath, &str)> = import_paths
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
            PythonNodeDeclaration::read_from_described_node_type(described_node_type, import_path)
                .map_err(refuse_this_type)
        })
        .collect()
}

/// What one out-of-stream run of the stream's node interpreter does, in the words its refusals
/// name it by.
pub(crate) struct NodeInterpreterRunOutOfStream {
    /// What the run is doing while it runs, such as "describing them".
    pub(crate) activity: &'static str,
    /// The run's own name, such as "describe".
    pub(crate) run_name: &'static str,
    /// The name of the one document it prints, such as "describe document".
    pub(crate) document_name: &'static str,
}

/// A describe, in the words its refusals name it by.
const A_DESCRIBE: NodeInterpreterRunOutOfStream = NodeInterpreterRunOutOfStream {
    activity: "describing them",
    run_name: "describe",
    document_name: "describe document",
};

/// Start the run of the stream's node interpreter out of stream that `run_command` builds,
/// bounded by `run_bound` and cut short by the stream's shutdown or the host's interrupt, and
/// read the document it printed, every failure refused through `refuse`, naming the run as
/// `what_runs` does.
pub(crate) fn run_the_node_interpreter_out_of_stream_reading_its_document<
    Document: serde::de::DeserializeOwned,
>(
    run_command: impl FnOnce() -> Result<Command>,
    stream_environment: &StreamEnvironment,
    run_bound: Duration,
    the_stream_the_run_belongs_to: &LoadedStreamAHelperProcessBelongsTo,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
    refuse: &dyn Fn(String) -> Error,
    what_runs: &NodeInterpreterRunOutOfStream,
) -> Result<(Document, String)> {
    let NodeInterpreterRunOutOfStream {
        activity,
        run_name,
        document_name,
    } = what_runs;
    let interpreter = stream_environment.interpreter.display();
    if is_interrupted_by_the_host() {
        return Err(refuse(interrupted_by_the_host_refusal(run_name)));
    }

    let mut command = run_command().map_err(|refusal| refuse(refusal.to_string()))?;
    let mut child =
        spawn_outside_every_iceoryx2_listener_bind(&mut command).map_err(|spawn_failure| {
            refuse(format!(
                "the stream's interpreter `{interpreter}` could not be started in `{}`: \
                 {spawn_failure}",
                stream_environment.project_directory.display()
            ))
        })?;
    // `pre_exec` made the describe the leader of a group whose id is its pid.
    if !crate::core::runtime::register_a_helper_process_group(
        child.id() as i32,
        the_stream_the_run_belongs_to.stream_tag,
    ) {
        tracing::warn!(
            "the {run_name}'s process group could not be registered, so a third interrupt will \
             not kill it; the {run_name} still ends itself when the app exits"
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

    let run_exit = wait_for_a_stream_interpreter_to_exit(
        &child,
        run_bound,
        &|| {
            the_stream_the_run_belongs_to
                .shutdown_escalation
                .is_requested()
        },
        is_interrupted_by_the_host,
    );
    // Killed whether or not the leader already exited: nothing a run out of
    // stream starts outlives it, and a descendant left holding its standard
    // streams would hold the readers open.
    let exit_status =
        kill_the_process_group_and_reap_its_leader(&mut child, format!("the {run_name}"));
    let quoted_standard_error = standard_error_tail_as_a_refusal_quotes_it(
        &standard_error_tail
            .map(|tail| {
                tail.text_once_closed_or_after(HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE)
            })
            .unwrap_or_default(),
    );

    match run_exit {
        StreamInterpreterExitAwaited::Exited => {}
        StreamInterpreterExitAwaited::BoundElapsed => {
            return Err(refuse(format!(
                "the stream's interpreter `{interpreter}` did not finish {activity} within {}s and \
                 was killed. Work a module does at import time runs here — a module that blocks \
                 at import blocks its {run_name}. {quoted_standard_error}",
                run_bound.as_secs_f64()
            )));
        }
        StreamInterpreterExitAwaited::ItsStreamsShutdownRequested => {
            return Err(refuse(format!(
                "the stream's shutdown began while its interpreter `{interpreter}` was still \
                 {activity}, and it was killed. {quoted_standard_error}"
            )));
        }
        StreamInterpreterExitAwaited::InterruptedByTheHost => {
            return Err(refuse(format!(
                "{}: the stream's interpreter `{interpreter}` was killed. \
                 {quoted_standard_error}",
                interrupted_by_the_host_refusal(run_name)
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
        return Err(refuse(format!(
            "the stream's interpreter `{interpreter}` printed no {document_name} and exited with \
             {exit_status_rendered}. {quoted_standard_error}"
        )));
    }
    let document = match serde_json::from_slice::<Document>(&standard_output_bytes) {
        Ok(document) => document,
        Err(not_the_document) => {
            return Err(refuse(format!(
                "the stream's interpreter `{interpreter}` exited with {exit_status_rendered}, and \
                 its standard output is not a {document_name} ({not_the_document}); it began \
                 {}. What a `sitecustomize`, `usercustomize` or `.pth` hook in the stream's \
                 environment prints lands ahead of the document. {quoted_standard_error}",
                standard_output_head_as_a_refusal_quotes_it(&standard_output_bytes)
            )));
        }
    };
    Ok((document, quoted_standard_error))
}

pub(crate) fn describe_the_node_types_of_the_streams_launch_within(
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
    describe_bound: Duration,
    the_stream_the_describe_belongs_to: &LoadedStreamAHelperProcessBelongsTo,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
) -> Result<NodeTypesDescribedAtTheStreamsLaunch> {
    let refuse_the_installed_modules = |refusal: String| Error::Runtime(refusal);
    let (describe_document, _): (NodeInterpreterDescribeDocument, String) =
        run_the_node_interpreter_out_of_stream_reading_its_document(
            || {
                node_interpreter_describe_installed_modules_command(
                    stream_environment,
                    lend_directory,
                )
            },
            stream_environment,
            describe_bound,
            the_stream_the_describe_belongs_to,
            is_interrupted_by_the_host,
            &refuse_the_installed_modules,
            &A_DESCRIBE,
        )?;
    let mut described_at_launch = NodeTypesDescribedAtTheStreamsLaunch {
        described: Vec::new(),
        refused: Vec::new(),
        described_of_the_projects_own_code: Vec::new(),
        refused_of_the_projects_own_code: Vec::new(),
    };
    let NodeInterpreterDescribeDocument {
        described_node_types,
        refused_node_types,
        installed_modules,
        project_node_types,
    } = describe_document;
    let the_declaration_described_of = |import_path: &str| {
        the_declaration_a_describe_document_reads_of(
            &described_node_types,
            &refused_node_types,
            import_path,
        )
    };
    for declared in installed_modules {
        let installed_module = InstalledModuleOfANodeType {
            distribution: declared.distribution,
            distribution_version: declared.distribution_version,
            entry_point: declared.entry_point,
        };
        match the_declaration_described_of(&declared.import_path) {
            Ok(declaration) => described_at_launch
                .described
                .push((declaration, installed_module)),
            Err(refusal) => {
                described_at_launch
                    .refused
                    .push((declared.import_path, installed_module, refusal))
            }
        }
    }
    for import_path in project_node_types {
        match the_declaration_described_of(&import_path) {
            Ok(declaration) => described_at_launch
                .described_of_the_projects_own_code
                .push(declaration),
            Err(refusal) => described_at_launch
                .refused_of_the_projects_own_code
                .push((import_path, refusal)),
        }
    }
    Ok(described_at_launch)
}

/// The declaration a describe document's `described_node_types` hold of the class at
/// `import_path`, or why they hold none: the refusal its `refused_node_types` gave, or why
/// its description does not read.
fn the_declaration_a_describe_document_reads_of(
    described_node_types: &[serde_json::Value],
    refused_node_types: &[NodeTypeTheDescribeRefused],
    import_path: &str,
) -> std::result::Result<PythonNodeDeclaration, String> {
    if let Some(refused) = refused_node_types
        .iter()
        .find(|refused| refused.import_path == import_path)
    {
        return Err(refused.refusal.trim_end().to_string());
    }
    let import_path = NodeClassImportPath::new(import_path)
        .map_err(|not_an_import_path| not_an_import_path.to_string())?;
    let described_node_type = described_node_types
        .iter()
        .find(|described| {
            described.get("import_path").and_then(|path| path.as_str())
                == Some(import_path.as_str())
        })
        .ok_or_else(|| "the describe neither described nor refused it".to_string())?;
    PythonNodeDeclaration::read_from_described_node_type(described_node_type, &import_path)
}

/// Register every class installed distributions declare into
/// `node_types_of_the_stream`, each recorded under the distribution declaring
/// it, then every class of the project's own code the stream has not described.
/// A class a natively compiled type already holds is skipped, and an installed
/// one the stream already described keeps its registration and gains the record.
pub(crate) fn register_the_node_types_described_at_the_streams_launch(
    node_types_of_the_stream: &NodeTypesOneStreamResolves,
    described_at_launch: NodeTypesDescribedAtTheStreamsLaunch,
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
    the_stream_its_helpers_belong_to: &LoadedStreamAHelperProcessBelongsTo,
) -> Vec<NodeClassImportPath> {
    for (import_path, installed_module, refusal) in described_at_launch.refused {
        tracing::warn!(
            node_class_import_path = import_path.as_str(),
            distribution = installed_module.distribution.as_str(),
            "an installed module's node type was not described, so the stream cannot add it: \
             {refusal}"
        );
    }
    let mut registered = Vec::new();
    for (declaration, installed_module) in described_at_launch.described {
        let node_type = declaration.descriptor.node_class_import_path.clone();
        if node_types_of_the_stream.is_a_natively_compiled_node_type(&node_type) {
            tracing::warn!(
                node_class_import_path = node_type.as_str(),
                distribution = installed_module.distribution.as_str(),
                "an installed module declares a node type a natively compiled one already holds, \
                 so it is left out"
            );
            continue;
        }
        if node_types_of_the_stream.was_described_in_this_streams_node_interpreter(&node_type) {
            node_types_of_the_stream
                .record_the_installed_module_declaring(node_type, installed_module);
            continue;
        }
        registered.extend(register_one_described_node_type_at_launch(
            node_types_of_the_stream,
            declaration,
            Some(installed_module),
            stream_environment,
            lend_directory,
            the_stream_its_helpers_belong_to,
        ));
    }
    for (import_path, refusal) in described_at_launch.refused_of_the_projects_own_code {
        tracing::warn!(
            node_class_import_path = import_path.as_str(),
            "a node type of the project's own code was not described, so the stream lists it \
             only once a graph names it: {refusal}"
        );
    }
    for declaration in described_at_launch.described_of_the_projects_own_code {
        let node_type = &declaration.descriptor.node_class_import_path;
        if node_types_of_the_stream.is_a_natively_compiled_node_type(node_type) {
            tracing::warn!(
                node_class_import_path = node_type.as_str(),
                "the project's own code declares a node type a natively compiled one already \
                 holds, so it is left out"
            );
            continue;
        }
        if node_types_of_the_stream.was_described_in_this_streams_node_interpreter(node_type) {
            continue;
        }
        registered.extend(register_one_described_node_type_at_launch(
            node_types_of_the_stream,
            declaration,
            None,
            stream_environment,
            lend_directory,
            the_stream_its_helpers_belong_to,
        ));
    }
    registered
}

/// Register `declaration`, described when the stream launched, into `node_types_of_the_stream`
/// under the installed module declaring it, if one does; its node type, or `None` with a
/// warning when the registration is refused.
fn register_one_described_node_type_at_launch(
    node_types_of_the_stream: &NodeTypesOneStreamResolves,
    declaration: PythonNodeDeclaration,
    installed_module: Option<InstalledModuleOfANodeType>,
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
    the_stream_its_helpers_belong_to: &LoadedStreamAHelperProcessBelongsTo,
) -> Option<NodeClassImportPath> {
    let node_type = declaration.descriptor.node_class_import_path.clone();
    let distribution = installed_module
        .as_ref()
        .map(|installed_module| installed_module.distribution.clone());
    match register_the_described_node_type(
        node_types_of_the_stream,
        declaration,
        installed_module,
        stream_environment,
        lend_directory,
        the_stream_its_helpers_belong_to,
    ) {
        Ok(()) => Some(node_type),
        Err(refusal) => {
            tracing::warn!(
                node_class_import_path = node_type.as_str(),
                distribution = distribution.as_deref(),
                "a node type described when the stream launched was refused: {refusal}"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests;
