// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab run` attached and `tatolab dev`: the stream loaded into the machine's runtime over a
//! `/mcp/stdio` connection of this process's own, which the runtime ties the stream's life to,
//! and its records followed by sequence number until a signal stops it. `dev` loads it again on
//! every settled save and, when the runtime goes away, waits for it and loads again.

use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use streamlib_runtime_client_contract::tatolab_state_directory::TatolabStateDirectory;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::TatolabCommandFailure;
use crate::local_api_connection::tokio_runtime_for_a_local_api_connection;
use crate::local_api_mcp_tool_client::{
    LocalApiMcpToolClient, LocalApiMcpToolClientFailure, LocalApiMcpToolClientFailureKind,
};
use crate::machine_runtime_local_api_socket::{
    local_api_socket_of_the_running_runtime, something_answers_at,
};
use crate::process_signal_handling::block_the_stop_signals_and_listen;
use crate::project_source_change_watcher::watch_project_sources;
use crate::runtime_log_files_reader::RuntimeLogRecordFilters;
use crate::stream_actions_on_the_runtime::{
    RUN_STREAM_TOOL_NAME, RunStreamToolResult, STOP_STREAM_TOOL_NAME, StreamLoadArguments,
    StreamLoadRequest, stop_stream_tool_arguments,
};
use crate::stream_log_records_from_the_runtime::{
    LOGS_TOOL_NAME, STREAM_LOG_RECORDS_FOLLOW_POLL_INTERVAL, logs_tool_arguments_after,
    render_stream_log_records_page, stream_log_records_page_from,
};
use crate::verb_standard_output::standard_output_closed_or_failed;

/// Bounds a load: the compile in the project's interpreter, the description of its Python types
/// and the load itself, each bounded runtime-side well within it.
const ATTACHED_STREAM_LOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// Bounds every other request of an attached stream's connection.
const ATTACHED_STREAM_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How often `dev` looks for the runtime's socket again after losing its connection.
const RUNTIME_SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Which verb holds the attached stream: `dev` adds the re-load on save and the reconnect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttachedStreamVerb {
    /// `tatolab run`.
    Run,
    /// `tatolab dev`.
    Dev,
}

impl AttachedStreamVerb {
    fn note_prefix(self) -> &'static str {
        match self {
            AttachedStreamVerb::Run => "tatolab",
            AttachedStreamVerb::Dev => "tatolab dev",
        }
    }
}

/// What reaches an attached stream's session besides the runtime's answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachedStreamEvent {
    /// SIGINT, SIGTERM or SIGHUP.
    StopSignalDelivered,
    /// A save in the project settled.
    ProjectSourcesChanged,
}

/// A call that finished, or the stop signal that came first.
enum FinishedUnlessStopped<CallOutcome> {
    Finished(CallOutcome),
    StopSignalDelivered,
}

/// Where the session goes next.
enum AttachedStreamSessionStep {
    /// Load the stream over the connection.
    Load,
    /// Follow the loaded stream's records after `after`.
    Follow { stream: String, after: u64 },
    /// `dev` with nothing loaded: wait for the next save.
    WaitForTheNextSave,
    /// `dev` without a connection: wait for the runtime's socket, then connect and load.
    Reconnect,
    /// The session is over with this exit.
    Exit(Result<u8, TatolabCommandFailure>),
}

