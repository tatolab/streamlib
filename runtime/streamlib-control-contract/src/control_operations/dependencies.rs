// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_runtime_client_contract::local_api_wire_contract::{
    DependencyTransactionToolResult, GraphTransactionToolResult, ListDependenciesToolResult,
    UpdateDependencyToolResult,
};

use super::EVERY_SURFACE;
use super::worked_examples;
use crate::control_operation::{
    ControlOperation, ControlOperationAgentToolListing, ControlOperationEffect,
    ControlOperationExample, ControlOperationExposure, ControlOperationGroup,
    ControlOperationStability,
};
use crate::control_operation_requests::{
    AddDependencyRequest, ListDependenciesRequest, RefreshFragmentsRequest,
    RemoveDependencyRequest, UpdateDependencyRequest, UseDependencyStreamRequest,
};

/// `add_dependency`: import another project into a project, as one transaction.
pub struct AddDependency;

impl ControlOperation for AddDependency {
    type Request = AddDependencyRequest;
    type Result = DependencyTransactionToolResult;
    const METHOD_NAME: &'static str = "add_dependency";
    const ONE_LINE_DOC: &'static str = "Import another project into a project, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Import another project, as one transaction by `author`: the project `source` holds, a folder or a `.zip` of one on this machine, is copied whole into `imports/<folder>/` and recorded in `tatolab-project.json` under `dependencies` with the `version` its copy carries, the SHA-256 over its files. Nothing reaches the network. The copy is this project's own from then on: its assets are referenced as `{\"asset\": \"<uuid>\", \"from\": \"<project uuid>\"}` and read from its folder, its streams are placed with `use_dependency_stream`, and to edit it, open `imports/<folder>/` as a project of its own. Answers the `dependency`. Once it applies, a project with a `pyproject.toml` has its `.venv` made current on its stream action queue, a `make_venv_current` action that touches no stream, and its change feed carries `venv_made_current` once that is done, so the node types its imports install list. Refused, nothing copied, for a source holding no `tatolab-project.json` or no project UUID in it, naming what is missing, for this project itself, a project already imported, or a folder already held.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Dependency;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::ADD_DEPENDENCY);
}

/// `update_dependency`: bring an imported project up to date with its source, as one transaction.
pub struct UpdateDependency;

impl ControlOperation for UpdateDependency {
    type Request = UpdateDependencyRequest;
    type Result = UpdateDependencyToolResult;
    const METHOD_NAME: &'static str = "update_dependency";
    const ONE_LINE_DOC: &'static str =
        "Bring an imported project up to date with its source, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Copy an import again, whole, as one transaction by `author`, from `source` or from the source it was last copied from, replacing its folder under `imports/` and its `version`. Every loaded stream holding one of its streams as a fragment follows the new copy. When files of its folder were edited since its copy, nothing changes: it answers them under `edited_files` with `overwrite_confirmation_needed`, and is sent again with `overwrite_edited_files` to overwrite them. Undo puts the folder back as it was. Once it applies, a project with a `pyproject.toml` has its `.venv` made current on its stream action queue, a `make_venv_current` action that touches no stream, and its change feed carries `venv_made_current` once that is done, so the node types its imports install list. Refused, nothing changed, for a project not imported, or a source holding another project.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Dependency;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::UPDATE_DEPENDENCY);
}

/// `list_dependencies`: list the projects a project imports.
pub struct ListDependencies;

impl ControlOperation for ListDependencies {
    type Request = ListDependenciesRequest;
    type Result = ListDependenciesToolResult;
    const METHOD_NAME: &'static str = "list_dependencies";
    const ONE_LINE_DOC: &'static str = "List the projects a project imports.";
    const AGENT_USAGE_NOTE: &'static str = "List the projects this one imports, each with its `folder`, `source` and `version` as `tatolab-project.json` holds them, and the files of its folder edited since its copy under `edited_files`; changes nothing. With `check_for_newer`, each source is read again and each import also answers `version_on_the_source` and `newer_version_exists`, or, for a source that could not be read, `check_refused_because`; nothing is copied until `update_dependency` copies it.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Dependency;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::LIST_DEPENDENCIES);
}

/// `remove_dependency`: take an imported project out of a project, as one transaction.
pub struct RemoveDependency;

impl ControlOperation for RemoveDependency {
    type Request = RemoveDependencyRequest;
    type Result = DependencyTransactionToolResult;
    const METHOD_NAME: &'static str = "remove_dependency";
    const ONE_LINE_DOC: &'static str =
        "Take an imported project out of a project, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Stop importing a project, as one transaction by `author`: its folder leaves `imports/` and its entry `tatolab-project.json`; undo puts both back as they were. Answers the `dependency` as it was. Once it applies, a project with a `pyproject.toml` has its `.venv` made current on its stream action queue, a `make_venv_current` action that touches no stream, and its change feed carries `venv_made_current` once that is done, so its node types list without the import's. Refused, nothing changed, while a node of any of this project's streams — a fragment holding one of its streams, or an input value holding `{\"asset\", \"from\"}` naming it, at any depth — still references it, naming each such node. Refused for a project not imported.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Dependency;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Destroys;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::REMOVE_DEPENDENCY);
}

/// `use_dependency_stream`: place an imported project's stream in a loaded stream's graph as a
/// fragment node, as one transaction.
pub struct UseDependencyStream;

impl ControlOperation for UseDependencyStream {
    type Request = UseDependencyStreamRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "use_dependency_stream";
    const ONE_LINE_DOC: &'static str = "Place an imported project's stream in a loaded stream's graph as a fragment node, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Place a stream of an imported project in a loaded stream's graph, as one committed transaction: a fragment node of type `tatolab.project:StreamFragment` whose setting `stream`, `{\"asset\", \"from\"}`, references the stream. The graph document stores the reference alone; each load, and each change to the stream or its import, places the stream's nodes and links as its graph document under `imports/<folder>/` is on disk, each node under the UUID UUIDv5(fragment node's UUID, its own). Its Graph Input and Output nodes are the fragment's ports. Answers as `transact` does. Refused for a name a node of the graph already holds, and for a stream that holds the stream it is placed in, naming each stream of the cycle.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Dependency;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::USE_DEPENDENCY_STREAM);
}

/// `refresh_fragments`: refresh, as one transaction, every fragment whose graph generator a changed
/// module defines.
pub struct RefreshFragments;

impl ControlOperation for RefreshFragments {
    type Request = RefreshFragmentsRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "refresh_fragments";
    const ONE_LINE_DOC: &'static str = "Refresh, as one transaction, every fragment whose graph generator a changed module defines.";
    const AGENT_USAGE_NOTE: &'static str = "Refresh, as one transaction, every fragment of the project's loaded streams whose graph generator, or a generator nested in it, is defined in a module one of `changed_files` holds: each is run again with the promoted settings its fragment keeps, as `import_graph_generator` refreshes it, each node keeping its UUID by name. Answers as `transact` does; with no such fragment, it answers no operation and records no transaction. A generator that raises refuses the whole refresh with its traceback, nothing changed.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
    const EXAMPLE: Option<ControlOperationExample> = Some(worked_examples::REFRESH_FRAGMENTS);
}
