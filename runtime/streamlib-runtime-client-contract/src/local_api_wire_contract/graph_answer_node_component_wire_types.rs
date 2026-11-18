// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What the runtime keeps on a node as `graph` answers it: its state and why, its counters, and
//! where and how its code runs.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{GraphInterfacePortOnTheWire, GraphVariableOnTheWire};

/// What the runtime keeps on a node, each under its own key; a key is absent where the node
/// keeps nothing of that kind.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GraphNodeComponentsOnTheWire {
    /// The node's state now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<NodeStateOnTheWire>,
    /// Why the node failed, while its state is `Error`; null on a node that has not failed.
    #[cfg_attr(
        feature = "json-schema",
        schemars(required, schema_with = "a_string_or_null")
    )]
    pub failed_because: Option<String>,
    /// What the node's `process()` keeps raising, while it does; null once a call runs cleanly.
    #[cfg_attr(
        feature = "json-schema",
        schemars(required, schema_with = "a_string_or_null")
    )]
    pub process_raising_because: Option<String>,
    /// Why a running node is not ready, in its own words, while it says so; null once it
    /// reports itself ready.
    #[cfg_attr(
        feature = "json-schema",
        schemars(required, schema_with = "a_string_or_null")
    )]
    pub not_ready_because: Option<String>,
    /// On a node in a helper process of its own, the helper processes started for it since its
    /// stream loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub helper_process_restarts: Option<HelperProcessRestartsOnTheWire>,
    /// The node's counters: the bags lost on its inbound links and refused and sent on its
    /// output ports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<NodeMetricsOnTheWire>,
    /// Whether the node is paused with its stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
    /// `true` on a node a removal is deleting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_deletion: Option<bool>,
    /// Whether the node's thread has signalled it is ready, and has been told to continue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_barrier: Option<NodeReadyBarrierOnTheWire>,
    /// The channel the node's thread is told to shut down on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shutdown_channel: Option<NodeShutdownChannelOnTheWire>,
    /// The thread the node runs on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_handle: Option<NodeThreadHandleOnTheWire>,
    /// The node's instance in the runtime's own process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_instance: Option<NodeInstanceOnTheWire>,
    /// Where a function node is evaluated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<FunctionNodeHostOnTheWire>,
    /// Whether the helper that last evaluated a function node ran it compiled; absent until one
    /// reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran_compiled: Option<bool>,
    /// The shader nodes evaluated with this one as one island.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fused_group: Option<ShaderNodeFusedGroupOnTheWire>,
    /// On a stateful function node, each field of the state its stream carries now, by field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_node_state: Option<Map<String, Value>>,
    /// On a graph interface node, the port of the stream it declares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_interface: Option<GraphInterfacePortOnTheWire>,
    /// On a graph variable's node, the variable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_variable: Option<GraphVariableOnTheWire>,
}

/// The schema of a string that is always present and may be null.
#[cfg(feature = "json-schema")]
fn a_string_or_null(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({ "type": ["string", "null"] })
}

/// A node's state as `graph` renders it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum NodeStateOnTheWire {
    /// Waiting to be started.
    Pending,
    /// Prepared, its `setup` not yet run.
    Idle,
    /// Running.
    Running,
    /// Paused with its stream.
    Paused,
    /// Shutting down.
    Stopping,
    /// Stopped.
    Stopped,
    /// Failed; `graph` says why.
    Error,
    /// Its helper process died on its own and a new one starts after a backoff.
    Restarting,
}

/// The helper processes started for a node since its stream loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct HelperProcessRestartsOnTheWire {
    /// New helper processes started since the stream loaded.
    pub restart_count: u32,
    /// The last death's exit and the end of its standard error; null before the first.
    pub last_exit_reason: Option<String>,
}

/// A node's counters, each by the link or the port it counts on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NodeMetricsOnTheWire {
    /// The bags lost on every inbound link, summed.
    pub frames_dropped: u64,
    /// The bags lost on each inbound link, by link id.
    pub dropped_bags_by_link: BTreeMap<String, u64>,
    /// The bags refused at the ceiling on each output port with a channel, by port.
    pub refused_bags_by_output_port: BTreeMap<String, u64>,
    /// The bags sent on each output port with a channel, by port: what the losses downstream
    /// are counted against.
    pub written_bags_by_output_port: BTreeMap<String, u64>,
    /// The samples discarded on each inbound link into a windowed port, by link id; absent when
    /// none was.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub discarded_samples_by_link: BTreeMap<String, u64>,
    /// The groups given up upstream on each output port whose writer gave one up, by port;
    /// absent when none was.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub given_up_groups_by_output_port: BTreeMap<String, u64>,
    /// What moved this function node out of its readers' islands into a process of its own;
    /// absent on a node never moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promoted_out_of_its_readers_islands: Option<FunctionNodePromotionOnTheWire>,
}

/// What moved a function node out of its readers' islands into a process of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct FunctionNodePromotionOnTheWire {
    /// The times it was moved; a node is moved once.
    pub promotions: u32,
    /// The name in the graph of the reader whose helper the node crashed or stalled.
    pub after_crashing_the_helper_of: String,
    /// Why that helper was lost, as its death was reported.
    pub reason: String,
}

/// Whether a node's thread has signalled it is ready, and has been told to continue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NodeReadyBarrierOnTheWire {
    /// Whether the node's thread signalled it is ready.
    pub ready_signaled: bool,
    /// Whether the node's thread was told to continue.
    pub continue_received: bool,
}

/// The channel a node's thread is told to shut down on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NodeShutdownChannelOnTheWire {
    /// Whether the node's thread has taken the channel's receiver.
    pub receiver_taken: bool,
}

/// The thread a node runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NodeThreadHandleOnTheWire {
    /// The thread's id, as the runtime prints it.
    pub thread_id: String,
    /// The thread's name.
    pub thread_name: String,
}

/// A node's instance in the runtime's own process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct NodeInstanceOnTheWire {
    /// The strong references held to the instance.
    pub arc_strong_count: usize,
    /// The weak references held to the instance.
    pub arc_weak_count: usize,
}

/// Where a function node is evaluated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum FunctionNodeHostOnTheWire {
    /// Inside the read of each reader named, by node id.
    InsideTheReadOf(Vec<String>),
    /// In a helper process of its own.
    InAHelperProcessOfItsOwn,
}

/// The shader nodes one evaluator runs as one island, and how its evaluations cut them into
/// kernels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ShaderNodeFusedGroupOnTheWire {
    /// The node that evaluates the island.
    pub evaluated_by: String,
    /// The island's members, by name, in evaluation order.
    pub members: Vec<String>,
    /// The members each kernel of the last evaluation fused, by name; null before the first.
    pub kernels: Option<Vec<Vec<String>>>,
    /// The submissions every evaluation so far recorded.
    pub submissions: u64,
    /// The dispatches every evaluation so far recorded.
    pub dispatches: u64,
}
