// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab`: `new` writes a stream project; `run`, `dev`, `stop`, `start`, `rm`, `rename`,
//! `streams` and `expose` load and manage the streams the machine's runtime holds; `relays` and `relay` list,
//! add, configure and remove the relays it joins; and `graph`, `logs` and `mcp` observe them — each through the runtime's local API socket, at its fixed path; `logs` also reads a stream's JSONL log files; `enable-virtual-camera` grants this
//! machine's users the virtual camera's loopback device, once; `--version` adds the runtime's
//! description to the CLI's own version. No verb starts a runtime.

// stdout and stderr are this binary's output channel to the user, as they are xtask's.
#![allow(clippy::disallowed_macros)]

mod cli_and_runtime_version_flag;
mod local_api_connection;
mod local_api_mcp_stdio_pipe;
mod local_api_mcp_tool_client;
mod local_api_unix_socket_http_client;
mod machine_runtime_local_api_socket;
mod process_signal_handling;
mod project_source_change_watcher;
mod relay_verbs_on_the_runtime;
mod runtime_gone_while_following_a_stream;
mod runtime_log_files_reader;
mod runtime_logs_verb;
mod runtime_observation_verbs;
mod scaffold_new_stream_project;
mod stream_actions_on_the_runtime;
mod stream_argument_resolution_through_the_listing;
mod stream_followed_and_reloaded_on_every_save;
mod stream_log_records_from_the_runtime;
mod verb_output_table;
mod verb_standard_output;
mod virtual_camera_loopback_permission_grant;

#[cfg(test)]
#[path = "../tests/common/stub_local_api_server.rs"]
mod stub_local_api_server;

#[cfg(test)]
#[path = "../tests/common/runtime_log_line_fixtures.rs"]
mod runtime_log_line_fixtures;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use streamlib_runtime_client_contract::local_api_wire_contract::ExposePortLevel;

use crate::stream_actions_on_the_runtime::{
    STREAM_ARGUMENT_HELP, STREAM_ARGUMENT_LONG_HELP, StreamLoadArguments,
};
use crate::virtual_camera_loopback_permission_grant::VirtualCameraGrantTargetMachine;

/// A command that ends `tatolab` with a message on stderr and an exit code.
#[derive(Debug)]
pub(crate) struct TatolabCommandFailure {
    /// What went wrong, printed after `error: `; `None` when the failing step already said why.
    pub(crate) message_for_the_user: Option<String>,
    /// The code `tatolab` exits with.
    pub(crate) exit_code: u8,
}

impl TatolabCommandFailure {
    /// A refusal: `error: <message>`, exit 1.
    pub(crate) fn refused(message_for_the_user: String) -> Self {
        Self {
            message_for_the_user: Some(message_for_the_user),
            exit_code: 1,
        }
    }
}

#[cfg(test)]
impl TatolabCommandFailure {
    /// The message of the refusal `command_outcome` failed with, asserting it exits 1.
    pub(crate) fn refusal_message_of<CommandSuccess: std::fmt::Debug>(
        command_outcome: Result<CommandSuccess, TatolabCommandFailure>,
    ) -> String {
        let command_failure = command_outcome.expect_err("the command must refuse");
        assert_eq!(command_failure.exit_code, 1, "a refusal exits 1");
        command_failure
            .message_for_the_user
            .expect("a refusal names its reason")
    }
}

#[derive(Parser)]
#[command(
    name = "tatolab",
    disable_version_flag = true,
    arg_required_else_help = true,
    about = "Tatolab — write a stream project, run it on this machine's runtime, and manage and \
             observe the streams the runtime holds.",
    disable_help_subcommand = true
)]
struct TatolabCommandLine {
    /// Print this CLI's version and, when a runtime answers, the runtime's description.
    #[arg(short = 'V', long = "version")]
    print_the_cli_and_runtime_versions: bool,
    #[command(subcommand)]
    verb: Option<TatolabVerb>,
}

