// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab dev`: the project's streams loaded into the machine's runtime over a `/mcp/stdio`
//! connection of this process's own, the project's log followed by cursor from where it ended
//! when `dev` started — across every re-load and restart, since the log is the project's — and
//! run again on every settled save, which the runtime answers by replacing each running stream,
//! or by recording it failed with the traceback `dev` prints when the save's code fails.
//! Ctrl-C ends the following and leaves the streams loaded. When the runtime goes away, `dev`
//! waits for it, waits for the restarted runtime to re-load the stream from its record, and
//! follows the log again; it never loads the stream again for a crash.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use streamlib_runtime_client_contract::local_api_wire_contract::{
    ListStreamsToolResult, ListedStreamState, RunStreamToolResult, RunStreamToolResultStream,
    RunStreamToolResultStreamState,
};
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
use crate::project_log_records_from_the_runtime::{
    DESCRIBE_RUNTIME_TOOL_NAME, LOGS_TOOL_NAME, PROJECT_LOG_RECORDS_FOLLOW_POLL_INTERVAL,
    ProjectLogRecordsAsked, project_log_records_page_from, render_project_log_records_page,
};
use crate::project_source_change_watcher::watch_project_sources;
use crate::runtime_gone_while_following_a_stream::{
    RuntimeLogDirectoryAsDescribed, the_runtime_is_gone,
};
use crate::runtime_log_record_decoding_and_filters::RuntimeLogRecordFilters;
use crate::stream_actions_on_the_runtime::{
    StreamActionTool, StreamLoadArguments, StreamLoadRequest, rendered_compile_warning_lines,
    the_stream_a_run_took_in_described, tool_result_from,
};
use crate::verb_standard_output::{
    standard_output_closed_or_failed, write_and_flush_verb_standard_output,
    write_verb_standard_error,
};

/// Bounds every request of `dev`'s connection but a stream action's, which each waits as long as
/// its own tool's timeout.
const DEV_CONNECTION_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How often `dev` looks for the runtime's socket again after losing its connection, and asks a
/// restarted runtime again whether it has re-loaded the followed stream.
const RUNTIME_SOCKET_AND_RE_LOAD_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// What `dev` prefixes each of its notes on standard error with.
const DEV_NOTE_PREFIX: &str = "tatolab dev";

/// What reaches `dev`'s session besides the runtime's answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DevSessionEvent {
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

/// The stream `dev` follows the project's log for, by its id, and whether a refused reload left
/// it to confirm the stream still runs.
#[derive(Debug, Clone)]
struct FollowedStreamPosition {
    stream: String,
    /// A reload was refused, and the next `logs` page confirms the runtime still answers for the
    /// stream the reload was to replace.
    a_refused_reload_left_it_to_confirm: bool,
}

impl FollowedStreamPosition {
    /// `stream`, followed in the project's log from where the session read up to.
    fn following(stream: String) -> Self {
        Self {
            stream,
            a_refused_reload_left_it_to_confirm: false,
        }
    }
}

/// Where the session goes next.
enum DevSessionStep {
    /// Run the stream over the connection; a stream already followed is the one the run replaces.
    Run(Option<FollowedStreamPosition>),
    /// Follow the loaded stream's records.
    Follow(FollowedStreamPosition),
    /// Nothing is followed: wait for the next save.
    WaitForTheNextSave,
    /// Without a connection: wait for the runtime's socket, then connect, and follow the stream
    /// again once the runtime has re-loaded it.
    WaitForTheRuntime(Option<String>),
    /// Wait until the restarted runtime has re-loaded this stream from its record.
    WaitForTheReLoadOf(String),
    /// The session is over with this exit.
    Exit(Result<u8, TatolabCommandFailure>),
}