/// `tatolab run` attached and `tatolab dev`: load the stream attached, follow its records, and stop
/// it on a signal; `dev` re-loads on every settled save.
pub(crate) fn run_stream_attached(
    attached_stream_verb: AttachedStreamVerb,
    stream_load_arguments: &StreamLoadArguments,
    caller_working_directory: &Path,
) -> Result<u8, TatolabCommandFailure> {
    let stream_load_request =
        StreamLoadRequest::from_arguments(stream_load_arguments, caller_working_directory)?;
    let (attached_stream_event_sender, attached_stream_events) = unbounded_channel();
    let stop_signal_event_sender = attached_stream_event_sender.clone();
    block_the_stop_signals_and_listen(move |_delivered_signal| {
        let _session_already_over =
            stop_signal_event_sender.send(AttachedStreamEvent::StopSignalDelivered);
    })
    .map_err(|io_failure| {
        TatolabCommandFailure::refused(format!("cannot listen for signals: {io_failure}"))
    })?;
    let local_api_socket_path = local_api_socket_of_the_running_runtime()?;
    if attached_stream_verb == AttachedStreamVerb::Dev {
        watch_project_sources(
            PathBuf::from(&stream_load_request.project_directory),
            move || {
                attached_stream_event_sender
                    .send(AttachedStreamEvent::ProjectSourcesChanged)
                    .is_ok()
            },
        )
        .map_err(|io_failure| {
            TatolabCommandFailure::refused(format!("cannot watch the project: {io_failure}"))
        })?;
    }
    let session_tokio_runtime = tokio_runtime_for_a_local_api_connection()?;
    let session_outcome = session_tokio_runtime.block_on(
        AttachedStreamSession {
            attached_stream_verb,
            stream_load_request,
            local_api_socket_path,
            attached_stream_events,
            attached_stream_events_open: true,
            project_sources_changed_during_a_call: false,
        }
        .run_until_it_ends(),
    );
    session_tokio_runtime.shutdown_background();
    session_outcome
}

/// One attached stream's session with the runtime.
struct AttachedStreamSession {
    attached_stream_verb: AttachedStreamVerb,
    stream_load_request: StreamLoadRequest,
    local_api_socket_path: PathBuf,
    attached_stream_events: UnboundedReceiver<AttachedStreamEvent>,
    attached_stream_events_open: bool,
    project_sources_changed_during_a_call: bool,
}

impl AttachedStreamSession {
    async fn run_until_it_ends(mut self) -> Result<u8, TatolabCommandFailure> {
        let mut connected_client = Some(
            LocalApiMcpToolClient::connect_over_an_mcp_stdio_upgrade(
                &self.local_api_socket_path,
                ATTACHED_STREAM_REQUEST_TIMEOUT,
            )
            .await?,
        );
        let mut next_step = AttachedStreamSessionStep::Load;
        loop {
            next_step = match next_step {
                AttachedStreamSessionStep::Exit(session_outcome) => {
                    if let Some(connected_client) = connected_client.take() {
                        connected_client.close().await;
                    }
                    return session_outcome;
                }
                AttachedStreamSessionStep::Reconnect => {
                    connected_client = None;
                    match self.wait_for_the_runtime_and_connect().await {
                        Some(reconnected_client) => {
                            connected_client = Some(reconnected_client);
                            AttachedStreamSessionStep::Load
                        }
                        None => AttachedStreamSessionStep::Exit(Ok(0)),
                    }
                }
                AttachedStreamSessionStep::WaitForTheNextSave => {
                    self.wait_for_the_next_save().await
                }
                AttachedStreamSessionStep::Load => match &connected_client {
                    Some(connected_client) => self.load(connected_client).await,
                    None => AttachedStreamSessionStep::Reconnect,
                },
                AttachedStreamSessionStep::Follow { stream, after } => match &connected_client {
                    Some(connected_client) => self.follow(connected_client, stream, after).await,
                    None => AttachedStreamSessionStep::Reconnect,
                },
            };
        }
    }

    fn note(&self, note_text: &str) {
        eprintln!("{}: {note_text}", self.attached_stream_verb.note_prefix());
    }

    /// The next event, or `None` once nothing can send one.
    async fn next_event(&mut self) -> Option<AttachedStreamEvent> {
        if !self.attached_stream_events_open {
            return std::future::pending().await;
        }
        let next_event = self.attached_stream_events.recv().await;
        self.attached_stream_events_open = next_event.is_some();
        next_event
    }

    /// `call` to its end, unless a stop signal comes first; a save meanwhile is remembered.
    async fn finish_unless_stopped<CallOutcome>(
        &mut self,
        call: impl Future<Output = CallOutcome>,
    ) -> FinishedUnlessStopped<CallOutcome> {
        tokio::pin!(call);
        loop {
            tokio::select! {
                biased;
                next_event = self.next_event() => match next_event {
                    Some(AttachedStreamEvent::StopSignalDelivered) => {
                        return FinishedUnlessStopped::StopSignalDelivered;
                    }
                    Some(AttachedStreamEvent::ProjectSourcesChanged) => {
                        self.project_sources_changed_during_a_call = true;
                    }
                    None => {}
                },
                call_outcome = &mut call => return FinishedUnlessStopped::Finished(call_outcome),
            }
        }
    }

