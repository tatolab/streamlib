// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_runtime_client_contract::local_api_wire_contract::{
    CreateProjectToolResult, DeleteProjectToolResult, DuplicateProjectToolResult,
    ImportProjectToolResult, ListSamplesToolResult, ProjectSettingsTransactionToolResult,
    ProjectStreamActionQueuedToolResult, RemoveProjectToolResult,
};

use super::EVERY_SURFACE;
use super::worked_examples;
use crate::control_operation::{
    ControlOperation, ControlOperationAgentToolListing, ControlOperationEffect,
    ControlOperationExample, ControlOperationExposure, ControlOperationGroup,
    ControlOperationStability,
};
use crate::control_operation_requests::{
    CreateProjectRequest, DuplicateProjectRequest, ImportProjectRequest, ListSamplesRequest,
    OneProjectByItsDirectoryRequest, RenameProjectRequest, SetProjectCoverRequest,
    SetProjectDescriptionRequest,
};

/// `open_project`: queue the open of a project: its streams are loaded and held with their playback stopped.
pub struct OpenProject;

impl ControlOperation for OpenProject {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = ProjectStreamActionQueuedToolResult;
    const METHOD_NAME: &'static str = "open_project";
    const ONE_LINE_DOC: &'static str =
        "Queue the open of a project: its streams are loaded and held with their playback stopped.";
    const AGENT_USAGE_NOTE: &'static str = "Open the project in `project_directory`: queue its open on its own stream action queue and answer at once with the action's id as `action`. The open reads its `tatolab-project.json` and each stream asset's graph document, makes the project's `.venv` with the uv the runtime carries when its `.venv/bin/python` does not run — however long uv takes — then loads each stream under its stream id, `<project>-<stream>-<hash>`, the hash over the project's UUID and the stream asset's UUID, and holds it with its playback stopped, recording each stream stopped or failed: a graph document that does not load, or a load that is refused, with its reason, and a `.venv` uv cannot make — uv's own words — failing every stream of the project. It starts no node. Once it is done, `graph`, every graph edit and the node catalog answer for each stream, and `start_playback` plays the project. The project stays open, held stopped or playing, across the runtime's restarts until `close_project`. It opens a project that was never opened, one `close_project` closed, and one whose every stream failed, retrying them. One project's actions run one at a time, in the order they were queued; different projects' run side by side. Follow the action with `describe_stream_action`, its streams with `list_streams` — `preparing`, with the action and its `step`, until it ends — and its records with `logs` given `action`. A directory that does not exist or holds no `tatolab-project.json` fails the action, never the call. Refused, with nothing queued: a `project_directory` that is not absolute, and a project open once its queued actions run, which `reload_project` refreshes.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::OPEN_PROJECT);
}

/// `reload_project`: queue a soft reload of an open project from disk, keeping its playback.
pub struct ReloadProject;

impl ControlOperation for ReloadProject {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = ProjectStreamActionQueuedToolResult;
    const METHOD_NAME: &'static str = "reload_project";
    const ONE_LINE_DOC: &'static str =
        "Queue a soft reload of an open project from disk, keeping its playback.";
    const AGENT_USAGE_NOTE: &'static str = "Softly reload the open project in `project_directory` from disk: queue the load on its stream action queue and answer at once with the action's id as `action`. It re-reads every stream asset's graph document. While the project's node code — its `.py` files, `pyproject.toml`, `uv.lock` and views' `tatolab_view.json` — is as it was at a stream's load, the stream stays loaded and its graph is brought to its document in place, each node matched by its UUID, so only the nodes and links that changed change; otherwise the stream is loaded again under its same id, keeping its display name. A stream whose graph document is gone is unloaded and forgotten, a failed stream is loaded again, and the project's `.venv` is synced only when its dependency files changed or its `.venv/bin/python` does not run. The project keeps its playback: a playing project's streams start playing, a paused one's start paused, and a stopped one's are held stopped. `tatolab dev` asks for it on each save. A hard reload is `close_project` then `open_project`. Follow it as `open_project`'s open. Refused, with nothing queued: a `project_directory` that is not absolute, and a project that is not open, which `open_project` opens.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::RELOAD_PROJECT);
}

/// `close_project`: queue the close of an open project: its streams are unloaded and stay closed until it is opened.
pub struct CloseProject;

impl ControlOperation for CloseProject {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = ProjectStreamActionQueuedToolResult;
    const METHOD_NAME: &'static str = "close_project";
    const ONE_LINE_DOC: &'static str = "Queue the close of an open project: its streams are unloaded and stay closed until it is opened.";
    const AGENT_USAGE_NOTE: &'static str = "Close the open project in `project_directory`: queue the close on its stream action queue and answer at once with the action's id as `action`. Once the project's earlier actions have run, it ends the playback of every stream of the project, unloads each, forgets its function-node state, and records each closed, so the runtime leaves the project unloaded across its restarts until `open_project` opens it again. It deletes nothing: the project's files, its `.venv` and what uv synced into it stay. Its streams are `stopping` in `list_streams` until it ends, then `closed`; follow it with `describe_stream_action`. Each recorded stream carries the close on its record until it has run, so a runtime crash finishes it at the next start. Refused, with nothing queued: a `project_directory` that is not absolute, and a project that is not open once its queued actions run.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::CLOSE_PROJECT);
}

