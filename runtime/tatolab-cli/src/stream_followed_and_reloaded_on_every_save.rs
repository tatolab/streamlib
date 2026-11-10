// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab dev`: a start of the project queued on the machine's runtime over a `/mcp/stdio`
//! connection of this process's own — a reload when the runtime refuses the start because the
//! project already runs or is paused — the project's log followed by cursor from where it ended
//! when `dev` started — each record under the project's folder name, across every load and
//! restart, since the log is the project's — and a reload queued for every settled save, which
//! replaces each stream and keeps a paused project paused, or records it failed with its traceback
//! in the log; the runtime refuses the reload of a stopped project. A save of a
//! `.py` file first refreshes every fragment whose graph generator that module defines. Ctrl-C ends
//! the following and leaves the streams loaded. When the runtime goes away, `dev` waits for it
//! and follows the log again while the restarted runtime re-loads the project from its records;
//! it never queues a load for a crash.

use std::collections::BTreeSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use streamlib_runtime_client_contract::local_api_wire_contract::StartOrReloadProjectToolResult;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::TatolabCommandFailure;
use crate::local_api_connection::tokio_runtime_for_a_local_api_connection;
use crate::local_api_mcp_tool_client::{
    LocalApiMcpToolClient, LocalApiMcpToolClientFailure, ONE_SHOT_VERB_TOOL_CALL_TIMEOUT,
};
use crate::machine_runtime_local_api_socket::{
    local_api_socket_of_the_running_runtime, something_answers_at,
};
use crate::process_signal_handling::block_the_stop_signals_and_listen;
use crate::project_log_records_from_the_runtime::{
    DESCRIBE_RUNTIME_TOOL_NAME, LOGS_TOOL_NAME, PROJECT_LOG_RECORDS_FOLLOW_POLL_INTERVAL,
    ProjectLogRecordsAsked, project_log_records_page_from, render_project_log_records_page,
};
use crate::project_source_change_watcher::{ProjectSourceWatchHold, watch_project_sources};
use crate::runtime_gone_while_following_a_stream::{
    RuntimeLogDirectoryAsDescribed, the_runtime_is_gone,
};
use crate::runtime_log_record_decoding_and_filters::RuntimeLogRecordFilters;
use crate::stream_actions_on_the_runtime::{
    ProjectLoadRequest, StreamActionTool, rendered_queued_stream_action_line, tool_result_from,
};
use crate::verb_standard_output::{
    standard_output_closed_or_failed, write_and_flush_verb_standard_output,
    write_verb_standard_error,
};

/// How often `dev` looks for the runtime's socket again after losing its connection.
const RUNTIME_SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The tool that refreshes every fragment whose graph generator a changed module defines.
const REFRESH_FRAGMENTS_TOOL_NAME: &str = "refresh_fragments";

/// What `dev` prefixes each of its notes on standard error with.
const DEV_NOTE_PREFIX: &str = "tatolab dev";

/// What reaches `dev`'s session besides the runtime's answers.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DevSessionEvent {
    /// SIGINT, SIGTERM or SIGHUP.
    StopSignalDelivered,
    /// A save in the project settled, changing these files.
    ProjectSourcesChanged { changed_files: Vec<PathBuf> },
}

/// A call that finished, or the stop signal that came first.
enum FinishedUnlessStopped<CallOutcome> {
    Finished(CallOutcome),
    StopSignalDelivered,
}

/// Where the session goes next.
enum DevSessionStep {
    /// Queue a load of the project over the connection, one per load still owed.
    QueueALoad,
    /// Follow the project's log from the session's cursor.
    FollowTheProjectLog,
    /// The project's log cannot be followed: wait for the next save, which queues a load.
    WaitForTheNextSave,
    /// Without a connection: wait for the runtime's socket, then connect and follow the log again.
    WaitForTheRuntime,
    /// The session is over with this exit.
    Exit(Result<u8, TatolabCommandFailure>),
}

