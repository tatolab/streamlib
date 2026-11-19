// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_runtime_client_contract::local_api_wire_contract::{
    DescribeNodeTypesToolResult, ListNodeTypesToolResult, ListPinTypesToolResult,
    SetNodeTypeGroupToolResult,
};

use super::EVERY_SURFACE;
use crate::control_operation::{
    ControlOperation, ControlOperationAgentToolListing, ControlOperationEffect,
    ControlOperationExposure, ControlOperationGroup, ControlOperationStability,
};
use crate::control_operation_requests::{
    DescribeNodeTypesRequest, ListNodeTypesRequest, ListPinTypesRequest, SetNodeTypeGroupRequest,
};

/// `list_pin_types`: every built-in pin type, how each is written, and what converts it.
pub struct ListPinTypes;

impl ControlOperation for ListPinTypes {
    type Request = ListPinTypesRequest;
    type Result = ListPinTypesToolResult;
    const METHOD_NAME: &'static str = "list_pin_types";
    const ONE_LINE_DOC: &'static str =
        "List every built-in pin type, how each is written, and what converts it.";
    const AGENT_USAGE_NOTE: &'static str = "List every built-in pin type a port may carry, each under `pin_types` with its `name` (the `type` tag of its pin type), `label`, `documentation`, the `flows` a pin of it may state with its default first, the `annotations` it may state, how a Python declaration writes it in `python_forms` and the TypeScript SDK in `typescript_form`, and the JSON Schema of its stored value in `value_json_schema`, absent on a type that stores no value. The List and bundle rows say in `how_to_declare` how a pin of one is declared; a bundle has no name, so a port declaring one is described member by member by `describe_node_types`. `converts_to` lists each conversion a node type of this runtime offers from a pin of the type, and each one a node type of the project's installed modules states once a loaded stream of the project has described it in its own interpreter: the exact type it takes as `from`, the type it gives as `to`, and the `node_type` that converts. `connect` refuses a link that needs a conversion, so a caller adds a node of that `node_type` and links each end to it. Reads nothing of the project but what its loaded streams described.";
    const SINCE_CONTRACT_VERSION: u32 = 3;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::NodeType;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}

/// `list_node_types`: the node types and graph generators a project offers, by the source each
/// comes from.
pub struct ListNodeTypes;

impl ControlOperation for ListNodeTypes {
    type Request = ListNodeTypesRequest;
    type Result = ListNodeTypesToolResult;
    const METHOD_NAME: &'static str = "list_node_types";
    const ONE_LINE_DOC: &'static str =
        "List the node types and graph generators a project offers, by the source each comes from.";
    const AGENT_USAGE_NOTE: &'static str = "List the node types and graph generators a project offers, each under `sources` by `source`: the project's own node code outside `imports/` (`project`), each import as its folder holds it now (`dependency`, with its `project`, `folder`, `version` and `project_name`), each installed distribution that declares `tatolab.nodes` entry points (`installed_distribution`, with its `distribution` and `distribution_version`), then the node types the runtime brings itself (`built_in`). Project code is read from source without running it. Each node type has its `node_type` (what `add_node` and `describe_node_types` take), the first line of its description as `documentation`, its `category`, its `kind` (`process`, `function` or `shader`) and `described_from`, where the runtime's description of it comes from: `built_in`, `source`, or `interpreter` when a loaded stream of the project described it in its own interpreter. `describe_node_types` answers each one's inputs and outputs. A graph generator has its `generator` (what `import_graph_generator` takes), its docstring as `description`, where it is declared, and the settings it promotes as `promoted_settings`. A module that does not parse is listed under its source's `unreadable_modules` with why and where; the rest still lists.";
    const SINCE_CONTRACT_VERSION: u32 = 3;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::NodeType;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
}

/// `describe_node_types`: the typed inputs and outputs of the node types named.
pub struct DescribeNodeTypes;

