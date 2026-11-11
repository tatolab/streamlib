// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Every operation the registry holds, the table the adapters iterate, and the one call type a
//! dispatcher matches on.

use serde_json::Value;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    DescribeRuntimeToolResult, GraphTransactionToolResult, ListStreamsToolResult,
};

use crate::control_operation::{
    ControlOperation, ControlOperationDeclaration, ControlOperationExposure, ControlOperationGroup,
    ControlOperationStability,
};
use crate::control_operation_requests::{
    AddNodeRequest, DescribeRuntimeRequest, GraphRequest, ListStreamsRequest,
    OneLinkGraphEditRequest, TransactRequest,
};

/// The contract's version: bumped only by a breaking change to an operation already in it.
pub const CONTROL_CONTRACT_VERSION: u32 = 1;

const EVERY_SURFACE: &[ControlOperationExposure] = &[
    ControlOperationExposure::Studio,
    ControlOperationExposure::Agent,
    ControlOperationExposure::Public,
];

/// `describe_runtime`: this runtime's version, build and uptime.
pub struct DescribeRuntime;

impl ControlOperation for DescribeRuntime {
    type Request = DescribeRuntimeRequest;
    type Result = DescribeRuntimeToolResult;
    const METHOD_NAME: &'static str = "describe_runtime";
    const ONE_LINE_DOC: &'static str = "Describe this runtime: its version, its build id, how long it has been up and where it logs.";
    const AGENT_USAGE_NOTE: &'static str = "Describe this runtime: its crate `version`, its `build_id` (`<version>+<git sha>.<per-build nonce>`, telling two builds of one version apart), `up_for_ms`, the monotonic milliseconds since its engine started — a restart resets it — and `runtime_log_directory`, the folder its own log is written to, `null` when it keeps none. Names no stream.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
}

/// `list_streams`: every stream the runtime holds, with its state.
pub struct ListStreams;

impl ControlOperation for ListStreams {
    type Request = ListStreamsRequest;
    type Result = ListStreamsToolResult;
    const METHOD_NAME: &'static str = "list_streams";
    const ONE_LINE_DOC: &'static str =
        "List every stream the runtime holds, by stream id, with its location and state.";
    const AGENT_USAGE_NOTE: &'static str = "List every stream the runtime holds: its stream id as `stream`, its `display_name`, its `location` — its project directory, its project's UUID as `project` and its stream asset's UUID as `stream_asset`, `null` for a record an earlier runtime wrote — and its state: `running` (loaded, or recorded and re-loaded at the runtime's next start), `stopped`, `paused` (loaded with its project paused by `pause_project`, until `resume_project`), `preparing` (an action that loads it — a `start_project` or `reload_project` — is queued or under way on it, named by `action`, on its `step`: `reading_the_graph_documents`, `making_the_venv` or `starting`, `null` while it waits), `stopping` (a `stop_project` or a `remove_project` of its project is queued or under way on it, named by `action`, on the step `stopping`, `null` while it waits), or `failed` (implicated in the runtime's last two crashes in a row, or could not re-load at its start, and skipped at the start until a `start_project` or `reload_project` of its project retries it; or recorded by an earlier runtime, never loaded, with `failed_because` saying to run it again and `remove_project` forgetting it), with the node count of its loaded graph (`null` when it is not loaded), `failed_because`, why a failed stream failed (`null` otherwise), and `capture_devices_whose_access_was_never_asked_for`, each of `camera` and `microphone` that a node of the loaded stream refused because nobody has asked the user for its access, as when the runtime re-loaded the stream on its own at its start (empty otherwise): a `stop_project` then `start_project` of its project asks.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Stream;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
}

/// `graph`: a loaded stream's live graph, or every loaded stream's.
pub struct Graph;

impl ControlOperation for Graph {
    type Request = GraphRequest;
    type Result = Value;
    const METHOD_NAME: &'static str = "graph";
    const ONE_LINE_DOC: &'static str = "Export a loaded stream's live graph, or every loaded stream's, with each node's and link's state.";
    const AGENT_USAGE_NOTE: &'static str = "Export a loaded stream's current graph as JSON: the stream it was loaded as, its nodes by name with their types, config and ports, its links by node and port, with each node's and link's live state and counters beside them, and this machine's name (`machine`), the first part of `<machine>/<stream id>/<node>/<port>`. A fragment — a graph generator's stored output — renders as one node of `kind` `fragment` whose ports are its Graph Input and Output nodes' and whose `fragment` holds its own nodes and links, a fragment inside it rendered the same way; its nodes run under their path of names, `look/soft/blur`. Without `stream`, `{machine, streams}` holds every loaded stream's graph.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Experimental;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Observe;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
}

/// `transact`: several graph operations of one project as one transaction.
pub struct Transact;