    async fn load(
        &mut self,
        connected_client: &LocalApiMcpToolClient,
    ) -> AttachedStreamSessionStep {
        self.project_sources_changed_during_a_call = false;
        let run_stream_arguments = self.stream_load_request.run_stream_tool_arguments(false);
        let load_outcome = match self
            .finish_unless_stopped(connected_client.call_tool_bounded_by(
                RUN_STREAM_TOOL_NAME,
                run_stream_arguments,
                ATTACHED_STREAM_LOAD_TIMEOUT,
            ))
            .await
        {
            FinishedUnlessStopped::Finished(load_outcome) => load_outcome,
            FinishedUnlessStopped::StopSignalDelivered => {
                self.note("stopped while the stream loaded; the runtime unloads it as this connection closes");
                return AttachedStreamSessionStep::Exit(Ok(0));
            }
        };
        let run_stream_result = match load_outcome.and_then(|run_stream_result_text| {
            serde_json::from_str::<RunStreamToolResult>(&run_stream_result_text).map_err(
                |parse_failure| {
                    LocalApiMcpToolClientFailure::request_refused_by_the_runtime(format!(
                        "run_stream answered something other than its result ({parse_failure}): \
                         {run_stream_result_text}"
                    ))
                },
            )
        }) {
            Ok(run_stream_result) => run_stream_result,
            Err(load_failure) => return self.after_a_failed_load(load_failure),
        };
        self.note(&format!(
            "{} loaded ({} nodes, project {}); Ctrl-C stops it",
            run_stream_result.stream,
            run_stream_result.node_count,
            run_stream_result.project_directory.display()
        ));
        if self.project_sources_changed_during_a_call {
            return self
                .reload_after_a_save(connected_client, &run_stream_result.stream)
                .await;
        }
        AttachedStreamSessionStep::Follow {
            stream: run_stream_result.stream,
            after: 0,
        }
    }

    fn after_a_failed_load(
        &mut self,
        load_failure: LocalApiMcpToolClientFailure,
    ) -> AttachedStreamSessionStep {
        match (self.attached_stream_verb, load_failure.kind) {
            (
                AttachedStreamVerb::Run,
                LocalApiMcpToolClientFailureKind::LocalApiConnectionClosed,
            ) => AttachedStreamSessionStep::Exit(Err(TatolabCommandFailure::refused(
                the_runtime_closed_the_connection_message(),
            ))),
            (AttachedStreamVerb::Run, _) => {
                AttachedStreamSessionStep::Exit(Err(load_failure.into()))
            }
            (
                AttachedStreamVerb::Dev,
                LocalApiMcpToolClientFailureKind::LocalApiConnectionClosed,
            ) => {
                self.note(&the_runtime_closed_the_connection_message());
                AttachedStreamSessionStep::Reconnect
            }
            (AttachedStreamVerb::Dev, LocalApiMcpToolClientFailureKind::LocalApiUnreachable) => {
                // A load that did not answer may still land; closing this connection unloads it.
                self.note(&format!("{load_failure}; connecting again"));
                AttachedStreamSessionStep::Reconnect
            }
            (AttachedStreamVerb::Dev, _) => {
                self.note(&load_failure.to_string());
                if self.project_sources_changed_during_a_call {
                    return AttachedStreamSessionStep::Load;
                }
                self.note("no stream is loaded — fix it and save again");
                AttachedStreamSessionStep::WaitForTheNextSave
            }
        }
    }