/// `tatolab dev [--dir DIR]`: queue a start of the project, or its reload when it already runs,
/// follow its log, and queue a reload for every settled save, until a stop signal ends the
/// following.
pub(crate) fn follow_the_project_log_and_load_the_project_on_every_save(
    caller_working_directory: &Path,
    requested_project_directory: Option<&Path>,
) -> Result<u8, TatolabCommandFailure> {
    let project_load_request = ProjectLoadRequest::of_the_project_a_verb_names(
        caller_working_directory,
        requested_project_directory,
    )?;
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
    let project_source_watch_hold = watch_project_sources(
        PathBuf::from(&project_load_request.project_directory),
        move |changed_files| {
            dev_session_event_sender
                .send(DevSessionEvent::ProjectSourcesChanged { changed_files })
                .is_ok()
        },
    )
    .map_err(|io_failure| {
        TatolabCommandFailure::refused(format!("cannot watch the project: {io_failure}"))
    })?;
    let session_tokio_runtime = tokio_runtime_for_a_local_api_connection()?;
    let session_outcome = session_tokio_runtime.block_on(
        DevSession {
            project_load_request,
            local_api_socket_path,
            dev_session_events,
            dev_session_events_open: true,
            loads_owed: 1,
            the_runtime_answered_a_load_of_this_session: false,
            python_files_changed_before_the_owed_load: BTreeSet::new(),
            project_source_watch_hold,
            runtime_log_directory: RuntimeLogDirectoryAsDescribed::NotDescribed,
            project_log_after: None,
            project_log_followed_from_its_start: false,
        }
        .run_until_it_ends(),
    );
    session_tokio_runtime.shutdown_background();
    session_outcome
}

/// One `dev` run: the project it loads, the runtime it talks to, and what it has seen.
struct DevSession {
    project_load_request: ProjectLoadRequest,
    local_api_socket_path: PathBuf,
    dev_session_events: UnboundedReceiver<DevSessionEvent>,
    dev_session_events_open: bool,
    /// Loads still to queue: the first, then one per settled save, never coalesced.
    loads_owed: usize,
    /// Whether the runtime answered one of the session's loads, after which each is a reload.
    the_runtime_answered_a_load_of_this_session: bool,
    /// The `.py` files saved since the last fragment refresh, refreshed before the next load.
    python_files_changed_before_the_owed_load: BTreeSet<PathBuf>,
    /// Quiets the watch while a refresh writes the project's graph documents.
    project_source_watch_hold: ProjectSourceWatchHold,
    /// Where the runtime said its own log is, named when it goes away.
    runtime_log_directory: RuntimeLogDirectoryAsDescribed,
    /// The cursor the project's log is read on from; `None` until the session read to its end.
    project_log_after: Option<String>,
    /// Whether the log is followed from its first record, because it could not be read to its end
    /// before the session's first load.
    project_log_followed_from_its_start: bool,
}

impl DevSession {
    async fn run_until_it_ends(mut self) -> Result<u8, TatolabCommandFailure> {
        let mut connected_client = match self.connect_unless_stopped().await {
            FinishedUnlessStopped::Finished(connect_outcome) => Some(connect_outcome?),
            FinishedUnlessStopped::StopSignalDelivered => {
                self.note("stopped before the runtime opened a connection; nothing was queued");
                return Ok(0);
            }
        };
        let mut next_step = DevSessionStep::QueueALoad;
        loop {
            next_step = match (next_step, &connected_client) {
                (DevSessionStep::Exit(session_outcome), _) => {
                    if let Some(connected_client) = connected_client.take() {
                        connected_client.close().await;
                    }
                    return session_outcome;
                }
                (DevSessionStep::WaitForTheRuntime, _) | (_, None) => {
                    connected_client = None;
                    match self.wait_for_the_runtime_and_connect().await {
                        Some(reconnected_client) => {
                            connected_client = Some(reconnected_client);
                            self.note("the runtime answers again — following the project's log on");
                            DevSessionStep::FollowTheProjectLog
                        }
                        None => self.stop_following(),
                    }
                }
                (DevSessionStep::WaitForTheNextSave, _) => self.wait_for_the_next_save().await,
                (DevSessionStep::QueueALoad, Some(connected_client)) => {
                    self.queue_a_load(connected_client).await
                }
                (DevSessionStep::FollowTheProjectLog, Some(connected_client)) => {
                    self.follow_the_project_log(connected_client).await
                }
            };
        }
    }

