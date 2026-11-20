// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_runtime_client_contract::local_api_wire_contract::{
    CancelStreamActionToolResult, DescribeStreamActionToolResult, PausePlaybackToolResult,
    ProjectStreamActionQueuedToolResult, ResumePlaybackToolResult, SetStreamClockScaleToolResult,
};

use super::EVERY_SURFACE;
use super::worked_examples;
use crate::control_operation::{
    ControlOperation, ControlOperationAgentToolListing, ControlOperationEffect,
    ControlOperationExample, ControlOperationExposure, ControlOperationGroup,
    ControlOperationStability,
};
use crate::control_operation_requests::{
    OneProjectByItsDirectoryRequest, OneStreamActionRequest, SetStreamClockScaleRequest,
};

/// `start_playback`: queue the start of an open project's playback.
pub struct StartPlayback;

impl ControlOperation for StartPlayback {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = ProjectStreamActionQueuedToolResult;
    const METHOD_NAME: &'static str = "start_playback";
    const ONE_LINE_DOC: &'static str = "Queue the start of an open project's playback.";
    const AGENT_USAGE_NOTE: &'static str = "Start the playback of the open project in `project_directory`: queue it on the project's stream action queue and answer at once with the action's id as `action`. Once the project's earlier actions have run, each stream the project holds stopped starts fresh from the graph it holds, edits made while stopped included, reading no graph document and making no `.venv`: every node instance is made anew, so only the state a node persists itself carries over from an earlier playback. Each stream is recorded running, so the runtime plays the project again across its restarts until `stop_playback`. A project plays or stops as a whole: no tool plays or stops one of its streams. Follow it with `describe_stream_action`. Refused, with nothing queued: a `project_directory` that is not absolute; a project that is not open, which `open_project` opens; a paused project, which `resume_playback` resumes; and a project that plays once its queued actions run, which `stop_playback` stops.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::START_PLAYBACK);
}

/// `stop_playback`: queue the stop of a project's playback, its streams staying loaded.
pub struct StopPlayback;

impl ControlOperation for StopPlayback {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = ProjectStreamActionQueuedToolResult;
    const METHOD_NAME: &'static str = "stop_playback";
    const ONE_LINE_DOC: &'static str =
        "Queue the stop of a project's playback, its streams staying loaded.";
    const AGENT_USAGE_NOTE: &'static str = "Stop the playback of the whole project in `project_directory`: queue the stop on its stream action queue and answer at once with the action's id as `action` beside the `project_directory`. Once the project's earlier actions have run, it ends the playback of every loaded stream of the project — every node instance, device claim, function-node state and the clock torn down — keeps each stream's graph loaded, so `graph`, every graph edit and the node catalog still answer for it, and records each stopped, so the runtime holds the project open and stopped across its restarts until `start_playback` plays it again from the graph it holds. Its streams are `stopping` in `list_streams` until it ends; follow it with `describe_stream_action`. Each recorded stream carries the stop on its record until it has run, so a runtime crash finishes it at the next start. Refused, with nothing queued: a `project_directory` that is not absolute, a project whose removal is queued, and a project none of whose streams plays once the actions queued before it run.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::STOP_PLAYBACK);
}

/// `pause_playback`: pause a playing project's playback at once.
pub struct PausePlayback;

impl ControlOperation for PausePlayback {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = PausePlaybackToolResult;
    const METHOD_NAME: &'static str = "pause_playback";
    const ONE_LINE_DOC: &'static str = "Pause a playing project's playback at once.";
    const AGENT_USAGE_NOTE: &'static str = "Pause the playback of the whole project in `project_directory` at once, never waiting behind an action on its queue, and answer the `project_directory` and, as `streams`, the id of each loaded stream this call paused. Every node of each loaded stream holds its state and processes nothing, and a scaled or stepped stream clock holds where it stands, opening no step, while a real-time clock is the machine's own and runs on; the streams stay loaded and are `paused` in `list_streams`, and a graph edit made meanwhile is applied and compiles on `resume_playback`. The project stays paused until `resume_playback`, `stop_playback`, `close_project` or `remove_project`: a stream a `reload_project` loads meanwhile, the one `tatolab dev` asks for on a save among them, starts paused. Pause is recorded, so a runtime restart re-loads the project paused. Pausing a paused project pauses nothing more. Refused: a `project_directory` that is not absolute, and a project none of whose streams plays — none loaded, or each held stopped, which `start_playback` plays — and whose last action queued is not a `start_playback`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::PAUSE_PLAYBACK);
}

/// `resume_playback`: resume a paused project's playback at once.
pub struct ResumePlayback;