    async fn follow(
        &mut self,
        connected_client: &LocalApiMcpToolClient,
        stream: String,
        mut after: u64,
    ) -> AttachedStreamSessionStep {
        loop {
            let logs_outcome = match self
                .finish_unless_stopped(
                    connected_client
                        .call_tool(LOGS_TOOL_NAME, logs_tool_arguments_after(&stream, after)),
                )
                .await
            {
                FinishedUnlessStopped::Finished(logs_outcome) => logs_outcome,
                FinishedUnlessStopped::StopSignalDelivered => {
                    return self.stop_and_exit(connected_client, &stream).await;
                }
            };
            let stream_log_records_page = match logs_outcome.and_then(|logs_tool_result_text| {
                stream_log_records_page_from(&logs_tool_result_text)
                    .map_err(LocalApiMcpToolClientFailure::request_refused_by_the_runtime)
            }) {
                Ok(stream_log_records_page) => stream_log_records_page,
                Err(logs_failure) => {
                    return self
                        .after_a_failed_follow(connected_client, &stream, logs_failure)
                        .await;
                }
            };
            let rendered_records = render_stream_log_records_page(
                &stream,
                &stream_log_records_page,
                &RuntimeLogRecordFilters::default(),
                &mut std::io::stderr(),
            );
            if let Err(write_failure) = write_and_flush_to_standard_output(&rendered_records) {
                self.stop_the_stream(connected_client, &stream).await;
                return AttachedStreamSessionStep::Exit(standard_output_closed_or_failed(
                    write_failure,
                ));
            }
            let page_brought_nothing = stream_log_records_page.next_after == after;
            after = stream_log_records_page.next_after;
            if self.project_sources_changed_during_a_call {
                return self.reload_after_a_save(connected_client, &stream).await;
            }
            if !page_brought_nothing {
                continue;
            }
            match self
                .finish_unless_stopped(tokio::time::sleep(STREAM_LOG_RECORDS_FOLLOW_POLL_INTERVAL))
                .await
            {
                FinishedUnlessStopped::Finished(()) => {}
                FinishedUnlessStopped::StopSignalDelivered => {
                    return self.stop_and_exit(connected_client, &stream).await;
                }
            }
        }
    }

    async fn after_a_failed_follow(
        &mut self,
        connected_client: &LocalApiMcpToolClient,
        stream: &str,
        logs_failure: LocalApiMcpToolClientFailure,
    ) -> AttachedStreamSessionStep {
        match (self.attached_stream_verb, logs_failure.kind) {
            (_, LocalApiMcpToolClientFailureKind::ToolCallFailed) => {
                self.note(&format!(
                    "{stream} was unloaded by the runtime — stopped elsewhere or by its \
                     watchdog: {logs_failure}"
                ));
                match self.attached_stream_verb {
                    AttachedStreamVerb::Run => AttachedStreamSessionStep::Exit(Ok(0)),
                    AttachedStreamVerb::Dev => AttachedStreamSessionStep::WaitForTheNextSave,
                }
            }
            (
                AttachedStreamVerb::Run,
                LocalApiMcpToolClientFailureKind::LocalApiConnectionClosed,
            ) => AttachedStreamSessionStep::Exit(Err(TatolabCommandFailure::refused(
                the_runtime_closed_the_connection_message(),
            ))),
            (
                AttachedStreamVerb::Dev,
                LocalApiMcpToolClientFailureKind::LocalApiConnectionClosed,
            ) => {
                self.note(&the_runtime_closed_the_connection_message());
                AttachedStreamSessionStep::Reconnect
            }
            (AttachedStreamVerb::Run, _) => {
                self.stop_the_stream(connected_client, stream).await;
                AttachedStreamSessionStep::Exit(Err(logs_failure.into()))
            }
            (AttachedStreamVerb::Dev, _) => {
                self.note(&format!("{logs_failure}; connecting again"));
                AttachedStreamSessionStep::Reconnect
            }
        }
    }

    /// Ask the runtime to unload `stream`, noting a failure: closing the connection unloads it
    /// anyway.
    async fn stop_the_stream(&self, connected_client: &LocalApiMcpToolClient, stream: &str) {
        match connected_client
            .call_tool(STOP_STREAM_TOOL_NAME, stop_stream_tool_arguments(stream))
            .await
        {
            Ok(_stop_stream_result_text) => self.note(&format!("{stream} stopped")),
            Err(stop_failure) => self.note(&format!(
                "{stop_failure}; the runtime unloads {stream} as this connection closes"
            )),
        }
    }

