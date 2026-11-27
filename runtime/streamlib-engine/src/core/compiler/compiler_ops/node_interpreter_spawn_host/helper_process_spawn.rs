// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Instant;

use crate::core::child_process_lifetime::{
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours,
    give_the_child_no_descriptor_beyond_stdio,
};
use crate::core::compiler::compiler_ops::subprocess_bridge::{
    EscalateTransport, SETUP_LIFECYCLE_COMMAND_TO_HELPER_PROCESS, SubprocessBridge,
    spawn_fd_line_reader,
};
use crate::core::compiler::compiler_ops::subprocess_escalate::WhatAHelperPlacedNodeReaches;
use crate::core::compiler::compiler_ops::subprocess_escalate::shared_native_service_lease::SharedNativeServiceLeasesOfOneHelper;
use crate::core::context::{
    CAPTURE_DEVICE_ACCESS_PROMPTING_ENVIRONMENT_VARIABLE, CaptureDeviceAccessPrompting,
    GpuContextLimitedAccess, RuntimeContextFullAccess,
};
use crate::core::error::{Error, Result};
use crate::core::execution::ProcessExecution;
use crate::iceoryx2::{
    ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE, spawn_outside_every_iceoryx2_listener_bind,
};

use super::{
    HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE,
    NODE_INTERPRETER_ENTRYPOINT_ENVIRONMENT_VARIABLE,
    NODE_INTERPRETER_NODE_NAME_ENVIRONMENT_VARIABLE,
    NODE_INTERPRETER_NODE_RUN_ID_ENVIRONMENT_VARIABLE,
    NODE_INTERPRETER_STREAM_ID_ENVIRONMENT_VARIABLE,
    NODE_INTERPRETER_STREAM_PROJECT_DIRECTORY_ENVIRONMENT_VARIABLE, NodeInterpreterSpawnHost,
    REGISTRATION_DEADLINE, REGISTRATION_SHUTDOWN_OBSERVATION_INTERVAL,
    SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE, SurfaceShareChannelNamedToTheHelperProcess,
    lifecycle_reply_tag, node_interpreter_bootstrap_command, node_interpreter_bootstrap_path,
    refusal_of_a_helper_process_that_died_while_setting_up, reported_reason,
    spawn_standard_error_reader_keeping_its_tail,
};

impl NodeInterpreterSpawnHost {
    /// The setup command its helper is sent, carrying `ports`, the board it writes loss counts
    /// on and the stream clock it reads.
    pub(super) fn the_setup_command_to_its_helper(
        &self,
        ports: serde_json::Value,
        loss_count_board: serde_json::Value,
        stream_clock_named_to_the_helper: serde_json::Value,
    ) -> serde_json::Value {
        // The helper's writer keeps the last message of each, as a native node's does.
        let output_ports_keeping_their_last_message: Vec<&str> = self
            .descriptor
            .outputs
            .iter()
            .filter(|output_port| output_port.flows_as_a_value())
            .map(|output_port| output_port.name.as_str())
            .collect();
        serde_json::json!({
            // The engine's own constant: the escalate dispatch reads
            // this exact spelling to decide that a window may be
            // minted, and a rename on one side alone would refuse
            // every window silently.
            "cmd": SETUP_LIFECYCLE_COMMAND_TO_HELPER_PROCESS,
            "capability": "full",
            "input_values": self.node_input_values,
            "node_run_id": self.node_run_id,
            "ports": ports,
            "loss_count_board": loss_count_board,
            "output_ports_keeping_their_last_message": output_ports_keeping_their_last_message,
            "function_node_evaluated_at_setup": self.its_function_node_is_evaluated_at_setup,
            "stream_clock": stream_clock_named_to_the_helper,
        })
    }

