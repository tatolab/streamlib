// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab`: `new` writes a stream project; `run` and `dev` compile a stream in its project's
//! venv and start `tatolabd` attached; `nodes` lists the runtimes running on this machine, and
//! `graph` and `tap` read one through its local API socket; `enable-virtual-camera` grants this
//! machine's users the virtual camera's loopback device, once.

// stdout and stderr are this binary's output channel to the user, as they are xtask's.
#![allow(clippy::disallowed_macros)]

mod attached_tatolabd_supervisor;
mod forwarded_signal_listener;
mod local_api_mcp_tool_client;
mod local_api_runtime_selection;
mod local_api_unix_socket_http_client;
mod project_source_change_watcher;
mod runtime_observation_verbs;
mod scaffold_new_stream_project;
mod surface_image_exchange;
mod virtual_camera_loopback_permission_grant;

#[cfg(test)]
#[path = "../tests/common/stub_local_api_server.rs"]
mod stub_local_api_server;

#[cfg(test)]
#[path = "../tests/common/isolated_node_registry.rs"]
mod isolated_node_registry;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

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

    /// An exit whose reason a child process already reported.
    pub(crate) fn already_reported(exit_code: u8) -> Self {
        Self {
            message_for_the_user: None,
            exit_code,
        }
    }
}

#[derive(Parser)]
#[command(
    name = "tatolab",
    version,
    about = "Tatolab — write a stream project, run it on tatolabd, and observe the runtimes \
             running on this machine.",
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
    /// Compile this stream in the project's venv and run it on tatolabd.
    Run(StreamLaunchArguments),
    /// Run this stream on tatolabd and restart it on every saved edit.
    Dev(StreamLaunchArguments),
    /// List the runtimes running on this machine.
    #[command(
        long_about = "Scans the node registry, liveness-checks every entry, prunes the ones that \
                      are gone, and prints runtime_name, runtime_id, local_api_socket, pid, alive? \
                      and hint. Only runtimes hosting a control plane register."
    )]
    Nodes,
    /// Export a running runtime's live graph as JSON.
    #[command(
        long_about = "Nodes, ports, links, channel names, states and metrics, as the runtime \
                      reports them right now."
    )]
    Graph(RuntimeTargetArguments),
    /// Collect a bounded sample of raw bags from one channel.
    #[command(
        long_about = "Attaches a read-only tap to CHANNEL and collects a bounded sample. The tap \
                      forwards bags verbatim and never blocks the producer, so a quiet channel \
                      returns a partial sample rather than hanging."
    )]
    Tap {
        /// The output port's address, <runtime_name>/<node>/<port>, as `graph` names them: its
        /// top-level runtime_name and a node's name.
        channel: String,
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
        #[command(flatten)]
        runtime_target: RuntimeTargetArguments,
    },
    /// Exchange published surface ids for PNG files on disk.
    #[command(
        long_about = "With SURFACE_ID, exchanges that one id. With --channel, taps the channel, \
                      reads a surface id out of each sampled bag, and exchanges it — one warm \
                      process, no window in the graph and no display server in the path. Writes \
                      exact full-resolution PNGs into --out and prints their paths on stdout, one \
                      per line — those paths are this run's frames, and --out is not cleared, so \
                      read them rather than listing the directory."
    )]
    Exchange(surface_image_exchange::SurfaceImageExchangeArguments),
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

/// The flags `run` and `dev` share; all but `--runtime-name` go to the compile entry verbatim.
#[derive(Args)]
pub(crate) struct StreamLaunchArguments {
    /// The stream to load: `<file>.py[:<function>]` or `<module>:<function>` (default: the sole
    /// @stream in stream.py).
    #[arg(value_name = "TARGET")]
    pub(crate) requested_stream_target: Option<OsString>,
    /// Entry file to launch, overriding the stream.py convention; not with TARGET.
    #[arg(short = 'f', long = "file", value_name = "FILE")]
    pub(crate) requested_entry_file: Option<OsString>,
    /// Project root to resolve the entry file or TARGET against (default: CWD, no walk-up).
    #[arg(long = "dir", value_name = "DIR")]
    pub(crate) requested_anchor_directory: Option<OsString>,
    /// Load the stream under this name instead of its function's.
    #[arg(long = "name", value_name = "NAME")]
    pub(crate) requested_stream_name: Option<OsString>,
    /// Name this runtime's tap channels begin with (else STREAMLIB_RUNTIME_NAME, else the
    /// engine's default).
    #[arg(long = "runtime-name", value_name = "NAME")]
    pub(crate) requested_runtime_name: Option<OsString>,
}

/// `--node`, which pins the runtime a verb drives; without it the verb takes the sole live one.
#[derive(Args, Debug, Clone, Default)]
pub(crate) struct RuntimeTargetArguments {
    /// Registered runtime name or runtime_id to target, reached through its local API socket
    /// (resolved via the node registry).
    #[arg(long = "node", value_name = "RUNTIME_NAME_OR_ID")]
    pub(crate) requested_runtime_name_or_id: Option<String>,
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
        TatolabVerb::Run(stream_launch_arguments) => {
            attached_tatolabd_supervisor::launch_stream_on_attached_tatolabd(
                attached_tatolabd_supervisor::StreamLaunchVerb::Run,
                &stream_launch_arguments,
            )
        }
        TatolabVerb::Dev(stream_launch_arguments) => {
            attached_tatolabd_supervisor::launch_stream_on_attached_tatolabd(
                attached_tatolabd_supervisor::StreamLaunchVerb::Dev,
                &stream_launch_arguments,
            )
        }
        TatolabVerb::Nodes => runtime_observation_verbs::print_node_registry_listing(),
        TatolabVerb::Graph(runtime_target) => {
            runtime_observation_verbs::print_local_api_tool_result_of_selected_runtime(
                runtime_target.requested_runtime_name_or_id.as_deref(),
                runtime_observation_verbs::GRAPH_TOOL_NAME,
                serde_json::Map::new(),
            )
        }
        TatolabVerb::Tap {
            channel,
            requested_bag_count,
            requested_max_bag_bytes,
            runtime_target,
        } => runtime_observation_verbs::print_local_api_tool_result_of_selected_runtime(
            runtime_target.requested_runtime_name_or_id.as_deref(),
            runtime_observation_verbs::TAP_TOOL_NAME,
            runtime_observation_verbs::tap_tool_arguments(
                &channel,
                requested_bag_count,
                requested_max_bag_bytes,
            ),
        ),
        TatolabVerb::Exchange(surface_image_exchange_arguments) => {
            surface_image_exchange::run_surface_image_exchange_verb(
                &surface_image_exchange_arguments,
            )
        }
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