#[derive(Subcommand)]
enum TatolabVerb {
    /// Scaffold a new stream project.
    #[command(
        long_about = "Write stream.py, nodes/, pyproject.toml, .python-version and .gitignore into \
                      DIRECTORY — one @stream wiring the sample animation through a GPU effect \
                      into an H.264 encoder it exposes publicly, with a meter on a fan-out."
    )]
    New {
        /// Directory to scaffold the project into.
        directory: PathBuf,
    },
    /// Load this project's stream into the runtime, print its id, and return.
    #[command(
        long_about = "The runtime compiles the stream in the project's own .venv, loads it, and \
                      records it: it runs across the runtime's restarts until `tatolab stop` or \
                      `tatolab rm`. Running the same project and function again replaces it under \
                      the same id. A stream whose code fails is not refused: it is recorded \
                      failed and printed as `<id> failed: <reason>` with its traceback, until a \
                      run of the fixed code replaces it or `tatolab start` retries it. Prints \
                      the stream's id and how to follow it with `tatolab logs -f`; exits 0 when \
                      every stream it loaded runs, and 1 when one is failed or the load is \
                      refused, naming why."
    )]
    Run {
        #[command(flatten)]
        stream_load_arguments: StreamLoadArguments,
    },
    /// Load this project's stream, follow its records, and load it again on every saved edit.
    #[command(
        long_about = "`run`, then the stream's records followed, plus a watch on the project's \
                      .py files: each settled save runs the stream again, which replaces it. A \
                      save whose code fails records the stream failed, and dev prints its \
                      traceback and waits for the next save. Ctrl-C ends dev and leaves the \
                      stream loaded. When the runtime \
                      goes away, dev says so, waits for it to re-load the stream from its record, \
                      and follows it again."
    )]
    Dev {
        #[command(flatten)]
        stream_load_arguments: StreamLoadArguments,
    },
    /// Stop a stream: unload it, and keep it stopped across restarts.
    Stop {
        #[arg(help = STREAM_ARGUMENT_HELP, long_help = STREAM_ARGUMENT_LONG_HELP)]
        stream: Option<String>,
    },
    /// Load a stopped stream again, or retry a failed one.
    Start {
        #[arg(help = STREAM_ARGUMENT_HELP, long_help = STREAM_ARGUMENT_LONG_HELP)]
        stream: Option<String>,
    },
    /// Remove a stream: unload it and forget it.
    Rm {
        #[arg(help = STREAM_ARGUMENT_HELP, long_help = STREAM_ARGUMENT_LONG_HELP)]
        stream: Option<String>,
    },
    /// Show a stream under another display name; its id and address stay as they are.
    #[command(
        override_usage = "tatolab rename [STREAM] <DISPLAY_NAME>",
        after_long_help = STREAM_ARGUMENT_LONG_HELP,
        long_about = "Show a stream under another display name — any text, never cast and never \
                      unique. Its id, its address and its broadcast stay as they are, so no reader \
                      of its ports is cut off, and the name holds across restarts and re-loads \
                      without --name."
    )]
    Rename {
        /// [STREAM], as every stream verb takes it, then the display name to show it under.
        #[arg(value_names = ["STREAM", "DISPLAY_NAME"], num_args = 1..=2, required = true)]
        stream_then_display_name: Vec<String>,
    },
    /// List the streams the runtime holds: running, stopped or failed, and why each failed.
    Streams,
    /// Set how far one output port of a stream is readable.
    #[command(
        long_about = "Private (the default here) lets this machine's other streams and agents read \
                      the port; --public lets readers off the machine read it too; --remove makes \
                      it internal to its stream. The change is live — a reader the new level no \
                      longer allows is cut at once — and the stream's record holds it, over what \
                      its function exposes, across restarts."
    )]
    #[command(
        override_usage = "tatolab expose [STREAM] <NODE> <PORT> [--public | --remove]",
        after_long_help = STREAM_ARGUMENT_LONG_HELP
    )]
    Expose {
        /// [STREAM], as every stream verb takes it; the node by its name in the stream's graph;
        /// and the output port by its name on the node.
        #[arg(value_names = ["STREAM", "NODE", "PORT"], num_args = 2..=3, required = true)]
        stream_then_node_then_port: Vec<String>,
        /// Make the port public instead of private.
        #[arg(long = "public", conflicts_with = "remove")]
        public: bool,
        /// Make the port internal to its stream instead of private.
        #[arg(long = "remove")]
        remove: bool,
    },
    /// List the relays the runtime joins: each one's adapter, state, settings and secrets set.
    #[command(
        long_about = "Every relay instance: the local relay's two built-in listeners and each \
                      added one, with its adapter, its connection state, its settings, and each \
                      secret setting as set or unset — never its value — then why each relay not \
                      joined is not."
    )]
    Relays,
    /// Add, configure or remove one of the relays the runtime joins.
    Relay {
        #[command(subcommand)]
        relay_verb: relay_verbs_on_the_runtime::RelayVerb,
    },
    /// Export the runtime's live graph as JSON: every stream's, or one's.
    #[command(
        long_about = "Nodes, ports, links, channel names, states and metrics, as the runtime \
                      reports them right now: every loaded stream under the machine's name, or with \
                      --stream that stream's graph alone, in the shape a load takes."
    )]
    Graph {
        /// Only this loaded stream's graph.
        #[arg(long = "stream", value_name = "STREAM")]
        requested_stream: Option<String>,
    },
    /// Read a loaded stream's records from the runtime, or its JSONL log file.
    #[command(
        long_about = "Reads a loaded stream's records as the runtime holds them, by sequence \
                      number — the stream --stream names, by its id or a display name only it \
                      holds, or the one loaded from the working directory; -f keeps following. \
                      With RUNTIME_ID-STREAM, renders that stream's on-disk JSONL log exactly as \
                      the runtime mirrored it; --list names each one."
    )]
    Logs(runtime_logs_verb::RuntimeLogsVerbArguments),
    /// Connect an MCP host to the runtime over this command's stdin and stdout.
    #[command(
        long_about = "For an MCP host to launch: `claude mcp add tatolab -- tatolab mcp`, or `ssh \
                      <machine> tatolab mcp` for the runtime on another machine. Copies bytes \
                      between stdio and the runtime's MCP server, through its local API socket, \
                      without reading them. A stream the host runs is recorded by the runtime and \
                      outlives this connection."
    )]
    Mcp,
    /// Grant this machine's users the permission a VirtualCameraSink needs, once.
    #[command(
        long_about = "Install the standard grant behind the virtual camera's loopback door: load \
                      v4l2loopback with no devices (persisted in modules-load.d and modprobe.d) \
                      and tag its control node `uaccess` for the logged-in user. One privileged \
                      step through pkexec (sudo in a headless shell); the engine never runs it."
    )]
    EnableVirtualCamera {
        /// Write the three files' contents and the commands to stdout and change nothing.
        #[arg(long = "print")]
        print_grant_without_installing: bool,
    },
}