/// `tatolab dev`: load the stream, follow its records, and run it again on every settled save,
/// until a stop signal ends the following.
pub(crate) fn follow_the_stream_and_reload_it_on_every_save(
    stream_load_arguments: &StreamLoadArguments,
    caller_working_directory: &Path,
) -> Result<u8, TatolabCommandFailure> {
    let stream_load_request =
        StreamLoadRequest::from_arguments(stream_load_arguments, caller_working_directory)?;
    let (dev_session_event_sender, dev_session_events) = unbounded_channel();
    let stop_signal_event_sender = dev_session_event_sender.clone();
    block_the_stop_signals_and_listen(move |_delivered_signal| {
        let _session_already_over =
            stop_signal_event_sender.send(DevSessionEvent::StopSignalDelivered);
    })
    .map_err(|io_failure| {
        TatolabCommandFailure::refused(format!("cannot listen for signals: {io_failure}"))
    })?;
    let local_api_socket_path = local_api_socket_of_the_running_runtime()?;
    watch_project_sources(
        PathBuf::from(&stream_load_request.project_directory),
        move || {
            dev_session_event_sender
                .send(DevSessionEvent::ProjectSourcesChanged)
                .is_ok()
        },
    )
    .map_err(|io_failure| {
        TatolabCommandFailure::refused(format!("cannot watch the project: {io_failure}"))
    })?;
    let session_tokio_runtime = tokio_runtime_for_a_local_api_connection()?;
    let session_outcome = session_tokio_runtime.block_on(
        DevSession {
            stream_load_request,
            local_api_socket_path,
            dev_session_events,
            dev_session_events_open: true,
            project_sources_changed_during_a_call: false,
            runtime_log_directory: RuntimeLogDirectoryAsDescribed::NotDescribed,
            project_log_after: None,
        }
        .run_until_it_ends(),
    );
    session_tokio_runtime.shutdown_background();
    session_outcome
}

/// One `dev` run: the load it asks for, the runtime it talks to, and what it has seen.
struct DevSession {
    stream_load_request: StreamLoadRequest,
    local_api_socket_path: PathBuf,
    dev_session_events: UnboundedReceiver<DevSessionEvent>,
    dev_session_events_open: bool,
    /// A save settled while a call was in flight; the session acts on it once the call is over.
    project_sources_changed_during_a_call: bool,
    /// Where the runtime said its own log is, named when it goes away.
    runtime_log_directory: RuntimeLogDirectoryAsDescribed,
    /// The cursor the project's log is read on from; `None` until the session read to its end.
    project_log_after: Option<String>,
}

impl DevSession {
    async fn run_until_it_ends(mut self) -> Result<u8, TatolabCommandFailure> {
        let mut connected_client = match self.connect_unless_stopped().await {
            FinishedUnlessStopped::Finished(connect_outcome) => Some(connect_outcome?),
            FinishedUnlessStopped::StopSignalDelivered => {
                self.note("stopped before the runtime opened a connection; nothing was loaded");
                return Ok(0);
            }
        };
        let mut next_step = DevSessionStep::Run(None);
        loop {
            next_step = match next_step {
                DevSessionStep::Exit(session_outcome) => {
                    if let Some(connected_client) = connected_client.take() {
                        connected_client.close().await;
                    }
                    return session_outcome;
                }
                DevSessionStep::WaitForTheRuntime(followed_stream) => {
                    connected_client = None;
                    match self.wait_for_the_runtime_and_connect().await {
                        Some(reconnected_client) => {
                            connected_client = Some(reconnected_client);
                            match followed_stream {
                                Some(stream) => DevSessionStep::WaitForTheReLoadOf(stream),
                                None => {
                                    self.note(
                                        "the runtime answers again — nothing was loaded before \
                                         it went away; save again to load the stream",
                                    );
                                    DevSessionStep::WaitForTheNextSave
                                }
                            }
                        }
                        None => DevSessionStep::Exit(Ok(0)),
                    }
                }
                DevSessionStep::WaitForTheNextSave => self.wait_for_the_next_save().await,
                DevSessionStep::Run(followed_stream_the_run_replaces) => match &connected_client {
                    Some(connected_client) => {
                        self.run_the_stream_over_the_connection(
                            connected_client,
                            followed_stream_the_run_replaces,
                        )
                        .await
                    }
                    None => DevSessionStep::WaitForTheRuntime(
                        followed_stream_the_run_replaces.map(|followed| followed.stream),
                    ),
                },
                DevSessionStep::WaitForTheReLoadOf(stream) => match &connected_client {
                    Some(connected_client) => {
                        self.wait_for_the_re_load_of(connected_client, stream).await
                    }
                    None => DevSessionStep::WaitForTheRuntime(Some(stream)),
                },
                DevSessionStep::Follow(followed) => match &connected_client {
                    Some(connected_client) => self.follow(connected_client, followed).await,
                    None => DevSessionStep::WaitForTheRuntime(Some(followed.stream)),
                },
            };
        }
    }