    /// A `/mcp/stdio` connection of the session's own, unless a stop signal comes first; once it
    /// opens, the runtime's description names where its log is.
    async fn connect_unless_stopped(
        &mut self,
    ) -> FinishedUnlessStopped<Result<LocalApiMcpToolClient, LocalApiMcpToolClientFailure>> {
        let local_api_socket_path = self.local_api_socket_path.clone();
        let connected_client = match self
            .finish_unless_stopped(LocalApiMcpToolClient::connect_over_an_mcp_stdio_upgrade(
                &local_api_socket_path,
                ONE_SHOT_VERB_TOOL_CALL_TIMEOUT,
            ))
            .await
        {
            FinishedUnlessStopped::Finished(Ok(connected_client)) => connected_client,
            FinishedUnlessStopped::Finished(Err(connect_failure)) => {
                return FinishedUnlessStopped::Finished(Err(connect_failure));
            }
            FinishedUnlessStopped::StopSignalDelivered => {
                return FinishedUnlessStopped::StopSignalDelivered;
            }
        };
        match self
            .finish_unless_stopped(
                connected_client.call_tool(DESCRIBE_RUNTIME_TOOL_NAME, serde_json::Map::new()),
            )
            .await
        {
            FinishedUnlessStopped::Finished(describe_runtime_outcome) => {
                self.runtime_log_directory =
                    RuntimeLogDirectoryAsDescribed::from_the_describe_runtime_answer(
                        describe_runtime_outcome,
                    );
                FinishedUnlessStopped::Finished(Ok(connected_client))
            }
            FinishedUnlessStopped::StopSignalDelivered => {
                connected_client.close().await;
                FinishedUnlessStopped::StopSignalDelivered
            }
        }
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

    /// `call` to its end, unless a stop signal comes first; a save meanwhile owes a load.
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
                    Some(DevSessionEvent::ProjectSourcesChanged { changed_files }) => {
                        self.a_save_settled(changed_files);
                    }
                    None => {}
                },
                call_outcome = &mut call => return FinishedUnlessStopped::Finished(call_outcome),
            }
        }
    }

    /// A settled save owes a load, and a refresh first of what its `.py` files define.
    fn a_save_settled(&mut self, changed_files: Vec<PathBuf>) {
        self.loads_owed += 1;
        self.python_files_changed_before_the_owed_load.extend(
            changed_files
                .into_iter()
                .filter(|changed_file| changed_file.extension().is_some_and(|ext| ext == "py")),
        );
    }

    /// Refresh every fragment whose graph generator a saved `.py` file defines, the watch quiet
    /// while the refresh writes the graph documents, so its own writes owe no load. A refusal
    /// is noted and the load goes on.
    async fn refresh_the_fragments_the_saved_modules_define(
        &mut self,
        connected_client: &LocalApiMcpToolClient,
    ) -> Option<DevSessionStep> {
        if self.python_files_changed_before_the_owed_load.is_empty() {
            return None;
        }
        let mut refresh_arguments = serde_json::Map::new();
        refresh_arguments.insert(
            "project_directory".to_owned(),
            self.project_load_request.project_directory.clone().into(),
        );
        refresh_arguments.insert(
            "changed_files".to_owned(),
            self.python_files_changed_before_the_owed_load
                .iter()
                .map(|changed_file| changed_file.display().to_string().into())
                .collect::<Vec<serde_json::Value>>()
                .into(),
        );
        self.project_source_watch_hold.hold_back_the_reports();
        let refresh_outcome = self
            .finish_unless_stopped(
                connected_client.call_tool(REFRESH_FRAGMENTS_TOOL_NAME, refresh_arguments),
            )
            .await;
        self.project_source_watch_hold
            .release_taking_the_graph_documents_as_seen();
        let refresh_outcome = match refresh_outcome {
            FinishedUnlessStopped::Finished(refresh_outcome) => refresh_outcome,
            FinishedUnlessStopped::StopSignalDelivered => {
                self.note("stopped before the runtime answered the fragments' refresh");
                return Some(DevSessionStep::Exit(Ok(0)));
            }
        };
        if let Err(refresh_failure) = &refresh_outcome
            && let Some(after_the_runtime_went_away) =
                self.after_the_runtime_went_away(refresh_failure)
        {
            return Some(after_the_runtime_went_away);
        }
        self.python_files_changed_before_the_owed_load.clear();
        match refresh_outcome {
            Ok(refresh_result_text) => {
                let fragments_refreshed =
                    serde_json::from_str::<serde_json::Value>(&refresh_result_text)
                        .ok()
                        .and_then(|refresh_result| {
                            refresh_result["operations"].as_array().map(Vec::len)
                        })
                        .unwrap_or(0);
                if fragments_refreshed > 0 {
                    self.note(&format!(
                        "refreshed {fragments_refreshed} fragment(s) whose graph generator changed"
                    ));
                }
            }
            Err(refresh_failure) => self.note(&format!(
                "{refresh_failure}; the fragments keep their last run, and the load goes on"
            )),
        }
        None
    }

    /// The exit a stop signal earns while the log is followed: the project's streams run on.
    fn stop_following(&self) -> DevSessionStep {
        self.note(
            "stopped following; the project's streams keep running — `tatolab stop` stops one, \
             `tatolab rm` removes the project",
        );
        DevSessionStep::Exit(Ok(0))
    }

    /// Queue one owed load of the project, then follow its log; the log is first read to its end
    /// when the session has no cursor yet, so it shows what its own loads write and none of what
    /// came before. The load stays owed until the runtime answers it, so a runtime gone before its
    /// answer is asked again once it is back.
    async fn queue_a_load(&mut self, connected_client: &LocalApiMcpToolClient) -> DevSessionStep {
        if self.project_log_after.is_none() {
            match self.read_the_project_log_to_its_end(connected_client).await {
                FinishedUnlessStopped::Finished(Ok(end_of_the_project_log)) => {
                    self.project_log_after = Some(end_of_the_project_log);
                }
                FinishedUnlessStopped::Finished(Err(logs_failure)) => {
                    if let Some(after_the_runtime_went_away) =
                        self.after_the_runtime_went_away(&logs_failure)
                    {
                        return after_the_runtime_went_away;
                    }
                    self.note(&format!(
                        "the project's log could not be read to its end: {logs_failure}; it is \
                         followed from its start"
                    ));
                    self.project_log_followed_from_its_start = true;
                }
                FinishedUnlessStopped::StopSignalDelivered => return DevSessionStep::Exit(Ok(0)),
            }
        }
        if let Some(after_the_refresh) = self
            .refresh_the_fragments_the_saved_modules_define(connected_client)
            .await
        {
            return after_the_refresh;
        }
        let start_or_reload_outcome = match self.queue_a_start_or_reload(connected_client).await {
            FinishedUnlessStopped::Finished(start_or_reload_outcome) => start_or_reload_outcome,
            FinishedUnlessStopped::StopSignalDelivered => {
                self.note(
                    "stopped before the runtime answered the load; `tatolab streams` lists what \
                     it holds",
                );
                return DevSessionStep::Exit(Ok(0));
            }
        };
        let (start_or_reload_tool, start_or_reload_outcome) = start_or_reload_outcome;
        if let Err(start_or_reload_failure) = &start_or_reload_outcome
            && let Some(after_the_runtime_went_away) =
                self.after_the_runtime_went_away(start_or_reload_failure)
        {
            return after_the_runtime_went_away;
        }
        self.loads_owed = self.loads_owed.saturating_sub(1);
        self.the_runtime_answered_a_load_of_this_session = true;
        match start_or_reload_outcome.and_then(|start_or_reload_result_text| {
            tool_result_from::<StartOrReloadProjectToolResult>(
                start_or_reload_tool.tool_name(),
                &start_or_reload_result_text,
            )
            .map_err(LocalApiMcpToolClientFailure::request_refused_by_the_runtime)
        }) {
            Ok(start_or_reload_result) => self.note(
                rendered_queued_stream_action_line(
                    &start_or_reload_result.action,
                    "load",
                    &start_or_reload_result
                        .project_directory
                        .display()
                        .to_string(),
                )
                .trim_end(),
            ),
            Err(start_or_reload_failure) => {
                self.note(&format!(
                    "{start_or_reload_failure}; save again to queue another load"
                ));
            }
        }
        DevSessionStep::FollowTheProjectLog
    }

    /// The session's load and the tool that answered it: `reload_project` once the runtime
    /// answered one of the session's loads; before that `start_project`, then `reload_project`
    /// when the runtime refuses the start, as it does a project that already runs or is paused.
    async fn queue_a_start_or_reload(
        &mut self,
        connected_client: &LocalApiMcpToolClient,
    ) -> FinishedUnlessStopped<(
        StreamActionTool,
        Result<String, LocalApiMcpToolClientFailure>,
    )> {
        if !self.the_runtime_answered_a_load_of_this_session {
            match self
                .finish_unless_stopped(
                    connected_client.call_tool(
                        StreamActionTool::StartProject.tool_name(),
                        self.project_load_request
                            .start_or_reload_project_tool_arguments(),
                    ),
                )
                .await
            {
                FinishedUnlessStopped::Finished(Err(start_failure))
                    if !the_runtime_is_gone(&start_failure) => {}
                FinishedUnlessStopped::Finished(start_outcome) => {
                    return FinishedUnlessStopped::Finished((
                        StreamActionTool::StartProject,
                        start_outcome,
                    ));
                }
                FinishedUnlessStopped::StopSignalDelivered => {
                    return FinishedUnlessStopped::StopSignalDelivered;
                }
            }
        }
        match self
            .finish_unless_stopped(
                connected_client.call_tool(
                    StreamActionTool::ReloadProject.tool_name(),
                    self.project_load_request
                        .start_or_reload_project_tool_arguments(),
                ),
            )
            .await
        {
            FinishedUnlessStopped::Finished(reload_outcome) => {
                FinishedUnlessStopped::Finished((StreamActionTool::ReloadProject, reload_outcome))
            }
            FinishedUnlessStopped::StopSignalDelivered => {
                FinishedUnlessStopped::StopSignalDelivered
            }
        }
    }

    /// Where the session goes after `failure` reached it: when the runtime is gone, a note naming
    /// its log, then a wait for it. `None` when the runtime is still there.
    fn after_the_runtime_went_away(
        &self,
        failure: &LocalApiMcpToolClientFailure,
    ) -> Option<DevSessionStep> {
        if !the_runtime_is_gone(failure) {
            return None;
        }
        self.note(&self.runtime_log_directory.the_runtime_went_away_message());
        Some(DevSessionStep::WaitForTheRuntime)
    }

    /// The project's log, by its directory: every record of each of its streams and their loads.
    fn project_log_records_asked(&self) -> ProjectLogRecordsAsked<'_> {
        ProjectLogRecordsAsked {
            project_directory: Path::new(&self.project_load_request.project_directory),
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

    /// Follow the project's log page by page from the session's cursor, queuing each load a save
    /// owes as it comes.
    async fn follow_the_project_log(
        &mut self,
        connected_client: &LocalApiMcpToolClient,
    ) -> DevSessionStep {
        if self.project_log_after.is_none() && !self.project_log_followed_from_its_start {
            match self.read_the_project_log_to_its_end(connected_client).await {
                FinishedUnlessStopped::Finished(Ok(end_of_the_project_log)) => {
                    self.project_log_after = Some(end_of_the_project_log);
                }
                FinishedUnlessStopped::Finished(Err(logs_failure)) => {
                    return self.after_a_failed_follow(logs_failure);
                }
                FinishedUnlessStopped::StopSignalDelivered => return self.stop_following(),
            }
        }
        loop {
            if self.loads_owed > 0 {
                self.note("a saved change — queuing another load of the project");
                return DevSessionStep::QueueALoad;
            }
            let logs_arguments = self
                .project_log_records_asked()
                .logs_tool_arguments_after(self.project_log_after.as_deref());
            let logs_outcome = match self
                .finish_unless_stopped(connected_client.call_tool(LOGS_TOOL_NAME, logs_arguments))
                .await
            {
                FinishedUnlessStopped::Finished(logs_outcome) => logs_outcome,
                FinishedUnlessStopped::StopSignalDelivered => return self.stop_following(),
            };
            let project_log_records_page = match logs_outcome.and_then(|logs_tool_result_text| {
                project_log_records_page_from(&logs_tool_result_text)
                    .map_err(LocalApiMcpToolClientFailure::request_refused_by_the_runtime)
            }) {
                Ok(project_log_records_page) => project_log_records_page,
                Err(logs_failure) => return self.after_a_failed_follow(logs_failure),
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
            let page_brought_nothing = project_log_records_page.records.is_empty();
            self.project_log_after = Some(project_log_records_page.next_after);
            if !page_brought_nothing {
                continue;
            }
            match self
                .finish_unless_stopped(tokio::time::sleep(PROJECT_LOG_RECORDS_FOLLOW_POLL_INTERVAL))
                .await
            {
                FinishedUnlessStopped::Finished(()) => {}
                FinishedUnlessStopped::StopSignalDelivered => return self.stop_following(),
            }
        }
    }

    /// Where the session goes after a `logs` call failed: a wait for the runtime when it is gone,
    /// else a wait for the save that queues another load and follows the log again.
    fn after_a_failed_follow(
        &mut self,
        logs_failure: LocalApiMcpToolClientFailure,
    ) -> DevSessionStep {
        if let Some(after_the_runtime_went_away) = self.after_the_runtime_went_away(&logs_failure) {
            return after_the_runtime_went_away;
        }
        self.note(&format!(
            "the project's log cannot be followed: {logs_failure}; the streams keep running — save \
             again to queue another load and follow it"
        ));
        DevSessionStep::WaitForTheNextSave
    }

    async fn wait_for_the_next_save(&mut self) -> DevSessionStep {
        if self.loads_owed > 0 {
            return DevSessionStep::QueueALoad;
        }
        match self.next_event().await {
            Some(DevSessionEvent::ProjectSourcesChanged { changed_files }) => {
                self.a_save_settled(changed_files);
                DevSessionStep::QueueALoad
            }
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
                .finish_unless_stopped(tokio::time::sleep(RUNTIME_SOCKET_POLL_INTERVAL))
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
}