/// `remove_project`: queue the removal of a project from the runtime: its streams are stopped and their records forgotten.
pub struct RemoveProject;

impl ControlOperation for RemoveProject {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = RemoveProjectToolResult;
    const METHOD_NAME: &'static str = "remove_project";
    const ONE_LINE_DOC: &'static str = "Queue the removal of a project from the runtime: its streams are stopped and their records forgotten.";
    const AGENT_USAGE_NOTE: &'static str = "Queue the removal of the whole project in `project_directory` on its stream action queue and answer at once with the action's id as `action`: once the project's earlier actions have run, it unloads every loaded stream of the project and forgets every record of it, so the runtime no longer re-loads any of them, and deletes no file. `delete_asset` on a stream asset unloads and forgets that one stream. The project's streams are `stopping` in `list_streams` until it ends; follow it with `describe_stream_action`. Refused, with nothing queued: a `project_directory` that is not absolute, and a project the runtime holds no stream of.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::REMOVE_PROJECT);
}

/// `rename_project`: rename a project, as one transaction on its history.
pub struct RenameProject;

impl ControlOperation for RenameProject {
    type Request = RenameProjectRequest;
    type Result = ProjectSettingsTransactionToolResult;
    const METHOD_NAME: &'static str = "rename_project";
    const ONE_LINE_DOC: &'static str = "Rename a project, as one transaction on its history.";
    const AGENT_USAGE_NOTE: &'static str = "Rename a project, as one transaction by `author` on its history that undoes like any edit: its `name` in `tatolab-project.json` changes, and its UUID, its folder and its streams' ids stay as they are. Answers how people see the project now, with the `transaction` and `commit`. Refused for a blank name.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::RENAME_PROJECT);
}

/// `set_project_description`: describe a project, or clear its description, as one transaction on its history.
pub struct SetProjectDescription;

impl ControlOperation for SetProjectDescription {
    type Request = SetProjectDescriptionRequest;
    type Result = ProjectSettingsTransactionToolResult;
    const METHOD_NAME: &'static str = "set_project_description";
    const ONE_LINE_DOC: &'static str =
        "Describe a project, or clear its description, as one transaction on its history.";
    const AGENT_USAGE_NOTE: &'static str = "Describe a project, or clear its description, as one transaction by `author` on its history that undoes like any edit: its `description` in `tatolab-project.json`, served by `list_assets` as `project_description`. Answers how people see the project now, with the `transaction` and `commit`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::SET_PROJECT_DESCRIPTION);
}

/// `set_project_cover`: give a project a cover, or take it away, as one transaction on its history.
pub struct SetProjectCover;

impl ControlOperation for SetProjectCover {
    type Request = SetProjectCoverRequest;
    type Result = ProjectSettingsTransactionToolResult;
    const METHOD_NAME: &'static str = "set_project_cover";
    const ONE_LINE_DOC: &'static str =
        "Give a project a cover, or take it away, as one transaction on its history.";
    const AGENT_USAGE_NOTE: &'static str = "Give a project a cover, or take it away, as one transaction by `author` on its history that undoes like any edit: its `cover` in `tatolab-project.json` names a media asset the project holds by its UUID, served by `list_assets` as `project_cover`. An image a person picks, or a frame a viewer showed, is first imported with `import_asset` and then named here; the asset cannot be deleted while it is the cover. Answers how people see the project now, with the `transaction` and `commit`. Refused for an asset the project does not hold, or one that is not a media asset.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::SET_PROJECT_COVER);
}

/// `duplicate_project`: duplicate a project into a folder that does not exist yet, as a project of its own.
pub struct DuplicateProject;

impl ControlOperation for DuplicateProject {
    type Request = DuplicateProjectRequest;
    type Result = DuplicateProjectToolResult;
    const METHOD_NAME: &'static str = "duplicate_project";
    const ONE_LINE_DOC: &'static str =
        "Duplicate a project into a folder that does not exist yet, as a project of its own.";
    const AGENT_USAGE_NOTE: &'static str = "Duplicate a project into `into`, a folder that does not exist yet, as a project of its own: every file a copy of it carries, its `imports/` included, and its whole history, so its edits can still be undone in the copy, under a new project UUID and the name `<name> copy`, the duplicate recorded as a commit on its history by `author`. Its `.venv` and the rest of its `.tatolab/` stay behind, and the duplicate is not opened: `open_project` opens it. Answers the duplicate's `project_directory`, `project`, `project_name` and `commit`. Refused, nothing left at `into`, for a relative `into`, one that exists, and one inside the project.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::DUPLICATE_PROJECT);
}