impl ControlOperation for ResumePlayback {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = ResumePlaybackToolResult;
    const METHOD_NAME: &'static str = "resume_playback";
    const ONE_LINE_DOC: &'static str = "Resume a paused project's playback at once.";
    const AGENT_USAGE_NOTE: &'static str = "Resume the paused playback of the project in `project_directory` at once and answer the `project_directory` and, as `streams`, the id of each stream this call resumed: every node carries on from the state it held, each held stream clock carries on from the stream time it was held at, and each stream compiles the graph edits made while it was paused, a compile that fails logged in the project's `logs` as a live edit's is. Refused: a `project_directory` that is not absolute, and a project that is not paused.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::RESUME_PLAYBACK);
}

/// `describe_stream_action`: describe one stream action by its id: its state, its step and each stream it reached.
pub struct DescribeStreamAction;

impl ControlOperation for DescribeStreamAction {
    type Request = OneStreamActionRequest;
    type Result = DescribeStreamActionToolResult;
    const METHOD_NAME: &'static str = "describe_stream_action";
    const ONE_LINE_DOC: &'static str =
        "Describe one stream action by its id: its state, its step and each stream it reached.";
    const AGENT_USAGE_NOTE: &'static str = "Describe the stream action `action`: its project, `kind` (`open`, `load`, `start_playback`, `stop`, `close`, `remove`, `delete`, `reload_at_start` or `make_venv_current`), `state` — `queued` behind the project's earlier actions, `in_progress` on its `step` (`reading_the_graph_documents`, `making_the_venv`, `starting`), `done`, `failed` with `failed_because`, or `cancelled` by `cancel_stream_action` — its `venv_sync`, what its latest uv sync of the project's `.venv` has done, read from uv's own lines from the moment uv starts and kept once it ends, `null` for an action that has not synced: its `phase` (`resolving`, `downloading`, `building`, `installing`, `installed`), `packages_resolved`, `packages_prepared` and `packages_installed` as uv counts them, `downloads_started` and `downloads_done` of the downloads uv names — only those over about a MiB — and under `packages` each package uv named, first named first, by `name`, `version` once uv names it, `kind` (`distribution` or `python_interpreter`), `state` (`downloading`, `downloaded`, `building`, `built`, `installed`, `uninstalled`) and `download_size_bytes` as uv states it; uv names no byte of a download as it arrives — and `streams`, each stream its project's graph documents named by `stream` id and `display_name`, with its `state` `running`, `stopped` or `failed` once the action took it in. Followed live as the resource `streamlib://stream-action/<action>`, which a `subscriptions/listen` naming it is sent `notifications/resources/updated` for each time the action changes. The runtime holds every queued and running action and the last 64 finished ones of each project; an older one is refused, its records still in the project's `logs`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Stream;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::DESCRIBE_STREAM_ACTION);
}

/// `cancel_stream_action`: cancel one stream action that is still queued or under way.
pub struct CancelStreamAction;

impl ControlOperation for CancelStreamAction {
    type Request = OneStreamActionRequest;
    type Result = CancelStreamActionToolResult;
    const METHOD_NAME: &'static str = "cancel_stream_action";
    const ONE_LINE_DOC: &'static str =
        "Cancel one stream action that is still queued or under way.";
    const AGENT_USAGE_NOTE: &'static str = "Cancel the stream action `action`, the only way an action ends early; nothing replaces or edits a queued one. A queued action is dropped from its project's queue, its pending mark taken off each stream's record so the record stays as it was, and answers `cancelled`. A running `open`, `load`, `reload_at_start` or `make_venv_current` answers `in_progress` and stops at the step it is on — a running uv sync is killed and a half-made `.venv` removed — then ends `cancelled`: a stream it was loading is recorded failed, and the streams it had not reached stay as recorded. Refused for a running `stop`, `close` or `remove`, whose unload has begun and cannot be cut short, which ends `done`; for an action already finished; and for one the runtime does not hold.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Stream;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::CANCEL_STREAM_ACTION);
}

/// `set_stream_clock_scale`: run a started stream's scaled clock at another scale from now on.
pub struct SetStreamClockScale;

impl ControlOperation for SetStreamClockScale {
    type Request = SetStreamClockScaleRequest;
    type Result = SetStreamClockScaleToolResult;
    const METHOD_NAME: &'static str = "set_stream_clock_scale";
    const ONE_LINE_DOC: &'static str =
        "Run a started stream's scaled clock at another scale from now on.";
    const AGENT_USAGE_NOTE: &'static str = "Run a started stream's scaled clock at `scale` from now on: its stream time carries on from where it stands, never jumping or running backwards, and every node's `ctx.time`, write stamps and continuous pace follow the new scale. Above 1 the clock runs in steps of its `step_ns`, paced at the scale, behind the step barrier every node runs on; back at or under 1 it runs free again. Refused for a stream on the real or a stepped clock, and for a scale above 1 while the stream holds a node that paces itself (execution = manual) and is not paced by real time. Live only: the stream's next start runs the clock its graph document names.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Stream;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::SET_STREAM_CLOCK_SCALE);
}
