// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab`: `new` writes a stream project; `run`, `dev`, `stop`, `start`, `rm`, `streams` and
//! `expose` load and manage the streams the machine's runtime holds, and `graph`, `tap`, `logs`,
//! `exchange` and `mcp` observe them — each through the runtime's local API socket, at its fixed
//! path; `logs` also reads a stream's JSONL log files; `enable-virtual-camera` grants this
//! machine's users the virtual camera's loopback device, once. No verb starts a runtime.

// stdout and stderr are this binary's output channel to the user, as they are xtask's.
#![allow(clippy::disallowed_macros)]

mod attached_stream_on_the_runtime;
mod local_api_connection;
mod local_api_mcp_stdio_pipe;
mod local_api_mcp_tool_client;
mod local_api_unix_socket_http_client;
mod machine_runtime_local_api_socket;
mod process_signal_handling;
mod project_source_change_watcher;
mod runtime_log_files_reader;
mod runtime_logs_verb;
mod runtime_observation_verbs;
mod scaffold_new_stream_project;
mod stream_actions_on_the_runtime;
mod stream_log_records_from_the_runtime;
mod surface_image_exchange;
mod verb_standard_output;
mod virtual_camera_loopback_permission_grant;

#[cfg(test)]
#[path = "../tests/common/stub_local_api_server.rs"]
mod stub_local_api_server;

#[cfg(test)]
#[path = "../tests/common/tapped_channel_bag_fixtures.rs"]
mod tapped_channel_bag_fixtures;

#[cfg(test)]
#[path = "../tests/common/runtime_log_line_fixtures.rs"]
mod runtime_log_line_fixtures;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use streamlib_runtime_client_contract::local_api_wire_contract::ExposePortLevel;

use crate::attached_stream_on_the_runtime::AttachedStreamVerb;
use crate::stream_actions_on_the_runtime::StreamLoadArguments;
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
    version,
    about = "Tatolab — write a stream project, run it on this machine's runtime, and manage and \
             observe the streams the runtime holds.",
    disable_help_subcommand = true
)]
struct TatolabCommandLine {
    #[command(subcommand)]
    verb: TatolabVerb,
}