/// `delete_project`: queue the deletion of a whole project.
pub struct DeleteProject;

impl ControlOperation for DeleteProject {
    type Request = OneProjectByItsDirectoryRequest;
    type Result = DeleteProjectToolResult;
    const METHOD_NAME: &'static str = "delete_project";
    const ONE_LINE_DOC: &'static str = "Queue the deletion of a whole project.";
    const AGENT_USAGE_NOTE: &'static str = "Delete the whole project in `project_directory`: queue its deletion on its stream action queue and answer at once with the action's id as `action` beside the `project_directory`. Once the project's earlier actions have run, it unloads every loaded stream of the project and forgets every record of it, as `remove_project` does, then moves the project's folder to this machine's Trash, from which its owner can put it back. Its streams are `stopping` in `list_streams` until it ends; follow it with `describe_stream_action`, which answers `failed` with why when the folder could not be moved. Refused, with nothing queued: a `project_directory` that is not absolute, and a folder holding no `tatolab-project.json`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::DELETE_PROJECT);
}

/// `import_project`: import a project from an archive into a folder that does not exist yet.
pub struct ImportProject;

impl ControlOperation for ImportProject {
    type Request = ImportProjectRequest;
    type Result = ImportProjectToolResult;
    const METHOD_NAME: &'static str = "import_project";
    const ONE_LINE_DOC: &'static str =
        "Import a project from an archive into a folder that does not exist yet.";
    const AGENT_USAGE_NOTE: &'static str = "Make a new project in `project_directory` from the `.zip` of a project at `source`, as a Marketplace entry or a GitHub archive is handed over. Its files are unpacked as `add_dependency` copies a zip — every file its history would record and every asset, its `imports/` included, never a hidden folder such as its `.tatolab/`, a cache, a Python virtual environment or a symbolic link — under a new project UUID of its own, so the same zip imported twice makes two projects, and committed by `author` as the first commit of a history of its own. Nothing reaches the network and nothing runs: open it with `open_project`, and `start_playback` plays it. Answers the new `project_directory`, its `project` UUID, its `name` and the `commit`. Refused, nothing left behind, for a relative path, a `source` that is a folder or no zip archive, a zip holding no `tatolab-project.json` or no project UUID in it, naming what is missing, or a `project_directory` that already exists.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::IMPORT_PROJECT);
}

/// `create_project`: queue the creation of a project from the starter or a sample.
pub struct CreateProject;

impl ControlOperation for CreateProject {
    type Request = CreateProjectRequest;
    type Result = CreateProjectToolResult;
    const METHOD_NAME: &'static str = "create_project";
    const ONE_LINE_DOC: &'static str =
        "Queue the creation of a project from the starter or a sample.";
    const AGENT_USAGE_NOTE: &'static str = "Create a project — the one `tatolab new` creates: the starter, its `tatolab-project.json`, one stream asset `streams/main.graph.json` holding the test pattern's one source, `pyproject.toml`, `.python-version` and `.gitignore`, or, given a `sample` that `list_samples` lists, a copy of that sample project under a new project UUID — committed as the first commit of the project's history, in `project_directory`, or with none in a folder `<adjective>-<noun>` drawn under `~/streams`, and queue its open, answering at once with the action's id as `action`, the stream id its `main` runs under as `stream`, and the resolved `project_directory`. The action writes the project's files, then opens it as `open_project` does — reads its graph documents, makes its `.venv`, however long uv takes, and holds its stream stopped until `start_playback` — the project named after its folder and its `main` stream shown under the folder's name. A directory already holding a file the create writes fails the action with nothing written. Follow it with `describe_stream_action` and `logs`; the project is recorded and re-loaded whenever the runtime starts, so a later session finds it in `list_streams`. Refused only for a `sample` the runtime unit does not carry, naming the ones it does, a relative `project_directory`, one that cannot be created, or, with none, a `HOME` that is unset or not absolute or a draw that finds no free folder.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::CREATE_PROJECT);
}

/// `list_samples`: list the sample projects a new project can be created from.
pub struct ListSamples;

impl ControlOperation for ListSamples {
    type Request = ListSamplesRequest;
    type Result = ListSamplesToolResult;
    const METHOD_NAME: &'static str = "list_samples";
    const ONE_LINE_DOC: &'static str =
        "List the sample projects a new project can be created from.";
    const AGENT_USAGE_NOTE: &'static str = "List the sample projects the runtime unit carries at its own version, each by the `name` `create_project`'s `sample` takes, its `title` and a `summary` of what it shows, in name order: none for a runtime run outside a runtime unit.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Project;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::LIST_SAMPLES);
}