    async fn stop_and_exit(
        &self,
        connected_client: &LocalApiMcpToolClient,
        stream: &str,
    ) -> AttachedStreamSessionStep {
        self.stop_the_stream(connected_client, stream).await;
        AttachedStreamSessionStep::Exit(Ok(0))
    }

    async fn reload_after_a_save(
        &mut self,
        connected_client: &LocalApiMcpToolClient,
        stream: &str,
    ) -> AttachedStreamSessionStep {
        self.note(&format!("a saved change — loading {stream} again"));
        match connected_client
            .call_tool(STOP_STREAM_TOOL_NAME, stop_stream_tool_arguments(stream))
            .await
        {
            Ok(_stop_stream_result_text) => AttachedStreamSessionStep::Load,
            Err(stop_failure)
                if stop_failure.kind
                    == LocalApiMcpToolClientFailureKind::LocalApiConnectionClosed =>
            {
                self.note(&the_runtime_closed_the_connection_message());
                AttachedStreamSessionStep::Reconnect
            }
            Err(stop_failure) => {
                self.note(&stop_failure.to_string());
                AttachedStreamSessionStep::Load
            }
        }
    }

    async fn wait_for_the_next_save(&mut self) -> AttachedStreamSessionStep {
        if std::mem::take(&mut self.project_sources_changed_during_a_call) {
            return AttachedStreamSessionStep::Load;
        }
        match self.next_event().await {
            Some(AttachedStreamEvent::ProjectSourcesChanged) => AttachedStreamSessionStep::Load,
            Some(AttachedStreamEvent::StopSignalDelivered) | None => {
                AttachedStreamSessionStep::Exit(Ok(0))
            }
        }
    }

    /// Poll for the runtime's socket until it answers and a connection opens over it; `None` when
    /// a stop signal ends the wait.
    async fn wait_for_the_runtime_and_connect(&mut self) -> Option<LocalApiMcpToolClient> {
        self.note(&format!(
            "waiting for a runtime to answer at {}",
            self.local_api_socket_path.display()
        ));
        loop {
            match self
                .finish_unless_stopped(tokio::time::sleep(RUNTIME_SOCKET_POLL_INTERVAL))
                .await
            {
                FinishedUnlessStopped::Finished(()) => {}
                FinishedUnlessStopped::StopSignalDelivered => return None,
            }
            if !something_answers_at(&self.local_api_socket_path) {
                continue;
            }
            match LocalApiMcpToolClient::connect_over_an_mcp_stdio_upgrade(
                &self.local_api_socket_path,
                ATTACHED_STREAM_REQUEST_TIMEOUT,
            )
            .await
            {
                Ok(connected_client) => {
                    self.note("the runtime answers again — loading the stream");
                    return Some(connected_client);
                }
                Err(connect_failure) => self.note(&format!("{connect_failure}; waiting")),
            }
        }
    }
}

/// What an attached stream's verb says when the runtime closed its connection unasked.
fn the_runtime_closed_the_connection_message() -> String {
    format!(
        "the runtime closed the connection — it crashed or was stopped; its log is under {}",
        runtime_log_directory_named_for_the_user()
    )
}

/// `<state dir>/logs/`, or the state directory's refusal when it has no place.
fn runtime_log_directory_named_for_the_user() -> String {
    match TatolabStateDirectory::resolve() {
        Ok(tatolab_state_directory) => {
            directory_with_its_trailing_slash(&tatolab_state_directory.runtime_log_directory())
        }
        Err(state_directory_refusal) => {
            format!(
                "the runtime's state directory, which this user cannot place: {state_directory_refusal}"
            )
        }
    }
}

fn directory_with_its_trailing_slash(directory: &Path) -> String {
    format!("{}/", directory.display())
}

fn write_and_flush_to_standard_output(rendered_records: &str) -> std::io::Result<()> {
    if rendered_records.is_empty() {
        return Ok(());
    }
    let mut locked_standard_output = std::io::stdout().lock();
    locked_standard_output.write_all(rendered_records.as_bytes())?;
    locked_standard_output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runtime_log_directory_is_named_with_its_trailing_slash() {
        assert_eq!(
            directory_with_its_trailing_slash(Path::new("/home/u/.local/state/tatolab/logs")),
            "/home/u/.local/state/tatolab/logs/"
        );
    }
}