#[derive(Subcommand)]
enum TatolabVerb {
    /// Scaffold a new stream project.
    #[command(
        long_about = "Write stream.py, nodes/, pyproject.toml, .python-version and .gitignore into \
                      DIRECTORY — one @stream wiring a working camera → effect → window pipeline, \
                      with a meter on a fan-out."
    )]
    New {
        /// Directory to scaffold the project into.
        directory: PathBuf,
        /// Wire the built-in test pattern instead of the camera, so the stream runs on a machine
        /// with no capture device.
        #[arg(long)]
        test_pattern: bool,
    },
    /// Load this project's stream into the runtime and follow its records.
    #[command(
        long_about = "The runtime compiles the stream in the project's own .venv and loads it. \
                      Attached, the stream lives as long as this command: Ctrl-C, a closed \
                      terminal or a killed tatolab unloads it. With -d, the runtime keeps it, \
                      across its own restarts, until `tatolab stop` or `tatolab rm`."
    )]
    Run {
        #[command(flatten)]
        stream_load_arguments: StreamLoadArguments,
        /// Hand the stream to the runtime to keep, print one line, and return.
        #[arg(short = 'd', long = "detach")]
        detach: bool,
    },
    /// Run this project's stream attached and load it again on every saved edit.
    #[command(
        long_about = "`run` attached, plus a watch on the project's .py files: each settled save \
                      stops the stream and loads it again. A load the runtime refuses waits for \
                      the next save; when the runtime goes away, dev waits for it and loads again."
    )]
    Dev {
        #[command(flatten)]
        stream_load_arguments: StreamLoadArguments,
    },
    /// Stop a stream: unload it, and keep a kept one stopped across restarts.
    Stop {
        /// The stream to stop, as `tatolab streams` names it.
        stream: String,
    },
    /// Load a stopped kept stream again, or retry a failed one.
    Start {
        /// The stream to start, as `tatolab streams` names it.
        stream: String,
    },
    /// Remove a stream: unload it and forget it.
    Rm {
        /// The stream to remove, as `tatolab streams` names it.
        stream: String,
    },
    /// List the streams the runtime holds: attached, kept, stopped or failed, and why each failed.
    Streams,
    /// Set how far one output port of a stream is readable.
    #[command(
        long_about = "Private (the default here) lets this machine's other streams and agents read \
                      the port; --public lets readers off the machine read it too; --remove makes \
                      it internal to its stream. The change is live — a reader the new level no \
                      longer allows is cut at once — and a kept stream records it, over what its \
                      function exposes, across restarts."
    )]
    Expose {
        /// The stream the port belongs to.
        stream: String,
        /// The node, by its name in the stream's graph.
        node: String,
        /// The output port, by its name on the node.
        port: String,
        /// Make the port public instead of private.
        #[arg(long = "public", conflicts_with = "remove")]
        public: bool,
        /// Make the port internal to its stream instead of private.
        #[arg(long = "remove")]
        remove: bool,
    },
    /// Export the runtime's live graph as JSON: every stream's, or one's.
    #[command(
        long_about = "Nodes, ports, links, channel names, states and metrics, as the runtime \
                      reports them right now: every loaded stream under the runtime's name, or with \
                      --stream that stream's graph alone, in the shape a load takes."
    )]
    Graph {
        /// Only this loaded stream's graph.
        #[arg(long = "stream", value_name = "STREAM")]
        requested_stream: Option<String>,
    },
    /// Collect a bounded sample of raw bags from one channel.
    #[command(
        long_about = "Attaches a read-only tap to CHANNEL and collects a bounded sample. The tap \
                      forwards bags verbatim and never blocks the producer, so a quiet channel \
                      returns a partial sample rather than hanging."
    )]
    Tap {
        /// The channel tapped, addressed as its output port.
        #[arg(
            help = "The output port's address, <runtime_name>/<node>/<port>, as graph names them: \
                    its top-level runtime_name and a node's name"
        )]
        channel: String,
        /// The loaded stream the channel belongs to.
        #[arg(long = "stream", value_name = "STREAM", required = true)]
        stream: String,
        /// Bags to collect before returning (default: a small sample).
        #[arg(long = "count", value_name = "N", allow_negative_numbers = true)]
        requested_bag_count: Option<i64>,
        /// Per-bag ceiling on the bytes returned. A bag over the cap comes back flagged and cannot
        /// be decoded, so raise this rather than accept one (default: high enough to carry any
        /// audio block whole).
        #[arg(
            long = "max-bag-bytes",
            value_name = "BYTES",
            allow_negative_numbers = true
        )]
        requested_max_bag_bytes: Option<i64>,
    },
    /// Read a loaded stream's records from the runtime, or its JSONL log file.
    #[command(
        long_about = "With --stream, reads that loaded stream's records as the runtime holds them, \
                      by sequence number; -f keeps following. With RUNTIME_ID-STREAM, renders that \
                      stream's on-disk JSONL log exactly as the runtime mirrored it."
    )]
    Logs(runtime_logs_verb::RuntimeLogsVerbArguments),
    /// Exchange published surface ids for PNG files on disk.
    #[command(
        long_about = "With SURFACE_ID, exchanges that one id. With --channel and --stream, taps \
                      the channel, reads a surface id out of each sampled bag, and exchanges it — \
                      one warm process, no window in the graph and no display server in the path. \
                      Writes exact full-resolution PNGs into --out and prints their paths on \
                      stdout, one per line — those paths are this run's frames, and --out is not \
                      cleared, so read them rather than listing the directory."
    )]
    Exchange(surface_image_exchange::SurfaceImageExchangeArguments),
    /// Connect an MCP host to the runtime over this command's stdin and stdout.
    #[command(
        long_about = "For an MCP host to launch: `claude mcp add tatolab -- tatolab mcp`, or `ssh \
                      <machine> tatolab mcp` for the runtime on another machine. Copies bytes \
                      between stdio and the runtime's MCP server, through its local API socket, \
                      without reading them. A stream the host loads attached lives as long as \
                      this connection."
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
fn caller_working_directory() -> Result<PathBuf, TatolabCommandFailure> {
    std::env::current_dir().map_err(|io_failure| {
        TatolabCommandFailure::refused(format!("cannot read the working directory: {io_failure}"))
    })
}