    /// A `/mcp/stdio` connection of the session's own, unless a stop signal comes first; once it
    /// opens, the runtime's description names where its log is.
    async fn connect_unless_stopped(
        &mut self,
    ) -> FinishedUnlessStopped<Result<LocalApiMcpToolClient, LocalApiMcpToolClientFailure>> {
        let local_api_socket_path = self.local_api_socket_path.clone();
        let connected = self
            .finish_unless_stopped(LocalApiMcpToolClient::connect_over_an_mcp_stdio_upgrade(
                &local_api_socket_path,
                DEV_CONNECTION_REQUEST_TIMEOUT,
            ))
            .await;
        if let FinishedUnlessStopped::Finished(Ok(connected_client)) = &connected {
            self.runtime_log_directory =
                RuntimeLogDirectoryAsDescribed::from_the_describe_runtime_answer(
                    connected_client
                        .call_tool(DESCRIBE_RUNTIME_TOOL_NAME, serde_json::Map::new())
                        .await,
                );
        }
        connected
    }

    fn note(&self, note_text: &str) {
        write_verb_standard_error(&format!("{DEV_NOTE_PREFIX}: {note_text}\n"));
    }

    /// The next event, or `None` once nothing can send one.
    async fn next_event(&mut self) -> Option<DevSessionEvent> {
        if !self.dev_session_events_open {
            return std::future::pending().await;
        }
        let next_event = self.dev_session_events.recv().await;
        self.dev_session_events_open = next_event.is_some();
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
                    Some(DevSessionEvent::StopSignalDelivered) => {
                        return FinishedUnlessStopped::StopSignalDelivered;
                    }
                    Some(DevSessionEvent::ProjectSourcesChanged) => {
                        self.project_sources_changed_during_a_call = true;
                    }
                    None => {}
                },
                call_outcome = &mut call => return FinishedUnlessStopped::Finished(call_outcome),
            }
        }
    }

    /// The exit a stop signal earns while `stream` is followed: the stream runs on.
    fn stop_following(&self, stream: &str) -> DevSessionStep {
        self.note(&format!(
            "stopped following; {stream} keeps running — `tatolab stop {stream}` stops it"
        ));
        DevSessionStep::Exit(Ok(0))
    }

    /// Run the stream over the connection; `followed_stream_the_run_replaces` is the stream
    /// already followed, which the run replaces — failed, when the save's code fails.
    async fn run_the_stream_over_the_connection(
        &mut self,
        connected_client: &LocalApiMcpToolClient,
        followed_stream_the_run_replaces: Option<FollowedStreamPosition>,
    ) -> DevSessionStep {
        self.project_sources_changed_during_a_call = false;
        if self.project_log_after.is_none() {
            match self.read_the_project_log_to_its_end(connected_client).await {
                FinishedUnlessStopped::Finished(Ok(end_of_the_project_log)) => {
                    self.project_log_after = Some(end_of_the_project_log);
                }
                FinishedUnlessStopped::Finished(Err(logs_failure)) => {
                    if let Some(after_the_runtime_went_away) = self.after_the_runtime_went_away(
                        &logs_failure,
                        followed_stream_the_run_replaces
                            .as_ref()
                            .map(|followed| followed.stream.as_str()),
                    ) {
                        return after_the_runtime_went_away;
                    }
                    self.note(&format!(
                        "the project's log could not be read to its end: {logs_failure}"
                    ));
                }
                FinishedUnlessStopped::StopSignalDelivered => return DevSessionStep::Exit(Ok(0)),
            }
        }
        let run_stream_arguments = self.stream_load_request.run_stream_tool_arguments();
        let run_outcome = match self
            .finish_unless_stopped(connected_client.call_tool_bounded_by(
                StreamActionTool::RunStream.tool_name(),
                run_stream_arguments,
                StreamActionTool::RunStream.tool_call_timeout(),
            ))
            .await
        {
            FinishedUnlessStopped::Finished(run_outcome) => run_outcome,
            FinishedUnlessStopped::StopSignalDelivered => {
                self.note(
                    "stopped while the stream loaded; the runtime finishes the load, and \
                     `tatolab streams` lists what it holds",
                );
                return DevSessionStep::Exit(Ok(0));
            }
        };
        let run_stream_result = match run_outcome.and_then(|run_stream_result_text| {
            tool_result_from::<RunStreamToolResult>(
                StreamActionTool::RunStream.tool_name(),
                &run_stream_result_text,
            )
            .map_err(LocalApiMcpToolClientFailure::request_refused_by_the_runtime)
        }) {
            Ok(run_stream_result) => run_stream_result,
            Err(run_failure) => {
                return self.after_a_failed_run(run_failure, followed_stream_the_run_replaces);
            }
        };
        write_verb_standard_error(&rendered_compile_warning_lines(&run_stream_result));
        let Some(stream) = run_stream_result.streams.into_iter().next() else {
            self.note(NO_STREAM_IS_LOADED_UNTIL_A_SAVE_COMPILES);
            return DevSessionStep::WaitForTheNextSave;
        };
        self.note(&the_stream_a_run_took_in_described(&stream));
        match stream {
            RunStreamToolResultStream {
                stream,
                state: RunStreamToolResultStreamState::Running,
                ..
            } => {
                let loaded = FollowedStreamPosition::following(stream);
                if self.project_sources_changed_during_a_call {
                    return self.run_again_after_a_save(loaded);
                }
                DevSessionStep::Follow(loaded)
            }
            RunStreamToolResultStream { stream, .. } => {
                self.note(&format!(
                    "{stream} is failed — fix it and save again to load it"
                ));
                DevSessionStep::WaitForTheNextSave
            }
        }
    }

    /// Where the session goes after `failure` reached it: when the runtime is gone, a note naming
    /// its log, then a wait for it that follows `followed_stream` again. `None` when the runtime
    /// is still there.
    fn after_the_runtime_went_away(
        &self,
        failure: &LocalApiMcpToolClientFailure,
        followed_stream: Option<&str>,
    ) -> Option<DevSessionStep> {
        if !the_runtime_is_gone(failure) {
            return None;
        }
        self.note(&self.runtime_log_directory.the_runtime_went_away_message());
        Some(DevSessionStep::WaitForTheRuntime(
            followed_stream.map(str::to_owned),
        ))
    }

    /// Where the session goes after a refused run: it follows the stream the run was to replace,
    /// which a refused run leaves as it was, or waits for a save when nothing was running.
    fn after_a_failed_run(
        &mut self,
        run_failure: LocalApiMcpToolClientFailure,
        followed_stream_the_run_replaces: Option<FollowedStreamPosition>,
    ) -> DevSessionStep {
        if let Some(after_the_runtime_went_away) = self.after_the_runtime_went_away(
            &run_failure,
            followed_stream_the_run_replaces
                .as_ref()
                .map(|followed| followed.stream.as_str()),
        ) {
            return after_the_runtime_went_away;
        }
        self.note(&run_failure.to_string());
        match followed_stream_the_run_replaces {
            Some(followed) if self.project_sources_changed_during_a_call => {
                DevSessionStep::Run(Some(followed))
            }
            Some(followed) => DevSessionStep::Follow(FollowedStreamPosition {
                a_refused_reload_left_it_to_confirm: true,
                ..followed
            }),
            None if self.project_sources_changed_during_a_call => DevSessionStep::Run(None),
            None => {
                self.note(NO_STREAM_IS_LOADED_UNTIL_A_SAVE_COMPILES);
                DevSessionStep::WaitForTheNextSave
            }
        }
    }

    /// The project's log, by its directory: every record of each of its streams and their loads.
    fn project_log_records_asked(&self) -> ProjectLogRecordsAsked<'_> {
        ProjectLogRecordsAsked {
            project_directory: Path::new(&self.stream_load_request.project_directory),
            stream: None,
            action: None,
        }
    }

    /// Page the project's log to its newest record, answering the cursor after it, so a session
    /// shows what its own runs write and none of what came before.
    async fn read_the_project_log_to_its_end(
        &mut self,
        connected_client: &LocalApiMcpToolClient,
    ) -> FinishedUnlessStopped<Result<String, LocalApiMcpToolClientFailure>> {
        let mut after: Option<String> = None;
        loop {
            let logs_arguments = self
                .project_log_records_asked()
                .logs_tool_arguments_after(after.as_deref());
            let logs_outcome = match self
                .finish_unless_stopped(connected_client.call_tool(LOGS_TOOL_NAME, logs_arguments))
                .await
            {
                FinishedUnlessStopped::Finished(logs_outcome) => logs_outcome,
                FinishedUnlessStopped::StopSignalDelivered => {
                    return FinishedUnlessStopped::StopSignalDelivered;
                }
            };
            let project_log_records_page = match logs_outcome.and_then(|logs_tool_result_text| {
                project_log_records_page_from(&logs_tool_result_text)
                    .map_err(LocalApiMcpToolClientFailure::request_refused_by_the_runtime)
            }) {
                Ok(project_log_records_page) => project_log_records_page,
                Err(logs_failure) => return FinishedUnlessStopped::Finished(Err(logs_failure)),
            };
            if project_log_records_page.records.is_empty() {
                return FinishedUnlessStopped::Finished(Ok(project_log_records_page.next_after));
            }
            after = Some(project_log_records_page.next_after);
        }
    }

    /// Follow the project's log page by page from the session's cursor while `followed` runs.
    async fn follow(
        &mut self,
        connected_client: &LocalApiMcpToolClient,
        mut followed: FollowedStreamPosition,
    ) -> DevSessionStep {
        if self.project_log_after.is_none() {
            self.note(&format!(
                "{} keeps running, unfollowed, since where the project's log ends is unknown — \
                 save again to load it",
                followed.stream
            ));
            return DevSessionStep::WaitForTheNextSave;
        }
        loop {
            let stream = followed.stream.clone();
            let logs_arguments = self
                .project_log_records_asked()
                .logs_tool_arguments_after(self.project_log_after.as_deref());
            let logs_outcome = match self
                .finish_unless_stopped(connected_client.call_tool(LOGS_TOOL_NAME, logs_arguments))
                .await
            {
                FinishedUnlessStopped::Finished(logs_outcome) => logs_outcome,
                FinishedUnlessStopped::StopSignalDelivered => return self.stop_following(&stream),
            };
            let project_log_records_page = match logs_outcome.and_then(|logs_tool_result_text| {
                project_log_records_page_from(&logs_tool_result_text)
                    .map_err(LocalApiMcpToolClientFailure::request_refused_by_the_runtime)
            }) {
                Ok(project_log_records_page) => project_log_records_page,
                Err(logs_failure) => {
                    return self.after_a_failed_follow(
                        &stream,
                        logs_failure,
                        followed.a_refused_reload_left_it_to_confirm,
                    );
                }
            };
            let rendered_records = render_project_log_records_page(
                &project_log_records_page,
                &RuntimeLogRecordFilters::default(),
                true,
                &mut std::io::stderr(),
            );
            if let Err(write_failure) = write_and_flush_verb_standard_output(&rendered_records) {
                return DevSessionStep::Exit(standard_output_closed_or_failed(write_failure));
            }
            if std::mem::take(&mut followed.a_refused_reload_left_it_to_confirm) {
                self.note(&format!(
                    "{stream} keeps running the last save that loaded — fix it and save again"
                ));
            }
            let page_brought_nothing = project_log_records_page.records.is_empty();
            self.project_log_after = Some(project_log_records_page.next_after);
            if self.project_sources_changed_during_a_call {
                return self.run_again_after_a_save(followed);
            }
            if !page_brought_nothing {
                continue;
            }
            match self
                .finish_unless_stopped(tokio::time::sleep(PROJECT_LOG_RECORDS_FOLLOW_POLL_INTERVAL))
                .await
            {
                FinishedUnlessStopped::Finished(()) => {}
                FinishedUnlessStopped::StopSignalDelivered => return self.stop_following(&stream),
            }
        }
    }

    /// Where the session goes after a `logs` call for `stream` failed: a stream the runtime no
    /// longer holds, or whose pages this CLI cannot read, waits for a save that runs it again.
    fn after_a_failed_follow(
        &mut self,
        stream: &str,
        logs_failure: LocalApiMcpToolClientFailure,
        a_refused_reload_left_it_to_confirm: bool,
    ) -> DevSessionStep {
        if let Some(after_the_runtime_went_away) =
            self.after_the_runtime_went_away(&logs_failure, Some(stream))
        {
            return after_the_runtime_went_away;
        }
        if logs_failure.kind == LocalApiMcpToolClientFailureKind::ToolCallFailed
            && a_refused_reload_left_it_to_confirm
        {
            self.note(NO_STREAM_IS_LOADED_UNTIL_A_SAVE_COMPILES);
            return DevSessionStep::WaitForTheNextSave;
        }
        if logs_failure.kind == LocalApiMcpToolClientFailureKind::ToolCallFailed {
            self.note(&format!(
                "{stream} is no longer loaded — stopped or removed elsewhere, or ended by its \
                 watchdog: {logs_failure}; save again to load it"
            ));
            return DevSessionStep::WaitForTheNextSave;
        }
        self.note(&format!(
            "{logs_failure}; {stream} keeps running, unfollowed — save again to load it"
        ));
        DevSessionStep::WaitForTheNextSave
    }

    /// After a settled save: run the stream again over the same connection, which the runtime
    /// answers by replacing `followed` once the save compiles.
    fn run_again_after_a_save(&self, followed: FollowedStreamPosition) -> DevSessionStep {
        self.note(&format!(
            "a saved change — loading {} again",
            followed.stream
        ));
        DevSessionStep::Run(Some(followed))
    }

    async fn wait_for_the_next_save(&mut self) -> DevSessionStep {
        if std::mem::take(&mut self.project_sources_changed_during_a_call) {
            return DevSessionStep::Run(None);
        }
        match self.next_event().await {
            Some(DevSessionEvent::ProjectSourcesChanged) => DevSessionStep::Run(None),
            Some(DevSessionEvent::StopSignalDelivered) | None => DevSessionStep::Exit(Ok(0)),
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
                .finish_unless_stopped(tokio::time::sleep(RUNTIME_SOCKET_AND_RE_LOAD_POLL_INTERVAL))
                .await
            {
                FinishedUnlessStopped::Finished(()) => {}
                FinishedUnlessStopped::StopSignalDelivered => return None,
            }
            if !something_answers_at(&self.local_api_socket_path) {
                continue;
            }
            match self.connect_unless_stopped().await {
                FinishedUnlessStopped::Finished(Ok(connected_client)) => {
                    return Some(connected_client);
                }
                FinishedUnlessStopped::Finished(Err(connect_failure)) => {
                    self.note(&format!("{connect_failure}; waiting"));
                }
                FinishedUnlessStopped::StopSignalDelivered => return None,
            }
        }
    }

    /// Ask the runtime until it has re-loaded `stream` from its record, then follow the project's
    /// log on from where the session read up to. A stream the runtime holds stopped, failed or not at all waits
    /// for a save, which runs it again.
    async fn wait_for_the_re_load_of(
        &mut self,
        connected_client: &LocalApiMcpToolClient,
        stream: String,
    ) -> DevSessionStep {
        self.note(&format!(
            "the runtime answers again — waiting for it to re-load {stream}"
        ));
        loop {
            let list_streams_outcome = match self
                .finish_unless_stopped(connected_client.call_tool_bounded_by(
                    StreamActionTool::ListStreams.tool_name(),
                    serde_json::Map::new(),
                    StreamActionTool::ListStreams.tool_call_timeout(),
                ))
                .await
            {
                FinishedUnlessStopped::Finished(list_streams_outcome) => list_streams_outcome,
                FinishedUnlessStopped::StopSignalDelivered => return self.stop_following(&stream),
            };
            let listed_streams = match list_streams_outcome.and_then(|list_streams_result_text| {
                tool_result_from::<ListStreamsToolResult>(
                    StreamActionTool::ListStreams.tool_name(),
                    &list_streams_result_text,
                )
                .map_err(LocalApiMcpToolClientFailure::request_refused_by_the_runtime)
            }) {
                Ok(list_streams_result) => list_streams_result.streams,
                Err(list_streams_failure) => {
                    if let Some(after_the_runtime_went_away) =
                        self.after_the_runtime_went_away(&list_streams_failure, Some(&stream))
                    {
                        return after_the_runtime_went_away;
                    }
                    self.note(&format!(
                        "{list_streams_failure}; save again to load {stream}"
                    ));
                    return DevSessionStep::WaitForTheNextSave;
                }
            };
            match listed_streams
                .iter()
                .find(|listed_stream| listed_stream.stream == stream)
                .map(|listed_stream| (listed_stream.state, listed_stream.node_count.is_some()))
            {
                Some((ListedStreamState::Running, true)) => {
                    self.note(&format!(
                        "{stream} is loaded again — following the project's log on"
                    ));
                    return DevSessionStep::Follow(FollowedStreamPosition::following(stream));
                }
                Some((ListedStreamState::Running, false)) => {}
                Some((not_running, _)) => {
                    self.note(&format!(
                        "{stream} came back {not_running}, so it is not followed; save again to \
                         load it"
                    ));
                    return DevSessionStep::WaitForTheNextSave;
                }
                None => {
                    self.note(&format!(
                        "the runtime no longer holds {stream}; save again to load it"
                    ));
                    return DevSessionStep::WaitForTheNextSave;
                }
            }
            if self.project_sources_changed_during_a_call {
                return self.run_again_after_a_save(FollowedStreamPosition::following(stream));
            }
            match self
                .finish_unless_stopped(tokio::time::sleep(RUNTIME_SOCKET_AND_RE_LOAD_POLL_INTERVAL))
                .await
            {
                FinishedUnlessStopped::Finished(()) => {}
                FinishedUnlessStopped::StopSignalDelivered => return self.stop_following(&stream),
            }
        }
    }
}

/// What `dev` says when a load was refused and nothing is loaded until a save compiles.
const NO_STREAM_IS_LOADED_UNTIL_A_SAVE_COMPILES: &str =
    "no stream is loaded — fix it and save again";