impl ControlOperation for DescribeNodeTypes {
    type Request = DescribeNodeTypesRequest;
    type Result = DescribeNodeTypesToolResult;
    const METHOD_NAME: &'static str = "describe_node_types";
    const ONE_LINE_DOC: &'static str =
        "Describe the inputs and outputs of the node types named, each with its pin type.";
    const AGENT_USAGE_NOTE: &'static str = "Describe each node type named in `node_types`, as `list_node_types` names it, from the best description the runtime has, in the order named. A described node type carries the source fields `list_node_types` gives it, `described_from`, its whole `documentation`, `category`, `kind`, `declared_at` (its path relative to its source's root and its line; absent on a built-in), and its `inputs` and `outputs`. Each port has its `name`, `documentation`, `pin_type` (the type, or how it is found: `is`, `one_of`, `any`, `same_as_input` or `set_by_input`) and `flow` (`value`, `stream` or `trigger`), and the `annotations` it states. Each input also has its `mode` (`wire_or_value`, `wire_only` or `setting_only`, a setting with when it is `read`), its `default_value`, its `lifetime` (`kept` or `reset`), and the JSON Schema a value set on it with `set_node_input_value` must satisfy as `value_json_schema`, its range folded in. A port of a node type not yet typed has no `pin_type` or `flow` and states its `family`, and a function or shader port read from source its `annotation`; its settings are listed after its ports as `setting_only` inputs, and those `set_exposed_config_fields` takes as `exposable_config_fields`. A project node type the runtime has not described in an interpreter is described from source, which reads no pin type. A node type that cannot be described carries `not_described_because`, a refusal with the runtime's words and, where the runtime serves one, the call that checks the project's code; the rest still describe. Refused whole, with `node_type_not_found` and the call to `list_node_types`, for a node type the project does not offer.";
    const SINCE_CONTRACT_VERSION: u32 = 3;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::NodeType;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::ReadsOnly;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ListedAsATool;
}

/// `set_node_type_group`: file a node type under a group of the project's own, or put it back
/// where its author files it, as one transaction on its history.
pub struct SetNodeTypeGroup;

impl ControlOperation for SetNodeTypeGroup {
    type Request = SetNodeTypeGroupRequest;
    type Result = SetNodeTypeGroupToolResult;
    const METHOD_NAME: &'static str = "set_node_type_group";
    const ONE_LINE_DOC: &'static str = "File a node type under a group of the project's own, or put it back where its author files it, as one transaction on its history.";
    const AGENT_USAGE_NOTE: &'static str = "File the node type `node_type`, as `list_node_types` names it, under `group` in the project, as one transaction by `author` on its history that undoes like any edit: the project's own node types, an import's, an installed distribution's and the built-ins can each be moved, and the move is saved in the project's `tatolab-project.json` under `node_type_groups`, never in the module. A group is a path where it is inside another, as `Effects/Stylize`, made by filing a node type there and gone once none is; a `null` `group` takes the project's own filing away, so the group an import files it under, else the one its author declared, stands again. A project imported into another carries its map, which files the import's own node types there unless the importing project files them itself; it files nothing else. Groups are display only: a move never changes what a node does, how it is addressed, or the stream. `list_node_types`, `describe_node_types` and `list_project_node_types` serve the group a node type ends up in as `category`, and who filed it there as `category_from` (`project`, `import` or `declaration`). Answers the `group` the project now files it under, with the `transaction` and `commit`. Refused, with `node_type_not_found` and the call to `list_node_types`, for a node type neither the project nor the runtime offers to be filed, though a filing is always taken away, and for a group that is no path, such as one with a blank name in it.";
    const SINCE_CONTRACT_VERSION: u32 = 3;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::NodeType;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
    const EFFECT: ControlOperationEffect = ControlOperationEffect::Changes;
    const AGENT_TOOL_LISTING: ControlOperationAgentToolListing =
        ControlOperationAgentToolListing::ReachedThroughCallOperation;
}