impl ControlOperation for Transact {
    type Request = TransactRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "transact";
    const ONE_LINE_DOC: &'static str =
        "Apply operations on one project's stream graphs as one transaction, whole or not at all.";
    const AGENT_USAGE_NOTE: &'static str = "Apply operations on the graphs of one project's loaded streams as one transaction by its `author`: applied in order, whole or not at all, to each running stream, and with `mode` `commit`, the default, written to each stream's graph document and recorded as one commit on the project's history, which `undo` reverses as one. Answers `mode`, the `transaction` and `commit` recording a committed edit, each operation as applied, `added_nodes` with each new node's UUID and the name it took, and `connected_links` with each new link's `state` — `pending`, `wired`, or `error` with its `error_reason`. Refused whole, in the runtime's own words, with nothing changed. A config value `{\"asset\": \"<uuid>\"}` must name an asset the project holds. An `add_node` may give its node an `as`, which later operations of the same transaction name the node by. The transaction is announced on the project's change feed, the resource `list_assets` names as `transactions_resource_uri`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
}

/// `add_node`: one node added to a loaded stream's graph.
pub struct AddNode;

impl ControlOperation for AddNode {
    type Request = AddNodeRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "add_node";
    const ONE_LINE_DOC: &'static str =
        "Add a node to a loaded stream's graph by its type's import path, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Add a node to a loaded stream's graph by its type's import path, as one transaction, the runtime making the node's UUID. A Python type is named `module:QualifiedClassName` and must be importable by the stream's own interpreter — a module in the stream's project, or a package installed in its venv: the runtime never imports it, and describes it in that interpreter the first time it is added; each add spawns it in its own processor interpreter, which imports the module fresh. The type's port declaration is read when it is first described and kept; to change a type's ports, add it under a new class name.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
}

/// `connect`: one link between an output port and an input port.
pub struct Connect;

impl ControlOperation for Connect {
    type Request = OneLinkGraphEditRequest;
    type Result = GraphTransactionToolResult;
    const METHOD_NAME: &'static str = "connect";
    const ONE_LINE_DOC: &'static str =
        "Link an output port to an input port in one loaded stream, as one transaction.";
    const AGENT_USAGE_NOTE: &'static str = "Link an output port to an input port in one loaded stream, as one transaction, each end a node's UUID and a port name `graph` lists under that node's `ports`. A stream reads another stream's track at a relay through a `MoqTrackReceive` node. A link is spliced in by connecting a new node on both sides, then disconnecting the link it replaces — or all three in one `transact`.";
    const SINCE_CONTRACT_VERSION: u32 = 1;
    const STABILITY: ControlOperationStability = ControlOperationStability::Stable;
    const GROUP: ControlOperationGroup = ControlOperationGroup::Graph;
    const EXPOSURES: &'static [ControlOperationExposure] = EVERY_SURFACE;
}

/// Every operation the registry holds, in the order the contract file lists them.
pub const CONTROL_OPERATIONS: &[ControlOperationDeclaration] = &[
    ControlOperationDeclaration::of::<DescribeRuntime>(),
    ControlOperationDeclaration::of::<ListStreams>(),
    ControlOperationDeclaration::of::<Graph>(),
    ControlOperationDeclaration::of::<Transact>(),
    ControlOperationDeclaration::of::<AddNode>(),
    ControlOperationDeclaration::of::<Connect>(),
];

/// One call of a registered operation with its parsed params; a dispatcher matches on it
/// exhaustively, so an operation added here and not served is a compile error.
#[derive(Debug)]
pub enum ControlOperationCall {
    /// A `describe_runtime` call.
    DescribeRuntime(DescribeRuntimeRequest),
    /// A `list_streams` call.
    ListStreams(ListStreamsRequest),
    /// A `graph` call.
    Graph(GraphRequest),
    /// A `transact` call.
    Transact(TransactRequest),
    /// An `add_node` call.
    AddNode(AddNodeRequest),
    /// A `connect` call.
    Connect(OneLinkGraphEditRequest),
}

/// Why a method and its params do not make a call.
#[derive(Debug, PartialEq, Eq)]
pub enum ControlOperationCallParseRefusal {
    /// No registered operation has the method name.
    NoSuchMethod {
        /// The method name as sent.
        method_name: String,
    },
    /// The params do not match the operation's request.
    InvalidParams {
        /// The method name.
        method_name: &'static str,
        /// What the request's parse said.
        why: String,
    },
}

impl std::fmt::Display for ControlOperationCallParseRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchMethod { method_name } => {
                write!(
                    formatter,
                    "the runtime serves no operation named `{method_name}`"
                )
            }
            Self::InvalidParams { method_name, why } => {
                write!(
                    formatter,
                    "`{method_name}`'s params do not match its request: {why}"
                )
            }
        }
    }
}