fn main() -> ExitCode {
    let command_line = TatolabCommandLine::parse();
    let command_outcome = match command_line.verb {
        TatolabVerb::New {
            directory,
            test_pattern,
        } => {
            let scaffolded_stream_source = if test_pattern {
                scaffold_new_stream_project::ScaffoldedStreamSource::TestPattern
            } else {
                scaffold_new_stream_project::ScaffoldedStreamSource::Camera
            };
            scaffold_new_stream_project::scaffold_new_stream_project(
                &directory,
                scaffolded_stream_source,
            )
            .map(|()| 0)
        }
        TatolabVerb::Run {
            stream_load_arguments,
            detach: true,
        } => caller_working_directory().and_then(|caller_working_directory| {
            stream_actions_on_the_runtime::run_stream_kept(
                &stream_load_arguments,
                &caller_working_directory,
            )
        }),
        TatolabVerb::Run {
            stream_load_arguments,
            detach: false,
        } => caller_working_directory().and_then(|caller_working_directory| {
            attached_stream_on_the_runtime::run_stream_attached(
                AttachedStreamVerb::Run,
                &stream_load_arguments,
                &caller_working_directory,
            )
        }),
        TatolabVerb::Dev {
            stream_load_arguments,
        } => caller_working_directory().and_then(|caller_working_directory| {
            attached_stream_on_the_runtime::run_stream_attached(
                AttachedStreamVerb::Dev,
                &stream_load_arguments,
                &caller_working_directory,
            )
        }),
        TatolabVerb::Stop { stream } => stream_actions_on_the_runtime::stop_stream(&stream),
        TatolabVerb::Start { stream } => stream_actions_on_the_runtime::start_stream(&stream),
        TatolabVerb::Rm { stream } => stream_actions_on_the_runtime::remove_stream(&stream),
        TatolabVerb::Streams => stream_actions_on_the_runtime::list_streams(),
        TatolabVerb::Expose {
            stream,
            node,
            port,
            public,
            remove,
        } => stream_actions_on_the_runtime::expose_port(
            &stream,
            &node,
            &port,
            match (public, remove) {
                (true, _) => ExposePortLevel::Public,
                (false, true) => ExposePortLevel::Internal,
                (false, false) => ExposePortLevel::Private,
            },
        ),
        TatolabVerb::Graph { requested_stream } => {
            runtime_observation_verbs::print_local_api_tool_result_of_the_running_runtime(
                runtime_observation_verbs::GRAPH_TOOL_NAME,
                runtime_observation_verbs::graph_tool_arguments(requested_stream.as_deref()),
            )
        }
        TatolabVerb::Tap {
            channel,
            stream,
            requested_bag_count,
            requested_max_bag_bytes,
        } => runtime_observation_verbs::print_local_api_tool_result_of_the_running_runtime(
            runtime_observation_verbs::TAP_TOOL_NAME,
            runtime_observation_verbs::tap_tool_arguments(
                &stream,
                &channel,
                requested_bag_count,
                requested_max_bag_bytes,
            ),
        ),
        TatolabVerb::Logs(logs_arguments) => runtime_logs_verb::run_runtime_logs_verb(logs_arguments),
        TatolabVerb::Exchange(surface_image_exchange_arguments) => {
            surface_image_exchange::run_surface_image_exchange_verb(
                &surface_image_exchange_arguments,
            )
        }
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