/// The working directory a project defaults to.
pub(crate) fn caller_working_directory() -> Result<PathBuf, TatolabCommandFailure> {
    std::env::current_dir().map_err(|io_failure| {
        TatolabCommandFailure::refused(format!("cannot read the working directory: {io_failure}"))
    })
}

fn main() -> ExitCode {
    let command_line = TatolabCommandLine::parse();
    let command_outcome = match (
        command_line.print_the_cli_and_runtime_versions,
        command_line.verb,
    ) {
        (true, _) => cli_and_runtime_version_flag::print_the_cli_and_runtime_versions(),
        (false, Some(verb)) => run_one_verb(verb),
        (false, None) => TatolabCommandLine::command()
            .error(
                clap::error::ErrorKind::MissingSubcommand,
                "name a verb, or pass --version",
            )
            .exit(),
    };
    match command_outcome {
        Ok(exit_code) => ExitCode::from(exit_code),
        Err(command_failure) => {
            if let Some(message_for_the_user) = command_failure.message_for_the_user {
                eprintln!("error: {message_for_the_user}");
            }
            ExitCode::from(command_failure.exit_code)
        }
    }
}

/// Run `verb` to its outcome.
fn run_one_verb(verb: TatolabVerb) -> Result<u8, TatolabCommandFailure> {
    match verb {
        TatolabVerb::New { directory } => {
            scaffold_new_stream_project::scaffold_new_stream_project(&directory).map(|()| 0)
        }
        TatolabVerb::Run {
            stream_load_arguments,
        } => caller_working_directory().and_then(|caller_working_directory| {
            stream_actions_on_the_runtime::run_stream(
                &stream_load_arguments,
                &caller_working_directory,
            )
        }),
        TatolabVerb::Dev {
            stream_load_arguments,
        } => caller_working_directory().and_then(|caller_working_directory| {
            stream_followed_and_reloaded_on_every_save::follow_the_stream_and_reload_it_on_every_save(
                &stream_load_arguments,
                &caller_working_directory,
            )
        }),
        TatolabVerb::Stop { stream } => {
            stream_actions_on_the_runtime::stop_stream(stream.as_deref())
        }
        TatolabVerb::Start { stream } => {
            stream_actions_on_the_runtime::start_stream(stream.as_deref())
        }
        TatolabVerb::Rm { stream } => {
            stream_actions_on_the_runtime::remove_stream(stream.as_deref())
        }
        TatolabVerb::Rename {
            stream_then_display_name,
        } => stream_actions_on_the_runtime::rename_stream(&stream_then_display_name),
        TatolabVerb::Streams => stream_actions_on_the_runtime::list_streams(),
        TatolabVerb::Expose {
            stream_then_node_then_port,
            public,
            remove,
        } => stream_actions_on_the_runtime::expose_port(
            &stream_then_node_then_port,
            match (public, remove) {
                (true, _) => ExposePortLevel::Public,
                (false, true) => ExposePortLevel::Internal,
                (false, false) => ExposePortLevel::Private,
            },
        ),
        TatolabVerb::Relays => relay_verbs_on_the_runtime::list_relays(),
        TatolabVerb::Relay { relay_verb } => relay_verbs_on_the_runtime::run_relay_verb(relay_verb),
        TatolabVerb::Graph { requested_stream } => {
            runtime_observation_verbs::print_local_api_tool_result_of_the_running_runtime(
                runtime_observation_verbs::GRAPH_TOOL_NAME,
                runtime_observation_verbs::graph_tool_arguments(requested_stream.as_deref()),
            )
        }
        TatolabVerb::Logs(logs_arguments) => runtime_logs_verb::run_runtime_logs_verb(logs_arguments),
        TatolabVerb::Mcp => local_api_mcp_stdio_pipe::pipe_stdio_to_the_running_runtimes_mcp_server(),
        TatolabVerb::EnableVirtualCamera {
            print_grant_without_installing: true,
        } => virtual_camera_loopback_permission_grant::print_virtual_camera_grant_for_hand_install(),
        TatolabVerb::EnableVirtualCamera {
            print_grant_without_installing: false,
        } => VirtualCameraGrantTargetMachine::this_machine()
            .map_err(|kernel_identification_failure| {
                TatolabCommandFailure::refused(format!(
                    "cannot read this machine's kernel name and release: \
                     {kernel_identification_failure}"
                ))
            })
            .and_then(|mut grant_target_machine| {
                virtual_camera_loopback_permission_grant::install_virtual_camera_grant_through_privilege_escalation_helper(
                    &mut grant_target_machine,
                )
            }),
    }
}