impl ControlOperationCall {
    /// The call `method_name` names with `params`; absent params read as `{}`.
    pub fn parse(
        method_name: &str,
        params: Option<Value>,
    ) -> Result<Self, ControlOperationCallParseRefusal> {
        let params = params.unwrap_or_else(|| Value::Object(Default::default()));
        match method_name {
            DescribeRuntime::METHOD_NAME => {
                the_request_of::<DescribeRuntime>(params).map(Self::DescribeRuntime)
            }
            ListStreams::METHOD_NAME => {
                the_request_of::<ListStreams>(params).map(Self::ListStreams)
            }
            Graph::METHOD_NAME => the_request_of::<Graph>(params).map(Self::Graph),
            Transact::METHOD_NAME => the_request_of::<Transact>(params).map(Self::Transact),
            AddNode::METHOD_NAME => the_request_of::<AddNode>(params).map(Self::AddNode),
            Connect::METHOD_NAME => the_request_of::<Connect>(params).map(Self::Connect),
            unknown => Err(ControlOperationCallParseRefusal::NoSuchMethod {
                method_name: unknown.to_owned(),
            }),
        }
    }

    /// The method name of the operation called.
    pub fn method_name(&self) -> &'static str {
        match self {
            Self::DescribeRuntime(_) => DescribeRuntime::METHOD_NAME,
            Self::ListStreams(_) => ListStreams::METHOD_NAME,
            Self::Graph(_) => Graph::METHOD_NAME,
            Self::Transact(_) => Transact::METHOD_NAME,
            Self::AddNode(_) => AddNode::METHOD_NAME,
            Self::Connect(_) => Connect::METHOD_NAME,
        }
    }
}

fn the_request_of<Operation: ControlOperation>(
    params: Value,
) -> Result<Operation::Request, ControlOperationCallParseRefusal> {
    serde_json::from_value(params).map_err(|parse_failure| {
        ControlOperationCallParseRefusal::InvalidParams {
            method_name: Operation::METHOD_NAME,
            why: parse_failure.to_string(),
        }
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn every_registered_operation_has_a_unique_method_name() {
        let method_names: BTreeSet<&str> = CONTROL_OPERATIONS
            .iter()
            .map(|declaration| declaration.method_name)
            .collect();
        assert_eq!(method_names.len(), CONTROL_OPERATIONS.len());
    }

    #[test]
    fn every_registered_operation_is_documented_versioned_and_served() {
        for declaration in CONTROL_OPERATIONS {
            assert!(
                !declaration.one_line_doc.is_empty(),
                "{}",
                declaration.method_name
            );
            assert!(
                !declaration.one_line_doc.contains('\n'),
                "{}",
                declaration.method_name
            );
            assert!(
                !declaration.agent_usage_note.is_empty(),
                "{}",
                declaration.method_name
            );
            assert!(
                (1..=CONTROL_CONTRACT_VERSION).contains(&declaration.since_contract_version),
                "{}",
                declaration.method_name
            );
            assert!(
                !declaration.exposures.is_empty(),
                "{}",
                declaration.method_name
            );
        }
    }

    #[test]
    fn every_registered_method_name_parses_to_the_call_of_that_name() {
        let params_of_each_method = [
            ("describe_runtime", serde_json::json!({})),
            ("list_streams", serde_json::json!({})),
            ("graph", serde_json::json!({})),
            ("transact", serde_json::json!({ "operations": [] })),
            (
                "add_node",
                serde_json::json!({
                    "stream": "00000000-0000-0000-0000-000000000001",
                    "type": "nodes.grayscale_effect:GrayscaleEffect"
                }),
            ),
            (
                "connect",
                serde_json::json!({
                    "stream": "00000000-0000-0000-0000-000000000001",
                    "source": { "node": "a", "port": "video" },
                    "target": { "node": "b", "port": "video" }
                }),
            ),
        ];
        let registered: BTreeSet<&str> = CONTROL_OPERATIONS
            .iter()
            .map(|declaration| declaration.method_name)
            .collect();
        let exercised: BTreeSet<&str> = params_of_each_method
            .iter()
            .map(|(method_name, _)| *method_name)
            .collect();
        assert_eq!(registered, exercised);
        for (method_name, params) in params_of_each_method {
            let call = ControlOperationCall::parse(method_name, Some(params))
                .unwrap_or_else(|refusal| panic!("{refusal}"));
            assert_eq!(call.method_name(), method_name);
        }
    }

    #[test]
    fn an_unknown_method_and_unknown_params_are_refused_by_name() {
        assert_eq!(
            ControlOperationCall::parse("expose_port", None).unwrap_err(),
            ControlOperationCallParseRefusal::NoSuchMethod {
                method_name: "expose_port".to_owned()
            }
        );
        let refusal =
            ControlOperationCall::parse("list_streams", Some(serde_json::json!({ "stream": "x" })))
                .unwrap_err();
        assert!(matches!(
            refusal,
            ControlOperationCallParseRefusal::InvalidParams {
                method_name: "list_streams",
                ..
            }
        ));
    }
}