    /// Build the command that becomes the child.
    ///
    /// Separate from the spawn so what a child inherits is assertable without
    /// starting one.
    pub(crate) fn build_helper_process_command(
        &self,
        runtime_id: &str,
        stream_id: &str,
        iceoryx2_domain_root: &Path,
        surface_share_channel: Option<SurfaceShareChannelNamedToTheHelperProcess<'_>>,
        runtime_vulkan_device_uuid: Option<[u8; 16]>,
        capture_device_access_prompting: CaptureDeviceAccessPrompting,
    ) -> Result<Command> {
        let mut command = node_interpreter_bootstrap_command(
            &self.stream_environment,
            &self.node_interpreter_lend_directory,
        )
        .map_err(|refusal| {
            Error::Runtime(format!(
                "[{}] could not start its node interpreter: {refusal}",
                self.node_display_name
            ))
        })?;
        command
            // The child never reads stdin; its fd1/fd2 are captured as
            // intercepted log pipes, and the framed protocol rides its own
            // socket so neither can corrupt it.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env(
                NODE_INTERPRETER_ENTRYPOINT_ENVIRONMENT_VARIABLE,
                &self.node_class_import_path,
            )
            .env(
                NODE_INTERPRETER_NODE_RUN_ID_ENVIRONMENT_VARIABLE,
                &self.node_run_id,
            )
            .env("STREAMLIB_RUNTIME_ID", runtime_id)
            .env(
                NODE_INTERPRETER_NODE_NAME_ENVIRONMENT_VARIABLE,
                &self.node_display_name,
            )
            .env(NODE_INTERPRETER_STREAM_ID_ENVIRONMENT_VARIABLE, stream_id)
            .env(
                NODE_INTERPRETER_STREAM_PROJECT_DIRECTORY_ENVIRONMENT_VARIABLE,
                &self.stream_environment.project_directory,
            )
            .env(
                CAPTURE_DEVICE_ACCESS_PROMPTING_ENVIRONMENT_VARIABLE,
                capture_device_access_prompting.as_environment_value(),
            )
            .env(
                ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE,
                iceoryx2_domain_root,
            );
        if let Some(surface_share_channel) = surface_share_channel {
            command.env(
                surface_share_channel.environment_variable,
                surface_share_channel.channel_name,
            );
        }
        if let Some(runtime_vulkan_device_uuid) = runtime_vulkan_device_uuid {
            streamlib_consumer_rhi::name_the_runtime_vulkan_device_to_a_spawned_process(
                &mut command,
                runtime_vulkan_device_uuid,
            );
        }
        detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
        // Registered before `EscalateTransport::attach`, whose own `pre_exec`
        // clears `FD_CLOEXEC` on the escalate socket's child end: `pre_exec`
        // closures run in registration order, so the one descriptor a helper is
        // owed is handed back after this sweep has marked everything.
        give_the_child_no_descriptor_beyond_stdio(&mut command);
        Ok(command)
    }

    /// The mode string the child drives its own loop in.
    pub(super) fn child_execution_mode(&self) -> &'static str {
        match self.link_wiring.far_side_process_execution() {
            ProcessExecution::Reactive => "reactive",
            ProcessExecution::Continuous { .. } => "continuous",
            ProcessExecution::Manual => "manual",
        }
    }

    /// Wait for the child's `ready`, killing it if the deadline passes.
    ///
    /// An unbounded wait here is what turns a class that blocks at import time
    /// into a graph that never comes up and never says why.
    pub(super) fn await_child_registration(&mut self) -> Result<()> {
        let bridge = self
            .bridge
            .as_ref()
            .ok_or_else(|| Error::Runtime("there is no helper process to wait for".to_string()))?;
        let deadline = Instant::now() + REGISTRATION_DEADLINE;
        let reply = loop {
            if self
                .the_stream_this_helper_belongs_to
                .shutdown_escalation
                .is_requested()
            {
                // A helper still importing when shutdown begins must not hold
                // the app for the rest of a sixty-second budget. It goes on the
                // ladder rather than taking a bare kill, because
                // the engine has any callback
                // interrupted at shutdown — `setup()` included — followed by
                // `teardown()`.
                self.stop_the_helper_process_on_the_shutdown_ladder();
                return Err(Error::Runtime(format!(
                    "[{}] its stream's shutdown began while its helper process was still \
                     setting up",
                    self.node_display_name,
                )));
            }
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .min(REGISTRATION_SHUTDOWN_OBSERVATION_INTERVAL);
            if Instant::now() >= deadline {
                self.take_the_helper_process_group_down();
                return Err(Error::Runtime(format!(
                    "[{}] its helper process did not finish setting up within {}s. The class is \
                     imported from `{}` in a fresh interpreter — work that blocks at import time \
                     blocks here.",
                    self.node_display_name,
                    REGISTRATION_DEADLINE.as_secs(),
                    self.node_class_import_path,
                )));
            }
            match bridge.recv_lifecycle_timeout(remaining) {
                Ok(reply) => break reply,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(self.refuse_the_helper_process_that_died_while_setting_up());
                }
            }
        };

        match lifecycle_reply_tag(&reply) {
            Some("ready") => {
                self.link_wiring
                    .note_how_its_far_side_ran_its_function_nodes(
                        reply.get("function_nodes_ran_compiled"),
                    );
                self.link_wiring
                    .note_whether_its_far_side_takes_its_pin_group_slots_live(
                        reply.get("takes_its_pin_group_slots_live"),
                    );
                Ok(())
            }
            _ => {
                let reported = reported_reason(&reply).to_string();
                // A helper that refused its own setup is never run and never
                // torn down, so this is the only place its group goes.
                self.take_the_helper_process_group_down();
                Err(Error::Runtime(format!(
                    "[{}] could not set itself up in its helper process:\n{reported}",
                    self.node_display_name
                )))
            }
        }
    }

    /// The refusal of a child that exited before `ready`, carrying the end of
    /// what it wrote to its standard error.
    ///
    /// A helper refuses its own start — an engine build other than the
    /// parent's, a missing variable — before its log channel exists, so raw
    /// standard error is the only place its reason is written.
    pub(super) fn refuse_the_helper_process_that_died_while_setting_up(&mut self) -> Error {
        // The group first, so nothing the helper started is still holding the
        // standard-error pipe the tail below waits on — and so a refused start
        // leaves no survivor, which the engine
        // owes at every helper exit and not only at the ones that reach
        // teardown.
        self.take_the_helper_process_group_down();
        let standard_error_tail = self
            .child_standard_error_tail
            .as_ref()
            .map(|tail| {
                tail.text_once_closed_or_after(HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE)
            })
            .unwrap_or_default();
        refusal_of_a_helper_process_that_died_while_setting_up(
            &self.node_display_name,
            &standard_error_tail,
        )
    }

    /// Hold every link recorded from now on behind the setup command, run
    /// `start_the_helper_process`, and give the helper up if that fails — so a
    /// link held behind a setup command that never went out is refused rather
    /// than pending for good.
    pub(super) fn set_up_holding_every_later_link_until_the_setup_command_goes_out(
        &mut self,
        start_the_helper_process: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        self.link_wiring
            .hold_every_later_link_until_the_setup_command_goes_out();
        let set_up = start_the_helper_process(self);
        if set_up.is_err() {
            self.give_up_on_the_helper_process();
        }
        set_up
    }

    /// Start the child and wait for its `ready` — everything `setup` does once
    /// later links are held.
    pub(super) fn start_the_helper_process_and_await_its_registration(
        &mut self,
        ctx: &RuntimeContextFullAccess<'_>,
    ) -> Result<()> {
        #[cfg(target_os = "linux")]
        let surface_share_channel = Some(SurfaceShareChannelNamedToTheHelperProcess {
            environment_variable: SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE,
            channel_name: ctx.surface_socket_path().as_os_str(),
        });
        #[cfg(target_os = "macos")]
        let surface_share_mach_service_rendezvous = ctx.surface_share_mach_service_rendezvous();
        #[cfg(target_os = "macos")]
        let surface_share_channel = Some(SurfaceShareChannelNamedToTheHelperProcess {
            environment_variable: SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE,
            channel_name: std::ffi::OsStr::new(
                surface_share_mach_service_rendezvous.service_name(),
            ),
        });
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let surface_share_channel = None;
        // The device every surface the helper imports was exported from: an
        // OPAQUE_FD import binds only on the exporting `deviceUUID`.
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let runtime_vulkan_device_uuid = Some(
            crate::host_rhi::HostGpuDeviceExt::vulkan_device(
                ctx.gpu_limited_access().host_inner().device().as_ref(),
            )
            .physical_device_uuid(),
        );
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let runtime_vulkan_device_uuid = None;

        // Before the child exists, so its first counts have a board to land on
        // and the board outlives whatever becomes of it.
        let mut loss_count_board = self
            .link_wiring
            .create_the_loss_count_board_for_this_helper_spawn(
                ctx,
                &self.node_run_id,
                self.descriptor
                    .outputs
                    .iter()
                    .map(|output_port| output_port.name.clone())
                    .collect(),
            )
            .map_err(|board_failure| {
                Error::Runtime(format!(
                    "[{}] could not create the board its helper process writes loss counts on: \
                     {board_failure}",
                    self.node_display_name
                ))
            })?;
        if let (Some(frame_interval), Some(board_named_to_the_helper)) = (
            self.frame_interval_its_helper_times_function_node_calls_against(),
            loss_count_board.as_object_mut(),
        ) {
            board_named_to_the_helper.insert(
                "function_node_frame_interval_ns".to_string(),
                serde_json::json!(frame_interval.as_nanos() as u64),
            );
        }
        let stream_clock = &ctx.host_base().stream_clock;
        let stream_clock_named_to_the_helper = serde_json::json!({
            "mode": stream_clock.spec().mode_name(),
            "board_service_name": stream_clock.board_service_name_for_helpers(),
        });
        let iceoryx2_domain_root = ctx.runtime_directory().iceoryx2_domain_root();
        let mut command = self.build_helper_process_command(
            &ctx.runtime_id(),
            ctx.stream_id(),
            &iceoryx2_domain_root,
            surface_share_channel,
            runtime_vulkan_device_uuid,
            ctx.capture_device_access_prompting(),
        )?;
        self.iceoryx2_domain_root = Some(iceoryx2_domain_root);
        let mut escalate_transport = EscalateTransport::attach(&mut command)?;

        let spawned_helper_process = spawn_outside_every_iceoryx2_listener_bind(&mut command);
        let mut child = spawned_helper_process.map_err(|spawn_failure| {
            Error::Runtime(format!(
                "[{}] could not start its node interpreter `{} {}`: {spawn_failure}",
                self.node_display_name,
                self.stream_environment.interpreter.display(),
                node_interpreter_bootstrap_path(&self.node_interpreter_lend_directory).display(),
            ))
        })?;
        // At once: a connect that arrives before its admission waits for it,
        // but only for a bounded while.
        #[cfg(target_os = "macos")]
        {
            self.surface_share_admission_of_the_helper_process =
                Some(surface_share_mach_service_rendezvous.admit_helper_process(child.id()));
        }
        // After the spawn, so the child is the only holder of its end and sees
        // EOF when this process lets go.
        escalate_transport.release_child_end();

        tracing::info!(
            "[{}] helper process started: pid={}, entrypoint={}",
            self.node_display_name,
            child.id(),
            self.node_class_import_path,
        );
        // `pre_exec` made the child the leader of a group whose id is its pid.
        if !crate::core::runtime::register_a_helper_process_group(
            child.id() as i32,
            self.the_stream_this_helper_belongs_to.stream_tag,
        ) {
            tracing::warn!(
                "[{}] its helper process group could not be registered, so a third interrupt \
                 will not kill it; the kernel still kills the helper itself when the app exits",
                self.node_display_name,
            );
        }

        // fd1/fd2 carry anything that bypasses `tatolab.stream.log` — a raw
        // `os.write`, a C extension's `printf`, an interpreter-level fatal —
        // and each line becomes an `intercepted` record in the unified JSONL.
        if let Some(child_stdout) = child.stdout.take() {
            spawn_fd_line_reader(child_stdout, "py-stdout", "fd1", &self.node_run_id);
        }
        if let Some(child_stderr) = child.stderr.take() {
            self.child_standard_error_tail = Some(spawn_standard_error_reader_keeping_its_tail(
                child_stderr,
                &self.node_run_id,
            ));
        }

        self.child = Some(child);
        let escalate_transport_parent_ends = escalate_transport.into_parent_ends();
        let bridge = SubprocessBridge::new(
            escalate_transport_parent_ends.bridge,
            Some(escalate_transport_parent_ends.native_escalate),
            // Uncounted: the helper's one-offs are counted by its escalate
            // registry, never also against this spawn host.
            GpuContextLimitedAccess::new(ctx.gpu_limited_access().host_inner().clone()),
            self.node_run_id.clone(),
            WhatAHelperPlacedNodeReaches {
                shared_native_service_leases: Arc::new(
                    SharedNativeServiceLeasesOfOneHelper::for_a_helper_of_the_stream(
                        Arc::clone(ctx.host_base().shared_native_services()),
                        ctx.stream_id().to_string(),
                    ),
                ),
                store_of_the_node: Some(ctx.host_base().store_of_this_node_opened_on_first_use()),
                ..self.what_the_node_reaches.clone()
            },
        )?;

        // Through the envelope, so the links it carries go out with this
        // command and every link held behind it is handed over after.
        let setup_command_sent = self
            .link_wiring
            .send_the_setup_command_then_hand_every_later_link_over(
                |ports| {
                    bridge.send(&self.the_setup_command_to_its_helper(
                        ports,
                        loss_count_board,
                        stream_clock_named_to_the_helper,
                    ))
                },
                bridge.link_delivery_to_this_subprocess(&self.node_display_name),
            );
        self.bridge = Some(bridge);
        if let Err(setup_command_send_failure) = setup_command_sent {
            // The child's end is already closed: it refused its own start
            // before reading anything.
            tracing::debug!(
                "[{}] could not send its helper process the setup command: \
                 {setup_command_send_failure}",
                self.node_display_name
            );
            return Err(self.refuse_the_helper_process_that_died_while_setting_up());
        }
        self.await_child_registration()
    }
}
