// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The node's MCP server, served by the official SDK, `rmcp`.
//!
//! [`LocalApiMcpServerHandler`] is the node's whole MCP surface: its tools are
//! the control vocabulary — the observation verbs graph, tap, logs, exchange
//! and shutdown, beside the graph-mutation verbs `add_node`, `remove_node`,
//! `connect` and `disconnect`, each naming a node by its name — and beside
//! them it serves the node catalog and the live graph as resources
//! ([`crate::mcp_resources`]) and recipes over those tools as prompts
//! ([`crate::mcp_prompts`]). `rmcp` owns the protocol; this module owns only
//! what the node says through it.
//!
//! A mutation tool answers when the engine accepted the change into its graph;
//! the wiring itself commits on the engine's own compile task, whose failure
//! `graph` and `logs` show rather than this call.
//!
//! `exchange` is the one tool whose result is not text: it answers a
//! published surface id with the frame itself, as an image content block the
//! host renders in-session. It composes with `tap` entirely at the caller,
//! which decodes a bag and reads whatever field it knows carries a surface id.
//!
//! Two of the tools (`tap`, `logs`) front WebSocket *streams* in the REST API.
//! MCP tools are request/response, so each bridges its stream to a **bounded
//! sample** — both by a count AND a monotonic sample window (a quiet channel /
//! idle event stream returns the partial sample rather than blocking the tool
//! call) — and returns the collected sample as the tool result.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use parking_lot::Mutex;
use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ListResourcesResult, PaginatedRequestParams,
    ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse, ServerCapabilities,
    ServerConfig, SubscriptionFilter,
};
use rmcp::service::{RequestContext, SubscriptionContext};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use rmcp::{prompt_handler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use streamlib::sdk::descriptors::ProcessorClassImportPath;
use streamlib::sdk::graph::{InputLinkPortRef, LinkUniqueId, OutputLinkPortRef};
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::pubsub::{Event, EventListener, PUBSUB, topics};
use streamlib::sdk::runtime::{ExchangedPublishedSurfaceFramePngImage, RuntimeOperations};
use tokio_util::sync::CancellationToken;

/// The only protocol revision the node serves: the latest `rmcp` implements.
const SERVED_MCP_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[ProtocolVersion::LATEST];

/// Server identity carried in every result's `serverInfo`.
const MCP_SERVER_NAME: &str = "streamlib-api-server";

/// Server version carried in `serverInfo` — the api-server crate version.
const MCP_SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The guidance `server/discover` hands an agent before its first call.
const LOCAL_API_MCP_SERVER_INSTRUCTIONS: &str = "StreamLib runtime control plane for one running node. Observe it with `graph` (nodes by name, their types and port names, and links), `tap` (raw bags on an output port, addressed `<runtime_name>/<node>/<port>`), `logs` and `exchange` (a published frame's pixels). Change its live graph with `add_node`, `connect`, `disconnect` and `remove_node`, each naming a node by its name: a Python class written to a module the app can import — a file beside the app, or a pip-installed package — is added by its `module:ClassName` path and runs in its own helper process; a link is spliced in by connecting the new node on both sides, then disconnecting the link it replaces. Read `graph` first for names and port names, and again afterwards to confirm a link's state is `wired` and the node is `Running`. A `connect` onto a node in a helper process returns before that helper has opened its port, so its link reads `pending` until the helper answers and then `wired`; a link that reads `error` carries the refusing end's own reason in `error_reason` and will never carry a bag — read the reason, `disconnect` it, and fix what it names. Both ends of a `connect` are ports on this node. The resource `streamlib://node-catalog` lists every type `add_node` can take with its config schema and ports, and `streamlib://graph` is the live graph. The prompts are step-by-step recipes over these tools: inserting a node into a link, fanning an output to another consumer, showing a channel on a virtual camera, and looking at what a channel carries.";

/// Bounded sample sizes for the streaming-tool → request/response bridge when
/// the caller does not pin its own `count`.
const DEFAULT_TAP_SAMPLE_COUNT: usize = 8;
const DEFAULT_LOGS_SAMPLE_COUNT: usize = 16;

/// Hard ceiling on a requested sample `count`, so a tool call cannot pin an
/// unbounded collection loop.
const MAX_SAMPLE_COUNT: usize = 1024;

/// Per-bag ceiling on the bytes a `tap` result hex-encodes, when the caller
/// names none.
///
/// Generous rather than frugal, because a trimmed bag is not a smaller answer —
/// it is no answer: a bag is a msgpack map, so a decoder needs all of it or
/// none, and every consumer here refuses a truncated one rather than reading
/// half a value.
const DEFAULT_MAX_TAP_BAG_BYTES: usize = 1024 * 1024;

/// Total bag bytes one `tap` result encodes, and therefore also the largest
/// per-bag cap a caller may name — [`bounded_tap_bag_bytes`] clamps to it.
///
/// The two are one number rather than two so the bound holds by construction:
/// were the per-bag cap allowed above this, a single bag could exceed the whole
/// response budget and nothing would stop it. Hex doubles this on the wire, and
/// serializing the result holds the encoded text and the JSON body at once,
/// which is why the figure is modest next to what a channel may carry — a bag
/// too big for a request/response tool is what `/ws/tap/{channel}` streams
/// verbatim.
const MAX_TAP_RESPONSE_BAG_BYTES: usize = 16 * 1024 * 1024;

// The default has to fit the budget it is charged against, or the very first
// bag of an ordinary call would trip it. Checked by the compiler rather than a
// test, because it is a fact about two literals.
const _: () = assert!(DEFAULT_MAX_TAP_BAG_BYTES <= MAX_TAP_RESPONSE_BAG_BYTES);

/// Long-edge ceiling for the image an `exchange` result carries inline, and
/// the default when a caller names no cap of its own.
///
/// A vision-model ingestion ceiling — not a GPU or protocol constant.
const EXCHANGE_IMAGE_LONG_EDGE_PIXEL_CAP: u32 = 1568;

/// Upper bound on how long the `logs` tool waits to fill its sample before
/// returning what it has collected. This is the bounded sample *window* for the
/// otherwise-unbounded event stream; a sparse / idle runtime returns early with
/// fewer events rather than blocking. Monotonic (tokio timer), never wall-clock.
const LOGS_SAMPLE_WINDOW: Duration = Duration::from_millis(500);

/// Upper bound on how long the `tap` tool waits to fill its bag sample before
/// returning what it has collected. The tap forwarder sends nothing on an idle,
/// slow, or paused channel (it backs off between empty polls), so without
/// this window a request/response tool call would block until `count` bags
/// actually flow. A quiet channel returns the partial sample (0..N bags)
/// instead. Monotonic (tokio timer), never wall-clock; mirrors
/// [`LOGS_SAMPLE_WINDOW`].
const TAP_SAMPLE_WINDOW: Duration = Duration::from_millis(500);

/// The node's MCP server handler: its tools, resources and prompts over the
/// node's [`RuntimeOperations`].
#[derive(Clone)]
pub(crate) struct LocalApiMcpServerHandler {
    pub(crate) runtime: Arc<dyn RuntimeOperations>,
    local_api_stopping: CancellationToken,
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
}

impl LocalApiMcpServerHandler {
    /// `local_api_stopping` ends every held `subscriptions/listen` with its
    /// final result, so the server's graceful shutdown never waits on a host
    /// that holds one open.
    pub(crate) fn new(
        runtime: Arc<dyn RuntimeOperations>,
        local_api_stopping: CancellationToken,
    ) -> Self {
        Self {
            runtime,
            local_api_stopping,
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
        }
    }
}

/// `POST /mcp`'s service: `rmcp`'s Streamable HTTP transport over one
/// [`LocalApiMcpServerHandler`], stateless, serving only
/// [`SERVED_MCP_PROTOCOL_VERSIONS`].
pub(crate) fn local_api_mcp_streamable_http_service(
    runtime: Arc<dyn RuntimeOperations>,
    local_api_stopping: CancellationToken,
) -> StreamableHttpService<LocalApiMcpServerHandler, NeverSessionManager> {
    let handler = LocalApiMcpServerHandler::new(runtime, local_api_stopping);
    StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(NeverSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_stateless_protocol_metadata_required(true)
            .with_json_response(true),
    )
}

// ============================================================================
// Tool arguments
// ============================================================================

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TapToolArguments {
    #[schemars(
        description = "The output port's address, `<runtime_name>/<node>/<port>`, under this node's own runtime name (the top-level `runtime_name` in `graph`). A port is tappable once a link carries from it."
    )]
    channel: String,
    #[schemars(
        range(min = 1),
        description = "Number of bags to collect before returning. Defaults to a small sample."
    )]
    count: Option<usize>,
    #[schemars(
        range(min = 1, max = MAX_TAP_RESPONSE_BAG_BYTES),
        description = "Per-bag ceiling on the bytes hex-encoded into the result. A bag over the cap comes back flagged `hex_truncated` and cannot be decoded, so raise this rather than accept one. Defaults high enough to carry any audio block whole."
    )]
    max_bag_bytes: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LogsToolArguments {
    #[schemars(
        range(min = 1),
        description = "Max events to collect before returning. Defaults to a small sample."
    )]
    count: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ExchangeToolArguments {
    #[schemars(
        description = "A surface id a bag published, e.g. the `{slot}#{generation}` of a pooled frame. A retired id is refused rather than answered with the slot's newer pixels — tap a newer bag and exchange that."
    )]
    surface_id: String,
    #[schemars(
        range(min = 1, max = EXCHANGE_IMAGE_LONG_EDGE_PIXEL_CAP),
        description = "Bound the returned image's long edge to this many pixels, aspect preserved and never upscaled. Defaults to the maximum, and a larger value is clamped to it: full resolution is the REST route's job, never an inline block."
    )]
    downscale_long_edge_pixel_cap: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ShutdownToolArguments {
    #[schemars(
        description = "Human-readable attribution logged with the request. Omit for unspecified."
    )]
    reason: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AddNodeToolArguments {
    #[serde(rename = "type")]
    #[schemars(
        description = "The node's class import path, e.g. `nodes.grayscale_effect:GrayscaleEffect`."
    )]
    processor_class_import_path: String,
    #[schemars(
        description = "The node's configuration, as the keys its config schema declares. Omit for none."
    )]
    config: Option<Map<String, Value>>,
    #[schemars(
        description = "The node's name: cast to lowercase URL-safe (`Front Camera` becomes `front-camera`), and refused when a node already has that name. Omit it to take the class's short name, cast, with the next free `-2`, `-3` … when one is taken. The name is the node's part of its ports' addresses."
    )]
    name: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RemoveNodeToolArguments {
    #[schemars(description = "A node's name, as `graph` or `add_node` reported it.")]
    name: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ConnectToolArguments {
    #[schemars(description = "The source node's name.")]
    from_node: String,
    #[schemars(description = "The source's output port name.")]
    from_port: String,
    #[schemars(description = "The destination node's name.")]
    to_node: String,
    #[schemars(description = "The destination's input port name.")]
    to_port: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DisconnectToolArguments {
    #[schemars(description = "A link id `graph` or `connect` reported.")]
    link_id: String,
}

/// A tool's answer: its result, or the message an `isError` result carries.
type ToolCallAnswer = Result<CallToolResult, String>;

// ============================================================================
// Tools
// ============================================================================

#[tool_router]
impl LocalApiMcpServerHandler {
    #[tool(
        description = "Export the current graph as JSON: the stream it was loaded as, its nodes by name with their types, config and ports, its links by node and port, the ports it exposes, with each node's and link's live state and counters beside them, the capability extensions loaded in this process, and this runtime's name (`runtime_name`), the first part of every tap channel."
    )]
    async fn graph(&self) -> ToolCallAnswer {
        let graph = self
            .runtime
            .to_json_async()
            .await
            .map_err(|e| format!("graph export failed: {e}"))?;
        Ok(json_text_tool_result(&graph))
    }

    #[tool(
        description = "Attach a read-only tap to a channel and collect a bounded sample of raw bags (FrameHeader-framed bytes; the hex plus byte length per bag). Bags arrive whole unless one exceeds `max_bag_bytes`, which is flagged as `hex_truncated`. The whole result is also byte-budgeted: the sample stops at the first bag that would exceed it, so `bags_withheld_at_byte_budget` is 0 or 1 — that one bag was received and discarded, and it accounts for the whole gap between `requested` and `received` when the window had time left."
    )]
    async fn tap(
        &self,
        Parameters(TapToolArguments {
            channel,
            count,
            max_bag_bytes,
        }): Parameters<TapToolArguments>,
    ) -> ToolCallAnswer {
        let sample = bounded_sample_count(count, DEFAULT_TAP_SAMPLE_COUNT);
        let max_bag_bytes = bounded_tap_bag_bytes(max_bag_bytes);

        let mut subscription = self
            .runtime
            .tap_async(channel.clone(), Some(sample))
            .await
            .map_err(|e| format!("tap attach failed: {e}"))?;

        let mut bags: Vec<Value> = Vec::with_capacity(sample);
        let mut remaining_response_bytes = MAX_TAP_RESPONSE_BAG_BYTES;
        let mut bags_withheld_at_byte_budget = 0usize;
        let deadline = tokio::time::Instant::now() + TAP_SAMPLE_WINDOW;
        while bags.len() < sample {
            match tokio::time::timeout_at(deadline, subscription.recv()).await {
                Ok(Some(bytes)) => {
                    let encoded_len = bytes.len().min(max_bag_bytes);
                    if encoded_len > remaining_response_bytes {
                        // Counted rather than silently eaten: this bag was received
                        // and is being dropped, so a caller reconciling `requested`
                        // against `received` is not short by an unexplained one.
                        bags_withheld_at_byte_budget += 1;
                        break;
                    }
                    remaining_response_bytes -= encoded_len;
                    bags.push(tap_bag_json(&bytes[..encoded_len], bytes.len()));
                }
                // Tap exhausted (count reached / forwarder ended), or the bounded
                // sample window elapsed on a quiet channel — return the partial sample.
                Ok(None) | Err(_) => break,
            }
        }
        let dropped_bags = subscription.dropped_bags();

        // `TapSubscription::drop` joins the forwarder OS thread; a synchronous join
        // must never run on a tokio worker, so detach it off the async runtime.
        if let Err(join_error) = tokio::task::spawn_blocking(move || drop(subscription)).await {
            tracing::warn!(channel = %channel, "tap detach task failed to join: {join_error}");
        }

        Ok(json_text_tool_result(&json!({
            "channel": channel,
            "requested": sample,
            "received": bags.len(),
            "window_ms": TAP_SAMPLE_WINDOW.as_millis(),
            "dropped_bags": dropped_bags,
            "max_bag_bytes": max_bag_bytes,
            "bags_withheld_at_byte_budget": bags_withheld_at_byte_budget,
            "bags": bags,
        })))
    }

    #[tool(
        description = "Collect a bounded sample of the runtime event stream (all topics) within a short monotonic window."
    )]
    async fn logs(
        &self,
        Parameters(LogsToolArguments { count }): Parameters<LogsToolArguments>,
    ) -> ToolCallAnswer {
        let sample = bounded_sample_count(count, DEFAULT_LOGS_SAMPLE_COUNT);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
        let listener: Arc<Mutex<dyn EventListener>> =
            Arc::new(Mutex::new(McpEventForwarder { tx }));
        // Without a subscriber the sample would be an honest-looking zero.
        PUBSUB
            .subscribe(topics::ALL, Arc::clone(&listener))
            .map_err(|subscribe_error| format!("logs subscription: {subscribe_error}"))?;

        let mut events: Vec<Value> = Vec::with_capacity(sample);
        let deadline = tokio::time::Instant::now() + LOGS_SAMPLE_WINDOW;
        while events.len() < sample {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(event)) => events.push(event_json(&event)),
                // Forwarder channel closed, or the bounded sample window elapsed.
                Ok(None) | Err(_) => break,
            }
        }
        // The bus removes the subscription at its next publish or subscribe.
        drop(listener);

        Ok(json_text_tool_result(&json!({
            "requested": sample,
            "received": events.len(),
            "window_ms": LOGS_SAMPLE_WINDOW.as_millis(),
            "events": events,
        })))
    }

    /// The cap defaults to [`EXCHANGE_IMAGE_LONG_EDGE_PIXEL_CAP`] and is clamped
    /// to it: a caller may ask for less than the ceiling and never more.
    #[tool(
        description = "Exchange a published surface id for that frame's pixels, returned as a PNG image block you can see directly. Ids come from bags a `tap` returned — this tool never reads a channel itself. The image is downscaled to a declared long-edge cap; the result states the surface's true extent and the REST route that returns the exact full-resolution bytes."
    )]
    async fn exchange(
        &self,
        Parameters(ExchangeToolArguments {
            surface_id,
            downscale_long_edge_pixel_cap,
        }): Parameters<ExchangeToolArguments>,
    ) -> ToolCallAnswer {
        let long_edge_pixel_cap = downscale_long_edge_pixel_cap
            .unwrap_or(EXCHANGE_IMAGE_LONG_EDGE_PIXEL_CAP)
            .clamp(1, EXCHANGE_IMAGE_LONG_EDGE_PIXEL_CAP);

        let exchanged = self
            .runtime
            .exchange_published_surface_id_for_png_image_bytes_async(
                surface_id.clone(),
                Some(long_edge_pixel_cap),
            )
            .await
            .map_err(|e| format!("exchange failed: {e}"))?;
        Ok(exchanged_frame_image_tool_call_result(
            &surface_id,
            long_edge_pixel_cap,
            &exchanged,
        ))
    }

    #[tool(
        description = "Ask the runtime to shut down. This is a request observed by whoever owns the run loop, which then runs a normal teardown — not an immediate kill. Idempotent: requesting twice is not an error. Returns as soon as the request is accepted; teardown is not awaited."
    )]
    fn shutdown(
        &self,
        Parameters(ShutdownToolArguments { reason }): Parameters<ShutdownToolArguments>,
    ) -> ToolCallAnswer {
        let reason = reason.unwrap_or_default();
        self.runtime
            .request_runtime_shutdown(&reason)
            .map_err(|e| format!("shutdown request failed: {e}"))?;
        Ok(json_text_tool_result(&json!({
            "status": crate::state::RUNTIME_SHUTDOWN_REQUESTED_STATUS,
            "reason": reason,
        })))
    }

    #[tool(
        description = "Add a node to the running graph by its class import path — the `type` string `graph` reports for every node and `streamlib://node-catalog` lists. A Python class is named `module:QualifiedClassName` and must be importable from the app's own environment (a module beside the app, or a pip-installed package); a built-in is named by the `type` an existing node of that kind shows. Returns the name the node received, which `connect` and `remove_node` take, once the engine has spawned it — a Python class in its own helper process, which imports the module fresh, so edited code is picked up by every new add. The class's port declaration is read the first time it is added and kept; to change a class's ports, add it under a new class name. Read `graph` to see its state and ports."
    )]
    async fn add_node(
        &self,
        Parameters(arguments): Parameters<AddNodeToolArguments>,
    ) -> ToolCallAnswer {
        let processor_class_import_path =
            ProcessorClassImportPath::new(&arguments.processor_class_import_path)
                .map_err(|e| format!("add_node `type`: {e}"))?;
        // Absent is an empty object: a config struct deserializes from `{}`.
        let config = Value::Object(arguments.config.unwrap_or_default());
        let mut spec = ProcessorSpec::new(processor_class_import_path, config);
        spec.display_name = arguments.name;

        let added = self
            .runtime
            .add_processor_async(spec)
            .await
            .map_err(|e| format!("add_node failed: {e}"))?;
        Ok(json_text_tool_result(&json!({ "name": added.name })))
    }

    #[tool(
        description = "Remove a node from the running graph by name, stopping it. Its links go with it."
    )]
    async fn remove_node(
        &self,
        Parameters(RemoveNodeToolArguments { name }): Parameters<RemoveNodeToolArguments>,
    ) -> ToolCallAnswer {
        let node = self
            .runtime
            .the_node_named(&name)
            .map_err(|e| format!("remove_node failed: {e}"))?;
        self.runtime
            .remove_processor_async(node.processor_id)
            .await
            .map_err(|e| format!("remove_node failed: {e}"))?;
        Ok(json_text_tool_result(&json!({ "removed_name": node.name })))
    }

    #[tool(
        description = "Link an output port to an input port on this node, and answer the new link's `link_id`. Each end is a node's name and a port name — the names `graph` lists for each node and under its `outputs` and `inputs`."
    )]
    async fn connect(
        &self,
        Parameters(arguments): Parameters<ConnectToolArguments>,
    ) -> ToolCallAnswer {
        let from = self
            .runtime
            .the_node_named(&arguments.from_node)
            .map(|node| OutputLinkPortRef::new(node.processor_id, arguments.from_port))
            .map_err(|e| format!("connect failed: {e}"))?;
        let to = self
            .runtime
            .the_node_named(&arguments.to_node)
            .map(|node| InputLinkPortRef::new(node.processor_id, arguments.to_port))
            .map_err(|e| format!("connect failed: {e}"))?;

        let link_id = self
            .runtime
            .connect_async(from, to)
            .await
            .map_err(|e| format!("connect failed: {e}"))?;
        let how_the_graph_reads_it = how_the_graph_reads_one_link(&self.runtime, &link_id).await;
        Ok(json_text_tool_result(&json!({
            "link_id": link_id.as_str(),
            "state": how_the_graph_reads_it.state,
        })))
    }

    #[tool(
        description = "Remove a link from a running graph by the id `graph` or `connect` reported."
    )]
    async fn disconnect(
        &self,
        Parameters(DisconnectToolArguments { link_id }): Parameters<DisconnectToolArguments>,
    ) -> ToolCallAnswer {
        self.runtime
            .disconnect_async(LinkUniqueId::from(link_id.as_str()))
            .await
            .map_err(|e| format!("disconnect failed: {e}"))?;
        Ok(json_text_tool_result(
            &json!({ "disconnected_link_id": link_id }),
        ))
    }
}

#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for LocalApiMcpServerHandler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
        .with_server_info(Implementation::new(MCP_SERVER_NAME, MCP_SERVER_VERSION))
        .with_instructions(LOCAL_API_MCP_SERVER_INSTRUCTIONS)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(SERVED_MCP_PROTOCOL_VERSIONS)
    }

    /// No list ever changes under a host, so a listen is acknowledged with
    /// nothing subscribed and held until it is cancelled.
    fn accepted_subscription_filter(
        &self,
        _requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        Some(SubscriptionFilter::default())
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
        tokio::select! {
            () = context.cancelled() => {}
            () = self.local_api_stopping.cancelled() => {}
        }
        Ok(())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(crate::mcp_resources::resources_list_result())
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        crate::mcp_resources::read_resource(&self.runtime, &request.uri)
            .await
            .map(Into::into)
    }
}

/// What `graph` says about a link just connected, beside its id.
///
/// A struct rather than a bare `Option<String>` so a second reading can join it
/// without changing every call site.
#[derive(Default)]
struct HowTheGraphReadsOneLink {
    state: Option<String>,
}

/// The state the new link reads, taken from the same `graph` a caller would
/// read next.
///
/// Read after the connect rather than returned by it: `connect_async` hands
/// back a link id, and everything else about the link is what `graph` renders.
/// A render that fails leaves the state unknown rather than failing a connect
/// that already took.
async fn how_the_graph_reads_one_link(
    runtime: &Arc<dyn RuntimeOperations>,
    link_id: &LinkUniqueId,
) -> HowTheGraphReadsOneLink {
    let graph = match runtime.to_json_async().await {
        Ok(graph) => graph,
        Err(unreadable) => {
            // Swallowing this would leave the caller a null state with nothing
            // to act on, and the operator nothing to re-derive it from.
            tracing::warn!(
                "a link was connected and this node's own graph could not be read to say what \
                 state it is in: {unreadable}"
            );
            return HowTheGraphReadsOneLink::default();
        }
    };
    let state = graph["links"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|link| link["id"].as_str() == Some(link_id.as_str()))
        .and_then(|link| link["state"].as_str())
        .map(str::to_string);
    HowTheGraphReadsOneLink { state }
}

// ============================================================================
// Result content
// ============================================================================

/// A successful tool result: the value as one pretty-JSON text block, the form
/// every tool here states a result a caller parses in.
fn json_text_tool_result(value: &Value) -> CallToolResult {
    CallToolResult::success(vec![json_text_content_block(value)])
}

fn json_text_content_block(value: &Value) -> ContentBlock {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    ContentBlock::text(text)
}

/// A successful `exchange` result: the frame as an image block the host
/// renders in-session, then a text block stating what the surface itself
/// carries and where the exact bytes live.
///
/// Both extents are reported because they differ whenever the cap applied, and
/// a downscaled picture whose true resolution went unsaid is a measurement
/// waiting to be wrong.
fn exchanged_frame_image_tool_call_result(
    published_surface_id: &str,
    downscale_long_edge_pixel_cap: u32,
    exchanged: &ExchangedPublishedSurfaceFramePngImage,
) -> CallToolResult {
    let stated = json!({
        "surface_id": published_surface_id,
        "source_surface_pixel_width": exchanged.source_surface_pixel_width,
        "source_surface_pixel_height": exchanged.source_surface_pixel_height,
        "encoded_image_pixel_width": exchanged.encoded_image_pixel_width,
        "encoded_image_pixel_height": exchanged.encoded_image_pixel_height,
        "downscale_long_edge_pixel_cap": downscale_long_edge_pixel_cap,
        "exact_bytes_rest_route": format!(
            "GET {}",
            crate::handlers::surface_image_exchange_route_path_for_surface_id(published_surface_id)
        ),
    });
    CallToolResult::success(vec![
        ContentBlock::image(
            base64::engine::general_purpose::STANDARD.encode(&exchanged.png_image_bytes),
            "image/png",
        ),
        json_text_content_block(&stated),
    ])
}

/// Clamp a requested sample count into `[1, MAX_SAMPLE_COUNT]`, defaulting when
/// the caller left it unset.
fn bounded_sample_count(requested: Option<usize>, default: usize) -> usize {
    requested.unwrap_or(default).clamp(1, MAX_SAMPLE_COUNT)
}

/// Clamp a requested per-bag cap into `[1, MAX_TAP_RESPONSE_BAG_BYTES]`,
/// defaulting when the caller left it unset.
///
/// Clamping to the whole-response budget is what makes the first bag unable to
/// exceed it, so the collection loop needs no special case for one.
fn bounded_tap_bag_bytes(requested: Option<usize>) -> usize {
    requested
        .unwrap_or(DEFAULT_MAX_TAP_BAG_BYTES)
        .clamp(1, MAX_TAP_RESPONSE_BAG_BYTES)
}

/// Render one raw tap bag as JSON: the bag's full byte length plus the hex of
/// however much of it the caller's cap admitted (raw bags are wire-neutral
/// bytes; decoding is the caller's concern).
///
/// Takes the already-clamped slice rather than clamping again, because the
/// collection loop must charge its budget the same number this encodes — two
/// sites computing one rule is two sites that can disagree.
fn tap_bag_json(encoded: &[u8], full_byte_len: usize) -> Value {
    json!({
        "byte_len": full_byte_len,
        "hex_preview": hex_encode(encoded),
        "hex_truncated": encoded.len() < full_byte_len,
    })
}

fn event_json(event: &Event) -> Value {
    json!({
        "topic": event.topic(),
        "name": event.log_name(),
        "event": serde_json::to_value(event).unwrap_or(Value::Null),
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    // A nibble table rather than `write!` per byte: this encodes up to
    // `MAX_TAP_RESPONSE_BAG_BYTES` on a tokio worker, where `core::fmt`'s
    // per-byte machinery costs several times what a two-push loop does.
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = Vec::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push(HEX_DIGITS[usize::from(byte >> 4)]);
        hex.push(HEX_DIGITS[usize::from(byte & 0x0f)]);
    }
    String::from_utf8(hex).expect("a hex-digit table only ever yields ASCII")
}

/// Forwards runtime events into the `logs` tool's bounded collection channel,
/// mirroring the REST WebSocket event forwarder.
struct McpEventForwarder {
    tx: tokio::sync::mpsc::UnboundedSender<Event>,
}

impl EventListener for McpEventForwarder {
    fn on_event(&mut self, event: &Event) -> streamlib::sdk::error::Result<()> {
        let _ = self.tx.send(event.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! MCP wire tests: an `rmcp` client drives the real `/mcp` endpoint that
    //! [`crate::handlers::build_router`] wires in, served on a real local API
    //! socket, through discovery, the tool catalog, and each tool through to
    //! the runtime. The router is the real one; only the `RuntimeOperations`
    //! backend is a stub, so the MCP → runtime seam is what's under test.
    //!
    //! The catalog assertions are two-sided on purpose — what is advertised,
    //! and what must never be again.

    use crate::control_plane_stub_support::{
        STUB_EXCHANGED_FRAME_SURFACE_ID, STUB_EXCHANGED_FRAME_SURFACE_ID_PERCENT_ENCODED,
        STUB_EXCHANGED_IMAGE_BYTES, STUB_SOURCE_SURFACE_EXTENT, StubSurfaceExchange,
    };
    use base64::Engine as _;
    use rmcp::RoleClient;
    use rmcp::model::{
        CallToolRequestParams, ClientCapabilities, GetPromptRequestParams,
        ReadResourceRequestParams, RequestMetaObject,
    };
    use rmcp::service::{
        ClientInitializeError, ClientLifecycleMode, ClientServiceExt, RunningService, ServiceError,
        SubscriptionEnd,
    };
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    use rmcp::transport::{StreamableHttpClientTransport, UnixSocketHttpClient};
    use streamlib::sdk::error::{Error, Result};
    use streamlib::sdk::runtime::{BoxFuture, RuntimeOperations, TapSubscription};

    use super::*;

    /// How the stub's `tap_async` answers: either it refuses (no channel), or it
    /// hands back a synthetic [`TapSubscription`] pre-loaded with `bags` and a
    /// fixed `dropped_bags` count. `keep_sender_open` retains the forward
    /// sender so `recv()` pends after the bags drain — modelling a quiet channel
    /// so the tap tool's monotonic sample window is what ends the collection.
    #[derive(Clone)]
    struct StubTapPlan {
        bags: Vec<Vec<u8>>,
        dropped_bags: u64,
        keep_sender_open: bool,
        sender_keepalive: Arc<Mutex<Option<tokio::sync::mpsc::Sender<Vec<u8>>>>>,
    }

    /// Stub runtime answering the observation ops without a live engine, and
    /// recording every shutdown reason so a dispatch test can confirm the tool
    /// reached the matching runtime op.
    ///
    /// Every graph-mutating op records what it was handed and answers a fixed
    /// id, so a mutation tool's test asserts the op it reached and the
    /// arguments it carried.
    struct ControlPlaneMcpDispatchStubRuntime {
        exported_graph: Arc<Mutex<Value>>,
        tap_plan: Option<StubTapPlan>,
        recorded_shutdown_reasons: Arc<Mutex<Vec<String>>>,
        recorded_graph_mutations: crate::control_plane_stub_support::RecordedGraphMutations,
        armed_add_processor_refusal: crate::control_plane_stub_support::ArmedAddProcessorRefusal,
        exchange: StubSurfaceExchange,
    }

    impl ControlPlaneMcpDispatchStubRuntime {
        fn new() -> Self {
            Self {
                exported_graph: Arc::new(Mutex::new(json!({ "nodes": [], "links": [] }))),
                tap_plan: None,
                recorded_shutdown_reasons: Arc::new(Mutex::new(Vec::new())),
                recorded_graph_mutations: Arc::new(Mutex::new(Vec::new())),
                armed_add_processor_refusal: Arc::new(Mutex::new(None)),
                exchange: StubSurfaceExchange::default(),
            }
        }

        /// A stub whose `add_node` refuses with `refusal`, standing in for an
        /// engine-side refusal the front end has to carry back.
        fn refusing_every_add_node(refusal: &str) -> Self {
            Self {
                armed_add_processor_refusal: Arc::new(Mutex::new(Some(refusal.to_string()))),
                ..Self::new()
            }
        }

        /// A stub whose `tap_async` yields a synthetic subscription over `bags`
        /// with the given dropped-bag count, dropping the forward sender once the
        /// bags are queued so `recv()` ends (exhaustion path).
        fn with_tap_bags(bags: Vec<Vec<u8>>, dropped_bags: u64) -> Self {
            Self {
                tap_plan: Some(StubTapPlan {
                    bags,
                    dropped_bags,
                    keep_sender_open: false,
                    sender_keepalive: Arc::new(Mutex::new(None)),
                }),
                ..Self::new()
            }
        }

        /// A stub whose `tap_async` yields a subscription over `bags` but keeps
        /// the forward sender alive, so `recv()` pends after the bags drain — a
        /// quiet channel whose collection ends on the monotonic sample window.
        fn with_quiet_tap(bags: Vec<Vec<u8>>) -> Self {
            Self {
                tap_plan: Some(StubTapPlan {
                    bags,
                    dropped_bags: 0,
                    keep_sender_open: true,
                    sender_keepalive: Arc::new(Mutex::new(None)),
                }),
                ..Self::new()
            }
        }
    }

    impl RuntimeOperations for ControlPlaneMcpDispatchStubRuntime {
        fn to_json_async(&self) -> BoxFuture<'_, Result<Value>> {
            let exported_graph = self.exported_graph.lock().clone();
            Box::pin(async move { Ok(exported_graph) })
        }
        fn tap_async(
            &self,
            channel: String,
            _count: Option<usize>,
        ) -> BoxFuture<'_, Result<TapSubscription>> {
            let Some(plan) = self.tap_plan.clone() else {
                return Box::pin(async move { Err(Error::TapChannelNotFound(channel)) });
            };
            Box::pin(async move {
                let (sender, receiver) =
                    tokio::sync::mpsc::channel::<Vec<u8>>(plan.bags.len().max(1));
                for bag in &plan.bags {
                    sender.send(bag.clone()).await.expect("stub tap queue send");
                }
                if plan.keep_sender_open {
                    *plan.sender_keepalive.lock() = Some(sender);
                }
                Ok(TapSubscription::from_forward_channel(
                    channel,
                    receiver,
                    plan.dropped_bags,
                ))
            })
        }
        crate::control_plane_stub_support::graph_mutation_ops_record_the_call!();
        crate::control_plane_stub_support::surface_exchange_op_answers_the_stub!();
        fn request_runtime_shutdown(&self, reason: &str) -> Result<()> {
            self.recorded_shutdown_reasons
                .lock()
                .push(reason.to_string());
            Ok(())
        }
        fn to_json(&self) -> Result<Value> {
            Ok(json!({}))
        }
    }

    /// The control vocabulary. This is the whole of it —
    /// `tools/list` is asserted equal to this, not merely a superset.
    const CONTROL_TOOL_NAMES: &[&str] = &[
        "graph",
        "tap",
        "logs",
        "exchange",
        "shutdown",
        "add_node",
        "remove_node",
        "connect",
        "disconnect",
    ];

    /// The authority the MCP client names in `Host`; the socket path is the
    /// address.
    const LOCAL_API_MCP_URI: &str = "http://localhost/mcp";

    /// The real router over a runtime, served on a local API socket in a fresh
    /// temp directory for as long as this lives.
    struct LocalApiServedForMcpTests {
        _socket_directory: tempfile::TempDir,
        running_server: Option<crate::local_api_socket::RunningLocalApiSocketServer>,
        local_api_socket_path: std::path::PathBuf,
    }

    impl LocalApiServedForMcpTests {
        fn stop_serving(&mut self) {
            drop(self.running_server.take());
        }
    }

    fn serve_the_local_api_over(runtime: Arc<dyn RuntimeOperations>) -> LocalApiServedForMcpTests {
        let socket_directory = tempfile::tempdir().unwrap();
        let local_api_socket_path = socket_directory.path().join("local-api.sock");
        let local_api_stopping = CancellationToken::new();
        let running_server = crate::local_api_socket::serve_router_on_local_api_socket(
            crate::handlers::build_router(runtime, local_api_stopping.clone()),
            local_api_stopping,
            &tokio::runtime::Handle::current(),
            &local_api_socket_path,
        )
        .unwrap();
        LocalApiServedForMcpTests {
            _socket_directory: socket_directory,
            running_server: Some(running_server),
            local_api_socket_path,
        }
    }

    fn mcp_transport_to(
        served: &LocalApiServedForMcpTests,
    ) -> StreamableHttpClientTransport<UnixSocketHttpClient> {
        StreamableHttpClientTransport::with_client(
            UnixSocketHttpClient::new(
                served.local_api_socket_path.to_str().unwrap(),
                LOCAL_API_MCP_URI,
            ),
            StreamableHttpClientTransportConfig::with_uri(LOCAL_API_MCP_URI),
        )
    }

    /// An `rmcp` client at the latest revision, connected to the real router
    /// over `runtime`.
    async fn connected_mcp_client(
        runtime: Arc<dyn RuntimeOperations>,
    ) -> (LocalApiServedForMcpTests, RunningService<RoleClient, ()>) {
        let served = serve_the_local_api_over(runtime);
        let client = ()
            .serve_with_lifecycle(
                mcp_transport_to(&served),
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::LATEST],
                },
            )
            .await
            .expect("the node answers `server/discover` at the latest revision");
        (served, client)
    }

    /// One `tools/call`: the result as it crossed the wire, or the protocol
    /// error that refused it.
    async fn tool_call_outcome(
        runtime: Arc<dyn RuntimeOperations>,
        tool_name: &str,
        arguments: Value,
    ) -> std::result::Result<Value, McpError> {
        let (_served, client) = connected_mcp_client(runtime).await;
        let request = CallToolRequestParams::new(tool_name.to_string())
            .with_arguments(arguments.as_object().cloned().unwrap_or_default());
        match client.call_tool(request).await {
            Ok(result) => Ok(serde_json::to_value(result).unwrap()),
            Err(ServiceError::McpError(refusal)) => Err(refusal),
            Err(other) => panic!("`{tool_name}` failed below the protocol: {other}"),
        }
    }

    async fn tool_call_result(
        runtime: Arc<dyn RuntimeOperations>,
        tool_name: &str,
        arguments: Value,
    ) -> Value {
        tool_call_outcome(runtime, tool_name, arguments)
            .await
            .unwrap_or_else(|refusal| panic!("`{tool_name}` was refused: {refusal:?}"))
    }

    async fn tool_call_refusal(
        runtime: Arc<dyn RuntimeOperations>,
        tool_name: &str,
        arguments: Value,
    ) -> McpError {
        match tool_call_outcome(runtime, tool_name, arguments).await {
            Ok(result) => panic!("`{tool_name}` answered a result: {result}"),
            Err(refusal) => refusal,
        }
    }

    async fn listed_tools(runtime: Arc<dyn RuntimeOperations>) -> Vec<Value> {
        let (_served, client) = connected_mcp_client(runtime).await;
        client
            .list_all_tools()
            .await
            .unwrap()
            .into_iter()
            .map(|tool| serde_json::to_value(tool).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn discover_answers_the_latest_revision_alone_with_the_tools_resources_and_prompts_capabilities()
     {
        let (_served, client) =
            connected_mcp_client(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;

        let discovered = client
            .discover(RequestMetaObject::with_client_context(
                ProtocolVersion::LATEST,
                Implementation::new("discover-test", "0"),
                ClientCapabilities::default(),
            ))
            .await
            .unwrap();

        assert_eq!(discovered.supported_versions, [ProtocolVersion::LATEST]);
        assert_eq!(
            discovered.server_info().map(|server_info| server_info.name),
            Some(MCP_SERVER_NAME.to_string())
        );
        assert!(discovered.capabilities.tools.is_some(), "{discovered:?}");
        assert!(
            discovered.capabilities.resources.is_some(),
            "{discovered:?}"
        );
        assert!(discovered.capabilities.prompts.is_some(), "{discovered:?}");
        assert_eq!(
            discovered.instructions.as_deref(),
            Some(LOCAL_API_MCP_SERVER_INSTRUCTIONS)
        );
    }

    /// A client that knows the handshake revisions only.
    struct HandshakeRevisionMcpClient {
        handshake_protocol_version: ProtocolVersion,
    }

    impl rmcp::ClientHandler for HandshakeRevisionMcpClient {
        fn get_info(&self) -> rmcp::model::ClientConfig {
            rmcp::model::ClientConfig::new(
                ClientCapabilities::default(),
                Implementation::new("handshake-test", "0"),
            )
            .with_protocol_version(self.handshake_protocol_version.clone())
        }
    }

    #[tokio::test]
    async fn an_initialize_handshake_is_refused_with_the_unsupported_version_error_naming_the_latest()
     {
        for handshake_protocol_version in [
            ProtocolVersion::V_2025_06_18,
            ProtocolVersion::LATEST_WITH_INITIALIZE,
        ] {
            let served =
                serve_the_local_api_over(Arc::new(ControlPlaneMcpDispatchStubRuntime::new()));
            let refusal = HandshakeRevisionMcpClient {
                handshake_protocol_version: handshake_protocol_version.clone(),
            }
            .serve_with_lifecycle(mcp_transport_to(&served), ClientLifecycleMode::Initialize)
            .await
            .err()
            .unwrap_or_else(|| panic!("{handshake_protocol_version} must not be served"));

            let ClientInitializeError::JsonRpcError(refusal) = refusal else {
                panic!("{handshake_protocol_version}: expected a JSON-RPC refusal, got {refusal}");
            };
            assert_eq!(
                refusal.code,
                rmcp::model::ErrorCode::UNSUPPORTED_PROTOCOL_VERSION
            );
            let data = refusal
                .data
                .expect("the refusal names the served revisions");
            assert_eq!(
                data["supported"],
                json!([ProtocolVersion::LATEST]),
                "{data}"
            );
            assert_eq!(
                data["requested"],
                json!(handshake_protocol_version),
                "{data}"
            );
        }
    }

    /// A host that holds `subscriptions/listen` open must not hold the node's
    /// shutdown: stopping the local API ends the stream with its final result.
    #[tokio::test]
    async fn a_listen_stream_ends_gracefully_when_the_local_api_stops_serving() {
        let (mut served, client) =
            connected_mcp_client(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;
        let mut subscription = client
            .listen(SubscriptionFilter::default())
            .await
            .expect("the node acknowledges a listen");
        assert_eq!(
            serde_json::to_value(subscription.acknowledged()).unwrap(),
            json!({}),
            "no list ever changes under a host, so nothing is subscribed"
        );

        served.stop_serving();

        let ended = tokio::time::timeout(Duration::from_secs(5), subscription.next())
            .await
            .expect("the stream ends once the local API stops");
        assert!(matches!(ended, Ok(None)), "{ended:?}");
        assert!(
            matches!(subscription.end(), Some(SubscriptionEnd::Graceful(_))),
            "the stream closes with the listen request's own final result"
        );
    }

    #[tokio::test]
    async fn tools_list_advertises_exactly_the_control_vocabulary() {
        let tools = listed_tools(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;
        let mut names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        names.sort_unstable();
        let mut control_tool_names = CONTROL_TOOL_NAMES.to_vec();
        control_tool_names.sort_unstable();

        // Exact, not a superset: the catalog IS the control vocabulary, so a
        // tool appearing here that is not in this list is a surface the plan
        // does not grant.
        assert_eq!(
            names, control_tool_names,
            "tools/list must advertise exactly the control vocabulary"
        );
        for tool in tools {
            assert_eq!(
                tool["inputSchema"]["type"], "object",
                "tool `{}` must declare an object inputSchema",
                tool["name"]
            );
        }
    }

    #[tokio::test]
    async fn tools_call_add_node_reaches_the_runtime_op_with_the_spec() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let body = tool_call_result(
            runtime,
            "add_node",
            json!({
                "type": "processors.grayscale_effect:GrayscaleEffect",
                "config": { "strength": 0.5 },
                "name": "Gray"
            }),
        )
        .await;
        assert_eq!(body["isError"], false, "body={body}");
        let text = body["content"][0]["text"].as_str().unwrap();
        let stated: Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            stated,
            json!({ "name": "gray" }),
            "the result is the name the node received, which `connect` and `remove_node` take"
        );
        let recorded = recorded.lock();
        let [crate::control_plane_stub_support::RecordedGraphMutation::AddProcessor(spec)] =
            &recorded[..]
        else {
            panic!("add_node must reach exactly one add op, recorded {recorded:?}");
        };
        assert_eq!(
            spec.name.as_str(),
            "processors.grayscale_effect:GrayscaleEffect"
        );
        assert_eq!(spec.config["strength"], 0.5);
        assert_eq!(spec.display_name.as_deref(), Some("Gray"));
    }

    /// What this locks is the door: an engine that refuses an add — a name
    /// already taken, an unknown class — reaches the MCP caller as a tool error
    /// carrying its own words, rather than as a success or a swallowed reason.
    #[tokio::test]
    async fn tools_call_add_node_carries_back_an_engine_refusal_in_its_own_words() {
        let refusal = "the node name `gray` is already taken in this graph";
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::refusing_every_add_node(
            refusal,
        ));

        let body = tool_call_result(
            runtime,
            "add_node",
            json!({
                "type": "processors.grayscale_effect:GrayscaleEffect",
                "name": "gray"
            }),
        )
        .await;
        assert_eq!(body["isError"], true, "body={body}");
        let text = body["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("add_node failed"), "{text}");
        assert!(
            text.contains(refusal),
            "the refusal must reach the caller in the engine's own words: {text}"
        );
    }

    #[tokio::test]
    async fn tools_call_add_node_without_config_or_name_sends_an_empty_object_and_answers_the_engines_name()
     {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let body = tool_call_result(
            runtime,
            "add_node",
            json!({ "type": "streamlib:CameraSource" }),
        )
        .await;

        assert_eq!(body["isError"], false, "body={body}");
        let stated: Value =
            serde_json::from_str(body["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            stated["name"],
            crate::control_plane_stub_support::STUB_ADDED_NODE_NAME,
            "an unnamed node is answered with the name the engine gave it"
        );
        let recorded = recorded.lock();
        let [crate::control_plane_stub_support::RecordedGraphMutation::AddProcessor(spec)] =
            &recorded[..]
        else {
            panic!("expected one add op, recorded {recorded:?}");
        };
        assert_eq!(spec.config, json!({}));
        assert_eq!(spec.display_name, None);
    }

    /// Mental-revert: read unknown fields past and `display_name` is dropped,
    /// so the node takes the class's short name while the caller believes it
    /// named it.
    #[tokio::test]
    async fn tools_call_add_node_refuses_an_argument_it_does_not_take() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let body = tool_call_result(
            runtime,
            "add_node",
            json!({
                "type": "processors.grayscale_effect:GrayscaleEffect",
                "display_name": "gray"
            }),
        )
        .await;

        let text = body["content"][0]["text"].as_str().unwrap();
        assert_eq!(body["isError"], true, "{text}");
        assert!(text.contains("display_name"), "{text}");
        assert!(recorded.lock().is_empty(), "nothing was added");
    }

    /// The address `connect` hands the engine for a port on this node.
    #[tokio::test]
    async fn tools_call_connect_and_disconnect_reach_their_runtime_ops() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let connect_body = tool_call_result(
            runtime.clone(),
            "connect",
            json!({
                "from_node": "camera", "from_port": "video",
                "to_node": "fx", "to_port": "video_from_upstream"
            }),
        )
        .await;
        assert_eq!(connect_body["isError"], false, "body={connect_body}");
        let stated: Value =
            serde_json::from_str(connect_body["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            stated["link_id"],
            crate::control_plane_stub_support::STUB_CREATED_LINK_ID
        );

        let disconnect_body =
            tool_call_result(runtime, "disconnect", json!({ "link_id": "link-9" })).await;
        assert_eq!(disconnect_body["isError"], false, "body={disconnect_body}");

        let recorded = recorded.lock();
        let [
            crate::control_plane_stub_support::RecordedGraphMutation::Connect(from, to),
            crate::control_plane_stub_support::RecordedGraphMutation::Disconnect(link_id),
        ] = &recorded[..]
        else {
            panic!("expected a connect then a disconnect, recorded {recorded:?}");
        };
        // Each end is the node the tool looked up by name.
        assert_eq!(from.processor_id().as_str(), "camera-id");
        assert_eq!(from.port_name(), "video");
        assert_eq!(to.processor_id().as_str(), "fx-id");
        assert_eq!(to.port_name(), "video_from_upstream");
        assert_eq!(link_id.as_str(), "link-9");
    }

    /// Each end's node is looked up by its name cast, so a caller that typed a
    /// name the way it reads still reaches the node `graph` lists; the port
    /// names reach the engine as given, and the engine casts them.
    #[tokio::test]
    async fn tools_call_connect_resolves_each_ends_node_by_its_cast_name() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let body = call_the_connect_tool(
            runtime,
            json!({
                "from_node": "Front Camera", "from_port": "Video",
                "to_node": "Gray FX", "to_port": "video_from_upstream",
            }),
        )
        .await;
        assert_eq!(body["isError"], false, "body={body}");

        let recorded = recorded.lock();
        let [crate::control_plane_stub_support::RecordedGraphMutation::Connect(from, to)] =
            &recorded[..]
        else {
            panic!("expected one connect op, recorded {recorded:?}");
        };
        assert_eq!(from.processor_id().as_str(), "front-camera-id");
        assert_eq!(from.port_name(), "Video");
        assert_eq!(to.processor_id().as_str(), "gray-fx-id");
        assert_eq!(to.port_name(), "video_from_upstream");
    }

    /// Call `connect` with `arguments` and hand back the whole tool result.
    async fn call_the_connect_tool(
        runtime: Arc<ControlPlaneMcpDispatchStubRuntime>,
        arguments: Value,
    ) -> Value {
        tool_call_result(runtime, "connect", arguments).await
    }

    /// Call `disconnect` with `arguments` and hand back the whole tool result.
    async fn call_the_disconnect_tool(
        runtime: Arc<ControlPlaneMcpDispatchStubRuntime>,
        arguments: Value,
    ) -> Value {
        tool_call_result(runtime, "disconnect", arguments).await
    }

    /// Every end needs its node and its port, and a call missing one is
    /// refused naming the field rather than wired against a guess.
    #[tokio::test]
    async fn tools_call_connect_refuses_an_end_missing_its_node_or_port_naming_the_field() {
        for missing_field in ["from_node", "from_port", "to_node", "to_port"] {
            let mut arguments = json!({
                "from_node": "camera", "from_port": "video",
                "to_node": "fx", "to_port": "video_from_upstream",
            });
            arguments
                .as_object_mut()
                .unwrap()
                .remove(missing_field)
                .expect("the field this case removes");
            let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
            let recorded_calls = Arc::clone(&runtime.recorded_graph_mutations);
            let body = call_the_connect_tool(runtime, arguments.clone()).await;

            let text = body["content"][0]["text"].as_str().unwrap();
            assert_eq!(body["isError"], true, "{arguments} gave {text}");
            assert!(
                text.contains(missing_field),
                "{arguments} must be refused naming {missing_field}: {text}"
            );
            assert!(
                recorded_calls.lock().is_empty(),
                "a refused connect reaches no runtime op"
            );
        }
    }

    /// An end named by an argument `connect` does not take is refused naming
    /// it — the id and display-name spellings included, which a caller holding
    /// an older `graph` would still send.
    #[tokio::test]
    async fn tools_call_connect_refuses_an_argument_it_does_not_take_naming_it() {
        for unknown_field in [
            "from_processor_id",
            "from_processor_display_name",
            "to_processor_id",
            "to_processor_display_name",
        ] {
            let mut arguments = json!({
                "from_node": "camera", "from_port": "video",
                "to_node": "fx", "to_port": "video_from_upstream",
            });
            arguments[unknown_field] = json!("camera");
            let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
            let recorded_calls = Arc::clone(&runtime.recorded_graph_mutations);
            let body = call_the_connect_tool(runtime, arguments.clone()).await;

            let text = body["content"][0]["text"].as_str().unwrap();
            assert_eq!(body["isError"], true, "{arguments} gave {text}");
            assert!(
                text.contains(unknown_field),
                "{arguments} must be refused naming {unknown_field}: {text}"
            );
            assert!(
                recorded_calls.lock().is_empty(),
                "a refused connect reaches no runtime op"
            );
        }
    }

    /// A misspelled argument is refused rather than read past.
    #[tokio::test]
    async fn tools_call_connect_refuses_a_misspelled_argument_rather_than_reading_past_it() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded_calls = Arc::clone(&runtime.recorded_graph_mutations);

        let body = call_the_connect_tool(
            runtime,
            json!({
                "from_node": "camera",
                "from_port": "video",
                "to_node": "fx",
                "to_prot": "video",
            }),
        )
        .await;

        let text = body["content"][0]["text"].as_str().unwrap();
        assert_eq!(body["isError"], true, "{text}");
        assert!(text.contains("to_prot"), "{text}");
        assert!(recorded_calls.lock().is_empty(), "nothing was wired");
    }

    /// Both ends are nodes on this node's own graph, so a node it does not
    /// hold, at either end, is refused naming it before any runtime op is
    /// reached.
    #[tokio::test]
    async fn tools_call_connect_refuses_a_node_this_node_does_not_hold_at_either_end() {
        for arguments in [
            json!({
                "from_node": crate::control_plane_stub_support::STUB_ABSENT_NODE_NAME,
                "from_port": "video",
                "to_node": "fx", "to_port": "video",
            }),
            json!({
                "from_node": "camera", "from_port": "video",
                "to_node": crate::control_plane_stub_support::STUB_ABSENT_NODE_NAME,
                "to_port": "video",
            }),
        ] {
            let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
            let recorded_calls = Arc::clone(&runtime.recorded_graph_mutations);

            let body = call_the_connect_tool(runtime, arguments.clone()).await;

            let text = body["content"][0]["text"].as_str().unwrap();
            assert_eq!(body["isError"], true, "{arguments} gave {text}");
            assert!(text.contains("connect failed"), "{text}");
            assert!(
                text.contains(crate::control_plane_stub_support::STUB_ABSENT_NODE_NAME),
                "{text}"
            );
            assert!(recorded_calls.lock().is_empty(), "nothing was wired");
        }
    }

    /// `disconnect` takes a `link_id` and nothing else: a missing one, or any
    /// other argument, is refused naming it before any runtime op is reached.
    #[tokio::test]
    async fn tools_call_disconnect_takes_a_link_id_alone() {
        for (arguments, what_the_refusal_must_name) in [
            (json!({}), "link_id"),
            (
                json!({ "link_id": "link-9", "runtime_name": "studio-display-9f3c" }),
                "runtime_name",
            ),
        ] {
            let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
            let recorded_calls = Arc::clone(&runtime.recorded_graph_mutations);
            let body = call_the_disconnect_tool(runtime, arguments.clone()).await;

            let text = body["content"][0]["text"].as_str().unwrap();
            assert_eq!(body["isError"], true, "{arguments} gave {text}");
            assert!(
                text.contains(what_the_refusal_must_name),
                "{arguments} must be refused naming {what_the_refusal_must_name}: {text}"
            );
            assert!(recorded_calls.lock().is_empty());
        }
    }

    /// The result is the new link's id and how this node's own `graph` reads
    /// it, so an agent learns whether its change took without a second call.
    #[tokio::test]
    async fn tools_call_connect_states_the_link_id_and_the_links_state() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        *runtime.exported_graph.lock() = json!({
            "runtime_name": crate::control_plane_stub_support::STUB_RUNTIME_NAME,
            "links": [
                { "id": "some-other-link", "state": "wired" },
                { "id": crate::control_plane_stub_support::STUB_CREATED_LINK_ID,
                  "state": "pending" },
            ],
        });

        let body = call_the_connect_tool(
            runtime,
            json!({
                "from_node": "camerasource",
                "from_port": "video",
                "to_node": "fx",
                "to_port": "video_from_upstream",
            }),
        )
        .await;

        let stated: Value =
            serde_json::from_str(body["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            stated["link_id"],
            crate::control_plane_stub_support::STUB_CREATED_LINK_ID
        );
        assert_eq!(stated["state"], "pending");
        assert_eq!(
            stated.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["link_id", "state"]
        );
    }

    #[tokio::test]
    async fn tools_call_remove_node_resolves_the_name_and_reaches_the_runtime_op() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let body = tool_call_result(runtime, "remove_node", json!({ "name": "FX" })).await;
        assert_eq!(body["isError"], false, "body={body}");
        let stated: Value =
            serde_json::from_str(body["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            stated,
            json!({ "removed_name": "fx" }),
            "the result names the node as `graph` listed it"
        );

        let recorded = recorded.lock();
        let [crate::control_plane_stub_support::RecordedGraphMutation::RemoveProcessor(id)] =
            &recorded[..]
        else {
            panic!("expected one remove op, recorded {recorded:?}");
        };
        assert_eq!(
            id.as_str(),
            format!(
                "fx{}",
                crate::control_plane_stub_support::STUB_NODE_ID_SUFFIX
            ),
            "the op is handed the id the runtime resolved the name to"
        );
    }

    #[tokio::test]
    async fn tools_call_remove_node_naming_an_absent_node_is_refused_naming_it() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let body = tool_call_result(
            runtime,
            "remove_node",
            json!({
                "name": crate::control_plane_stub_support::STUB_ABSENT_NODE_NAME
            }),
        )
        .await;
        assert_eq!(body["isError"], true, "body={body}");
        let text = body["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("remove_node failed"), "{text}");
        assert!(
            text.contains(crate::control_plane_stub_support::STUB_ABSENT_NODE_NAME),
            "the refusal must name the node asked for: {text}"
        );
        assert!(recorded.lock().is_empty(), "nothing was removed");
    }

    #[tokio::test]
    async fn a_mutation_tool_with_malformed_arguments_is_a_tool_error_that_reaches_no_op() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let body = tool_call_result(runtime, "connect", json!({ "from_node": "camera" })).await;
        assert_eq!(body["isError"], true, "body={body}");
        assert!(
            recorded.lock().is_empty(),
            "a refused call must reach no runtime op"
        );
    }

    #[tokio::test]
    async fn tools_call_graph_returns_the_runtime_json() {
        let body = tool_call_result(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "graph",
            json!({}),
        )
        .await;
        assert_eq!(body["isError"], false);
        let text = body["content"][0]["text"].as_str().unwrap();
        let graph: Value = serde_json::from_str(text).unwrap();
        assert!(graph["nodes"].is_array());
    }

    #[tokio::test]
    async fn tools_call_unknown_tool_is_refused_as_invalid_params() {
        let refusal = tool_call_refusal(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "does_not_exist",
            json!({}),
        )
        .await;
        assert_eq!(refusal.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    /// One 1024-sample stereo `f32` `AudioBlock`, msgpack-framed — the exact
    /// size this rig's PipeWire arm publishes.
    const AUDIO_BLOCK_BAG_BYTES: usize = 8366;

    async fn tap_sample_from(
        runtime: Arc<ControlPlaneMcpDispatchStubRuntime>,
        arguments: Value,
    ) -> Value {
        let body = tool_call_result(runtime, "tap", arguments).await;
        assert_eq!(body["isError"], false, "body={body}");
        serde_json::from_str(body["content"][0]["text"].as_str().unwrap())
            .expect("tap result text is JSON")
    }

    /// A bag is a msgpack map, so a decoder needs all of it or none — which
    /// makes the default cap load-bearing for every data model that rides its
    /// payload inline, audio first among them.
    ///
    /// Mental revert: set the default cap under 8 366 and this reddens.
    #[tokio::test]
    async fn tools_call_tap_carries_a_whole_audio_block_without_being_asked_to() {
        let audio_bag = vec![0xABu8; AUDIO_BLOCK_BAG_BYTES];
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::with_tap_bags(
            vec![audio_bag],
            0,
        ));

        let sample = tap_sample_from(runtime, json!({ "channel": "mic/audio" })).await;

        let bag = &sample["bags"][0];
        assert_eq!(
            bag["byte_len"].as_u64().unwrap(),
            AUDIO_BLOCK_BAG_BYTES as u64
        );
        assert_eq!(
            bag["hex_truncated"], false,
            "an audio block must arrive whole with no caller-supplied cap"
        );
        assert_eq!(
            bag["hex_preview"].as_str().unwrap().len(),
            AUDIO_BLOCK_BAG_BYTES * 2,
            "the hex is the whole bag, so a consumer can decode it"
        );
    }

    /// The bound the response budget's doc claims, held where it was false: a
    /// caller cannot name a per-bag cap above the whole-response budget, so one
    /// bag can never exceed it and the loop needs no exemption for the first.
    ///
    /// Mental revert: clamp `bounded_tap_bag_bytes` to anything larger and this
    /// reddens — which is what a 64 MiB ceiling against a 16 MiB budget did.
    #[test]
    fn a_per_bag_cap_can_never_be_named_above_the_whole_response_budget() {
        assert_eq!(
            bounded_tap_bag_bytes(Some(MAX_TAP_RESPONSE_BAG_BYTES * 4)),
            MAX_TAP_RESPONSE_BAG_BYTES
        );
        assert_eq!(bounded_tap_bag_bytes(Some(0)), 1);
        assert_eq!(bounded_tap_bag_bytes(None), DEFAULT_MAX_TAP_BAG_BYTES);
    }

    /// A single bag larger than any cap could admit still comes back — trimmed
    /// and flagged — rather than the sample being empty. This is the case the
    /// removed first-bag exemption used to serve, and it now holds because the
    /// cap cannot exceed the budget rather than because of a special case.
    #[tokio::test]
    async fn one_bag_larger_than_the_budget_is_returned_trimmed_rather_than_withheld() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::with_tap_bags(
            vec![vec![0x5Au8; MAX_TAP_RESPONSE_BAG_BYTES + 4096]],
            0,
        ));

        let sample = tap_sample_from(
            runtime,
            json!({ "channel": "big/bag", "max_bag_bytes": MAX_TAP_RESPONSE_BAG_BYTES }),
        )
        .await;

        assert_eq!(sample["received"], 1, "the sample is not empty");
        assert_eq!(sample["bags_withheld_at_byte_budget"], 0);
        assert_eq!(sample["bags"][0]["hex_truncated"], true);
        assert_eq!(
            sample["bags"][0]["byte_len"].as_u64().unwrap(),
            (MAX_TAP_RESPONSE_BAG_BYTES + 4096) as u64,
            "the bag's true size is reported however much of it was encoded"
        );
    }

    /// A bag over the cap is still reported and still flagged — the escape
    /// hatch is naming a bigger cap, not guessing at half a value.
    #[tokio::test]
    async fn tools_call_tap_honours_a_caller_named_per_bag_cap() {
        let bag_bytes = DEFAULT_MAX_TAP_BAG_BYTES + 4096;
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::with_tap_bags(
            vec![vec![0xCDu8; bag_bytes]],
            0,
        ));

        let trimmed = tap_sample_from(Arc::clone(&runtime), json!({ "channel": "big/bag" })).await;
        assert_eq!(trimmed["bags"][0]["hex_truncated"], true);
        assert_eq!(trimmed["max_bag_bytes"], DEFAULT_MAX_TAP_BAG_BYTES);

        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::with_tap_bags(
            vec![vec![0xCDu8; bag_bytes]],
            0,
        ));
        let whole = tap_sample_from(
            runtime,
            json!({ "channel": "big/bag", "max_bag_bytes": bag_bytes }),
        )
        .await;
        assert_eq!(
            whole["bags"][0]["hex_truncated"], false,
            "a caller who names a cap big enough gets the bag whole"
        );
    }

    /// `count` times the per-bag cap multiplies to gigabytes at the extremes,
    /// so the response carries its own budget. It stops the sample rather than
    /// trimming, because whole bags are the only ones worth returning.
    #[tokio::test]
    async fn tools_call_tap_stops_at_its_response_budget_rather_than_trimming() {
        let one_mib_bag = vec![0xEFu8; DEFAULT_MAX_TAP_BAG_BYTES];
        let bags_that_exceed_the_budget =
            MAX_TAP_RESPONSE_BAG_BYTES / DEFAULT_MAX_TAP_BAG_BYTES + 4;
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::with_tap_bags(
            vec![one_mib_bag; bags_that_exceed_the_budget],
            0,
        ));

        let sample = tap_sample_from(
            runtime,
            json!({ "channel": "big/bag", "count": bags_that_exceed_the_budget }),
        )
        .await;

        assert_eq!(
            sample["bags_withheld_at_byte_budget"], 1,
            "the bag the budget refused was received, so it is counted rather \
             than leaving `received` short for no stated reason"
        );
        let bags = sample["bags"].as_array().expect("bags array");
        assert!(
            bags.len() < bags_that_exceed_the_budget,
            "the budget must stop the sample short of what was asked for"
        );
        assert!(
            bags.iter().all(|bag| bag["hex_truncated"] == false),
            "every bag the budget did admit is whole"
        );
    }

    #[tokio::test]
    async fn tools_call_tap_shapes_bags_and_reports_dropped_count() {
        let big_bag = vec![0xABu8; DEFAULT_MAX_TAP_BAG_BYTES + 512];
        // Spans both nibble halves and every digit class, so the hex table is
        // pinned for case and for the six letters — `010203` alone leaves an
        // uppercase or transposed table green.
        let small_bag = vec![0x01u8, 0x02, 0x03, 0xAB, 0xCD, 0xEF];
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::with_tap_bags(
            vec![big_bag.clone(), small_bag.clone()],
            7,
        ));

        let body = tool_call_result(runtime, "tap", json!({ "channel": "cam/frame" })).await;
        let result = &body;
        assert_eq!(result["isError"], false, "body={body}");
        let text = result["content"][0]["text"].as_str().unwrap();
        let sample: Value = serde_json::from_str(text).expect("tap result text is JSON");

        assert_eq!(sample["channel"], "cam/frame");
        assert_eq!(sample["received"], 2);
        assert_eq!(sample["dropped_bags"], 7);
        assert!(sample["window_ms"].as_u64().unwrap() > 0);

        let bags = sample["bags"].as_array().expect("bags array");
        // Big bag: the full byte length is reported, the hex stops at the
        // per-bag cap, and truncation is flagged.
        assert_eq!(
            bags[0]["byte_len"].as_u64().unwrap(),
            (DEFAULT_MAX_TAP_BAG_BYTES + 512) as u64
        );
        assert_eq!(bags[0]["hex_truncated"], true);
        assert_eq!(
            bags[0]["hex_preview"].as_str().unwrap().len(),
            DEFAULT_MAX_TAP_BAG_BYTES * 2,
            "the hex is exactly the first DEFAULT_MAX_TAP_BAG_BYTES bytes"
        );
        // Small bag: previewed whole, not truncated.
        assert_eq!(bags[1]["byte_len"].as_u64().unwrap(), 6);
        assert_eq!(bags[1]["hex_truncated"], false);
        assert_eq!(bags[1]["hex_preview"], "010203abcdef");
    }

    #[tokio::test]
    async fn tools_call_tap_returns_partial_sample_within_window_on_quiet_channel() {
        // One bag flows, then the channel goes quiet (the forward sender is kept
        // open) so `recv()` pends; the request asks for four. Without the
        // monotonic sample window this tool call would block until three more
        // bags arrive — the hang this fix closes.
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::with_quiet_tap(vec![
            vec![0xAA, 0xBB],
        ]));

        let started = tokio::time::Instant::now();
        let body = tool_call_result(
            runtime,
            "tap",
            json!({ "channel": "cam/frame", "count": 4 }),
        )
        .await;
        let elapsed = started.elapsed();
        assert_eq!(body["isError"], false, "body={body}");
        let text = body["content"][0]["text"].as_str().unwrap();
        let sample: Value = serde_json::from_str(text).unwrap();
        assert_eq!(sample["requested"], 4);
        assert_eq!(
            sample["received"], 1,
            "a quiet channel returns the partial sample, not a full four"
        );
        assert!(
            elapsed < TAP_SAMPLE_WINDOW * 4,
            "tap must return within its sample window, not hang; took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn tools_call_logs_returns_bounded_window_sample() {
        // Tests beside this one publish on the one process-wide bus, so the
        // sample may fill before the window ends; either way the call returns
        // rather than hanging.
        let started = tokio::time::Instant::now();
        let body = tool_call_result(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "logs",
            json!({ "count": 4 }),
        )
        .await;
        let elapsed = started.elapsed();
        assert_eq!(body["isError"], false, "body={body}");
        let text = body["content"][0]["text"].as_str().unwrap();
        let sample: Value = serde_json::from_str(text).unwrap();
        assert_eq!(sample["requested"], 4);
        assert_eq!(
            sample["window_ms"].as_u64().unwrap(),
            LOGS_SAMPLE_WINDOW.as_millis() as u64
        );
        assert!(
            elapsed < LOGS_SAMPLE_WINDOW * 4,
            "logs must return within its sample window, not hang; took {elapsed:?}"
        );
    }

    /// Mental-revert: a bus that drops publishes until a runtime initializes
    /// it returns this sample empty.
    #[tokio::test]
    async fn tools_call_logs_samples_events_published_on_the_process_wide_bus() {
        let topic = "tools-call-logs-sampled-topic";
        let publisher = tokio::spawn(async move {
            loop {
                PUBSUB.publish(topic, &Event::custom(topic, json!({ "sampled": true })));
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });

        let body = tool_call_result(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "logs",
            json!({ "count": 4 }),
        )
        .await;
        publisher.abort();
        assert_eq!(body["isError"], false, "body={body}");
        let text = body["content"][0]["text"].as_str().unwrap();
        let sample: Value = serde_json::from_str(text).unwrap();
        assert_eq!(sample["received"], 4, "sample={sample}");
        assert!(
            sample["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["topic"] == topic),
            "an event published while logs sampled must be in the sample: {sample}"
        );
    }

    #[tokio::test]
    async fn tools_call_shutdown_reaches_the_runtime() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded_shutdowns = runtime.recorded_shutdown_reasons.clone();

        let body = tool_call_result(runtime, "shutdown", json!({ "reason": "agent asked" })).await;
        assert_eq!(body["isError"], false, "body={body}");
        let text = body["content"][0]["text"].as_str().unwrap();
        let outcome: Value = serde_json::from_str(text).unwrap();
        assert_eq!(outcome["status"], "RuntimeShutdownRequested");
        assert_eq!(outcome["reason"], "agent asked");
        assert_eq!(
            *recorded_shutdowns.lock(),
            vec!["agent asked".to_string()],
            "the tool must reach `request_runtime_shutdown` with the caller's reason"
        );
    }

    /// A malformed `shutdown` argument is an in-band tool error (`isError`),
    /// never a JSON-RPC error and never a silent shutdown — the agent has to
    /// see why its call did nothing.
    #[tokio::test]
    async fn tools_call_shutdown_with_malformed_arguments_is_an_in_band_tool_error() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded_shutdowns = runtime.recorded_shutdown_reasons.clone();

        let body = tool_call_result(runtime, "shutdown", json!({ "reason": 42 })).await;
        assert!(body.get("error").is_none(), "not a JSON-RPC error: {body}");
        assert_eq!(body["isError"], true, "body={body}");
        assert!(
            body["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .contains("42"),
            "the tool error must name the offending argument: {body}"
        );
        assert!(
            recorded_shutdowns.lock().is_empty(),
            "a malformed call must not reach the runtime"
        );
    }

    // ------------------------------------------------------------------
    // exchange: a published surface id in, an image content block out
    // ------------------------------------------------------------------

    fn exchange_stub(exchange: StubSurfaceExchange) -> Arc<ControlPlaneMcpDispatchStubRuntime> {
        Arc::new(ControlPlaneMcpDispatchStubRuntime {
            exchange,
            ..ControlPlaneMcpDispatchStubRuntime::new()
        })
    }

    /// Drive `tools/call` on `exchange` against a default stub, returning the
    /// `result` object and the `(surface id, cap)` pairs the tool handed the
    /// operation.
    async fn call_exchange_tool(arguments: Value) -> (Value, Vec<(String, Option<u32>)>) {
        let (body, calls) =
            call_exchange_tool_on(exchange_stub(StubSurfaceExchange::default()), arguments).await;
        (body.clone(), calls)
    }

    /// The same call against a stub the test chose, returning the whole
    /// JSON-RPC body — so a refusal test can assert it is an in-band tool
    /// error and not a JSON-RPC one.
    async fn call_exchange_tool_on(
        runtime: Arc<ControlPlaneMcpDispatchStubRuntime>,
        arguments: Value,
    ) -> (Value, Vec<(String, Option<u32>)>) {
        let recorded = runtime.exchange.recorded_calls.clone();
        let body = tool_call_result(runtime, "exchange", arguments).await;
        let calls = recorded.lock().clone();
        (body, calls)
    }

    fn content_block_of_type<'a>(result: &'a Value, block_type: &str) -> &'a Value {
        result["content"]
            .as_array()
            .unwrap_or_else(|| panic!("a content array; got {result}"))
            .iter()
            .find(|block| block["type"] == block_type)
            .unwrap_or_else(|| panic!("a `{block_type}` content block; got {result}"))
    }

    /// The whole point of the MCP spelling: the frame arrives *in the
    /// session* as a picture, not as a path or a URL the host would have to
    /// fetch — which is also why a host on another machine needs no shared
    /// filesystem.
    #[tokio::test]
    async fn tools_call_exchange_returns_the_frame_as_a_renderable_png_image_block() {
        let (result, _) =
            call_exchange_tool(json!({ "surface_id": STUB_EXCHANGED_FRAME_SURFACE_ID })).await;

        assert_eq!(result["isError"], false, "result={result}");
        let image = content_block_of_type(&result, "image");
        assert_eq!(image["mimeType"], "image/png");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(image["data"].as_str().expect("base64 image data"))
            .expect("the image block carries decodable base64");
        assert_eq!(
            decoded, STUB_EXCHANGED_IMAGE_BYTES,
            "the tool re-encodes nothing: the block is base64 of the operation's own bytes"
        );
    }

    /// A downscaled picture without its true extent is a measurement trap —
    /// so the text block states what the surface actually carries, which id
    /// it answered, and the one route that returns the exact bytes.
    #[tokio::test]
    async fn tools_call_exchange_states_the_true_extent_the_id_and_the_exact_bytes_route() {
        let (result, _) =
            call_exchange_tool(json!({ "surface_id": STUB_EXCHANGED_FRAME_SURFACE_ID })).await;

        let text = content_block_of_type(&result, "text")["text"]
            .as_str()
            .expect("text content block");
        let stated: Value = serde_json::from_str(text).expect("the text block is JSON");

        let (source_pixel_width, source_pixel_height) = STUB_SOURCE_SURFACE_EXTENT;
        assert_eq!(stated["surface_id"], STUB_EXCHANGED_FRAME_SURFACE_ID);
        assert_eq!(stated["source_surface_pixel_width"], source_pixel_width);
        assert_eq!(stated["source_surface_pixel_height"], source_pixel_height);
        assert_eq!(
            stated["encoded_image_pixel_width"], EXCHANGE_IMAGE_LONG_EDGE_PIXEL_CAP,
            "the inline image is the downscaled one, and says so"
        );
        assert_eq!(
            stated["encoded_image_pixel_height"],
            source_pixel_height * EXCHANGE_IMAGE_LONG_EDGE_PIXEL_CAP / source_pixel_width,
            "the short edge takes the cap's ratio, and stating it is what shows the two \
             extents differ at all"
        );
        assert_eq!(
            stated["exact_bytes_rest_route"],
            format!("GET /api/surfaces/{STUB_EXCHANGED_FRAME_SURFACE_ID_PERCENT_ENCODED}/image"),
            "the `#` of a frame id must be percent-encoded or the route names a fragment"
        );
    }

    /// The route the tool points at has to be the route the server serves;
    /// a drifting path would send an agent chasing a 404 for exact bytes.
    #[tokio::test]
    async fn the_exact_bytes_route_the_tool_names_is_the_route_the_spec_serves() {
        let (result, _) =
            call_exchange_tool(json!({ "surface_id": STUB_EXCHANGED_FRAME_SURFACE_ID })).await;
        let text = content_block_of_type(&result, "text")["text"]
            .as_str()
            .expect("text content block");
        let stated: Value = serde_json::from_str(text).unwrap();
        let named_route = stated["exact_bytes_rest_route"].as_str().unwrap();

        let served = crate::handlers::SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE.replace(
            "{surface_id}",
            STUB_EXCHANGED_FRAME_SURFACE_ID_PERCENT_ENCODED,
        );
        assert_eq!(named_route, format!("GET {served}"));
        assert!(
            crate::handlers::control_plane_openapi_spec()
                .paths
                .paths
                .contains_key(crate::handlers::SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE),
            "the route the tool names must exist in the served spec"
        );
    }

    /// Downscaled *by default*: a caller that names no cap still gets an
    /// image bounded to the declared ceiling, because a full-resolution
    /// frame inline is the one banned combination.
    #[tokio::test]
    async fn tools_call_exchange_applies_the_declared_cap_when_the_caller_names_none() {
        let (result, calls) =
            call_exchange_tool(json!({ "surface_id": STUB_EXCHANGED_FRAME_SURFACE_ID })).await;

        assert_eq!(result["isError"], false, "result={result}");
        assert_eq!(
            calls,
            vec![(
                STUB_EXCHANGED_FRAME_SURFACE_ID.to_string(),
                Some(EXCHANGE_IMAGE_LONG_EDGE_PIXEL_CAP)
            )],
            "the operation must never be handed `None` from this front end"
        );
    }

    #[tokio::test]
    async fn tools_call_exchange_honours_a_caller_cap_below_the_declared_ceiling() {
        let (_, calls) = call_exchange_tool(json!({
            "surface_id": STUB_EXCHANGED_FRAME_SURFACE_ID,
            "downscale_long_edge_pixel_cap": 512
        }))
        .await;

        assert_eq!(
            calls,
            vec![(STUB_EXCHANGED_FRAME_SURFACE_ID.to_string(), Some(512))]
        );
    }

    /// Full resolution lives on REST and nowhere else, so a cap above the
    /// declared ceiling is clamped rather than obeyed — an agent cannot ask
    /// its way into a payload its own API will refuse.
    #[tokio::test]
    async fn tools_call_exchange_clamps_a_caller_cap_above_the_declared_ceiling() {
        let (result, calls) = call_exchange_tool(json!({
            "surface_id": STUB_EXCHANGED_FRAME_SURFACE_ID,
            "downscale_long_edge_pixel_cap": 4096
        }))
        .await;

        assert_eq!(result["isError"], false, "result={result}");
        assert_eq!(
            calls,
            vec![(
                STUB_EXCHANGED_FRAME_SURFACE_ID.to_string(),
                Some(EXCHANGE_IMAGE_LONG_EDGE_PIXEL_CAP)
            )],
            "a larger cap is clamped to the ceiling, never passed through"
        );
    }

    /// A retired id is an in-band tool error naming the recycling, so the
    /// agent reads why its call returned no picture and taps a newer bag —
    /// never a JSON-RPC error, and never the slot's newer pixels.
    #[tokio::test]
    async fn tools_call_exchange_on_a_recycled_frame_is_a_tool_error_naming_the_recycling() {
        let (body, _) = call_exchange_tool_on(
            exchange_stub(StubSurfaceExchange::refusing_as_recycled(
                STUB_EXCHANGED_FRAME_SURFACE_ID,
            )),
            json!({ "surface_id": STUB_EXCHANGED_FRAME_SURFACE_ID }),
        )
        .await;

        assert!(body.get("error").is_none(), "not a JSON-RPC error: {body}");
        let result = &body;
        assert_eq!(result["isError"], true, "body={body}");
        let reported = result["content"][0]["text"].as_str().unwrap();
        assert!(
            reported.contains(STUB_EXCHANGED_FRAME_SURFACE_ID),
            "the refusal must name the id asked for: {reported}"
        );
        assert!(
            reported.contains("recycled"),
            "the refusal must say the frame was recycled: {reported}"
        );
        assert!(
            result["content"]
                .as_array()
                .is_some_and(|content| content.iter().all(|block| block["type"] == "text")),
            "a refusal carries no image block: {body}"
        );
    }

    /// Arguments the schema forbids never reach the runtime, and say why in
    /// band. The misspelled-key case is the one that would otherwise be
    /// silent: dropped, then answered with a differently-sized picture.
    #[tokio::test]
    async fn tools_call_exchange_with_arguments_the_schema_forbids_is_an_in_band_tool_error() {
        for (case, arguments, offending) in [
            (
                "no surface id",
                json!({ "downscale_long_edge_pixel_cap": 64 }),
                "surface_id",
            ),
            (
                "a cap of the wrong type",
                json!({ "surface_id": STUB_EXCHANGED_FRAME_SURFACE_ID, "downscale_long_edge_pixel_cap": "big" }),
                "big",
            ),
            (
                "a misspelled cap key",
                json!({ "surface_id": STUB_EXCHANGED_FRAME_SURFACE_ID, "downscal_long_edge_pixel_cap": 512 }),
                "downscal_long_edge_pixel_cap",
            ),
        ] {
            let (body, calls) =
                call_exchange_tool_on(exchange_stub(StubSurfaceExchange::default()), arguments)
                    .await;

            assert!(
                body.get("error").is_none(),
                "{case} must not be a JSON-RPC error: {body}"
            );
            assert_eq!(body["isError"], true, "{case}: {body}");
            assert!(
                body["content"][0]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(offending),
                "{case} must name `{offending}`: {body}"
            );
            assert!(
                calls.is_empty(),
                "{case} must not reach the runtime: {calls:?}"
            );
        }
    }

    /// Tap's arguments are its whole contract with a caller: no new
    /// argument, nothing renamed, nothing removed by a tool joining the
    /// catalog beside it.
    #[tokio::test]
    async fn the_tap_tool_schema_is_unchanged_by_the_exchange_joining_the_catalog() {
        let tools = listed_tools(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;
        let tap = tools
            .iter()
            .find(|tool| tool["name"] == "tap")
            .expect("the tap tool");
        let mut argument_names: Vec<&str> = tap["inputSchema"]["properties"]
            .as_object()
            .expect("tap declares properties")
            .keys()
            .map(String::as_str)
            .collect();
        argument_names.sort_unstable();
        assert_eq!(argument_names, ["channel", "count", "max_bag_bytes"]);
        assert_eq!(
            tap["inputSchema"]["required"],
            json!(["channel"]),
            "every argument beyond the channel stays optional, so the ordinary \
             call is still `tap <channel>`"
        );
    }

    // ------------------------------------------------------------------------
    // Resources and prompts
    // ------------------------------------------------------------------------

    use streamlib::sdk::descriptors::{
        PortDescriptor, ProcessorClassShortName, ProcessorDescriptor,
    };
    use streamlib::sdk::processors::PROCESSOR_REGISTRY;

    /// Two nodes and the one link between them, in the shape the engine's
    /// graph export takes, so the prompts parse what a real node answers.
    fn two_linked_nodes_graph() -> Value {
        json!({
            "stream": "desk-preview",
            "nodes": [
                {
                    "id": "PatternSourceId",
                    "type": "graph_probes:PatternSource",
                    "name": "pattern",
                    "ports": {
                        "inputs": [],
                        "outputs": [{ "name": "video", "description": "", "delivery_profile": null }]
                    },
                    "components": { "state": "Running" }
                },
                {
                    "id": "WindowSinkId",
                    "type": "graph_probes:WindowSink",
                    "name": "window",
                    "ports": {
                        "inputs": [{ "name": "video", "description": "", "delivery_profile": "newest" }],
                        "outputs": []
                    },
                    "components": { "state": "Running" }
                }
            ],
            "links": [{
                "id": "link-pattern-to-window",
                "source": { "node": "pattern", "port": "video" },
                "target": { "node": "window", "port": "video" },
                "state": "wired",
                "components": {}
            }],
            "exposed": [],
            "extensions": [],
            "runtime_name": "rig-desk-a1b2"
        })
    }

    /// The same graph after the helper that was to open the link refused it —
    /// what a live `connect` onto a helper-placed node leaves behind when
    /// that helper's port could not open.
    ///
    /// Kept beside the wired fixture rather than replacing its link: the
    /// prompts pick a link out of the graph they are rendered against, and an
    /// errored one has no business in the recipes' happy path.
    fn two_linked_nodes_graph_whose_link_a_helper_refused() -> Value {
        let mut graph = two_linked_nodes_graph();
        graph["links"][0]["state"] = json!("error");
        graph["links"][0]["error_reason"] = json!(
            "could not wire input port \"video\": BufferSizeExceedsMaxSupportedBufferSizeOfService"
        );
        graph
    }

    /// An agent is told to read a link's state, so a refused link has to reach
    /// it with the helper's own reason rather than a bare `error`.
    #[tokio::test]
    async fn tools_call_graph_carries_the_reason_a_helper_refused_a_link_for() {
        let runtime = ControlPlaneMcpDispatchStubRuntime::new();
        *runtime.exported_graph.lock() = two_linked_nodes_graph_whose_link_a_helper_refused();

        let body = tool_call_result(Arc::new(runtime), "graph", json!({})).await;
        let text = body["content"][0]["text"].as_str().unwrap();
        let graph: Value = serde_json::from_str(text).unwrap();
        assert_eq!(graph["links"][0]["state"], "error");
        assert!(
            graph["links"][0]["error_reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("BufferSizeExceeds")),
            "the helper's own reason is what tells an agent what to fix; got {graph}"
        );
    }

    /// The control vocabulary tells an agent to confirm `wired` in `graph`. A
    /// link onto a helper is not wired when `connect` returns, so the same text
    /// has to say what `pending` and `error` mean or the confirmation is a
    /// guess.
    #[tokio::test]
    async fn the_instructions_and_every_wiring_prompt_say_what_pending_and_error_mean() {
        register_a_virtual_camera_sink_probe_once();
        let mut texts = vec![(
            "instructions",
            LOCAL_API_MCP_SERVER_INSTRUCTIONS.to_string(),
        )];
        for (recipe, arguments) in [
            (
                "insert_node_between_linked_nodes",
                json!({ "link_id": "link-pattern-to-window", "type": "effects:Blur" }),
            ),
            (
                "fan_output_to_another_consumer",
                json!({ "from_node": "pattern", "from_port": "video", "type": "effects:Blur" }),
            ),
            (
                "show_channel_on_virtual_camera",
                json!({ "from_node": "pattern", "from_port": "video" }),
            ),
        ] {
            texts.push((
                recipe,
                prompt_text(stub_serving_two_linked_nodes(), recipe, arguments).await,
            ));
        }
        for (name, text) in texts {
            assert!(
                text.contains("pending") && text.contains("error_reason"),
                "{name} tells an agent to confirm a link's state but not what a link left \
                 `pending` by a helper that has not answered, or refused with a reason, \
                 means:\n{text}"
            );
        }
    }

    fn stub_serving_two_linked_nodes() -> Arc<ControlPlaneMcpDispatchStubRuntime> {
        let runtime = ControlPlaneMcpDispatchStubRuntime::new();
        *runtime.exported_graph.lock() = two_linked_nodes_graph();
        Arc::new(runtime)
    }

    /// The registry is process-global and refuses a second registration of a
    /// path, so the probe standing in for the built-in registers once for the
    /// whole test binary.
    fn register_a_virtual_camera_sink_probe_once() {
        static REGISTERED: std::sync::Once = std::sync::Once::new();
        REGISTERED.call_once(|| {
            PROCESSOR_REGISTRY
                .register_descriptor_only(
                    ProcessorDescriptor::new(
                        ProcessorClassShortName::new("VirtualCameraSink").unwrap(),
                        ProcessorClassImportPath::new(
                            crate::mcp_prompts::VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH,
                        )
                        .unwrap(),
                        "a virtual-camera probe",
                    )
                    .with_config_schema(
                        json!({ "type": "object", "properties": { "name": { "type": "string" } } }),
                    )
                    .with_input(
                        PortDescriptor::new("video", "frames to present", true)
                            .with_delivery_profile("newest"),
                    ),
                )
                .expect("the virtual camera path is registered by this helper alone");
        });
    }

    /// A result or refusal as it crossed the wire.
    fn wire_outcome<T: serde::Serialize>(
        outcome: std::result::Result<T, ServiceError>,
    ) -> std::result::Result<Value, McpError> {
        match outcome {
            Ok(result) => Ok(serde_json::to_value(result).unwrap()),
            Err(ServiceError::McpError(refusal)) => Err(refusal),
            Err(other) => panic!("the request failed below the protocol: {other}"),
        }
    }

    async fn listed_resources_result(runtime: Arc<dyn RuntimeOperations>) -> Value {
        let (_served, client) = connected_mcp_client(runtime).await;
        wire_outcome(client.list_resources(None).await).unwrap()
    }

    async fn listed_resource_templates_result(runtime: Arc<dyn RuntimeOperations>) -> Value {
        let (_served, client) = connected_mcp_client(runtime).await;
        wire_outcome(client.list_resource_templates(None).await).unwrap()
    }

    async fn listed_prompts_result(runtime: Arc<dyn RuntimeOperations>) -> Value {
        let (_served, client) = connected_mcp_client(runtime).await;
        wire_outcome(client.list_prompts(None).await).unwrap()
    }

    async fn resource_read_outcome(
        runtime: Arc<dyn RuntimeOperations>,
        uri: &str,
    ) -> std::result::Result<Value, McpError> {
        let (_served, client) = connected_mcp_client(runtime).await;
        wire_outcome(
            client
                .read_resource(ReadResourceRequestParams::new(uri))
                .await,
        )
    }

    async fn prompt_outcome(
        runtime: Arc<dyn RuntimeOperations>,
        prompt_name: &str,
        arguments: Value,
    ) -> std::result::Result<Value, McpError> {
        let (_served, client) = connected_mcp_client(runtime).await;
        let request = GetPromptRequestParams::new(prompt_name)
            .with_arguments(arguments.as_object().cloned().unwrap_or_default());
        wire_outcome(client.get_prompt(request).await)
    }

    async fn resource_document(runtime: Arc<dyn RuntimeOperations>, uri: &str) -> Value {
        let result = resource_read_outcome(runtime, uri)
            .await
            .unwrap_or_else(|refusal| panic!("reading `{uri}` was refused: {refusal:?}"));
        let contents = result["contents"].as_array().expect("a contents array");
        assert_eq!(contents.len(), 1, "one document per resource: {result}");
        assert_eq!(contents[0]["uri"], uri);
        assert_eq!(contents[0]["mimeType"], "application/json");
        serde_json::from_str(contents[0]["text"].as_str().expect("a text document"))
            .expect("the document is JSON")
    }

    async fn prompt_text(
        runtime: Arc<dyn RuntimeOperations>,
        prompt_name: &str,
        arguments: Value,
    ) -> String {
        let result = prompt_outcome(runtime, prompt_name, arguments)
            .await
            .unwrap_or_else(|refusal| panic!("`{prompt_name}` was refused: {refusal:?}"));
        let messages = result["messages"].as_array().expect("a messages array");
        assert_eq!(messages.len(), 1, "{result}");
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"]["type"], "text");
        messages[0]["content"]["text"].as_str().unwrap().to_string()
    }

    /// The tools a recipe's numbered steps call, in order — what a client
    /// following the text step by step dispatches.
    fn tool_names_the_numbered_steps_call(prompt_text: &str) -> Vec<String> {
        prompt_text
            .lines()
            .filter_map(|line| {
                let (step_number, rest) = line.split_once(". `")?;
                if step_number.is_empty() || !step_number.chars().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                rest.split_once('`')
                    .map(|(tool_name, _)| tool_name.to_string())
            })
            .collect()
    }

    fn served_tool_names() -> Vec<String> {
        LocalApiMcpServerHandler::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect()
    }

    #[tokio::test]
    async fn resources_list_names_the_node_catalog_and_the_live_graph() {
        let result =
            listed_resources_result(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;

        let uris: Vec<&str> = result["resources"]
            .as_array()
            .expect("a resources array")
            .iter()
            .map(|resource| resource["uri"].as_str().unwrap())
            .collect();
        assert_eq!(uris, ["streamlib://node-catalog", "streamlib://graph"]);
        for resource in result["resources"].as_array().unwrap() {
            assert_eq!(resource["mimeType"], "application/json", "{resource}");
            assert!(
                resource["description"]
                    .as_str()
                    .is_some_and(|d| !d.is_empty())
            );
        }

        let templates =
            listed_resource_templates_result(Arc::new(ControlPlaneMcpDispatchStubRuntime::new()))
                .await;
        assert_eq!(templates["resourceTemplates"], json!([]), "{templates}");
    }

    /// Mental revert: render the catalog once at startup and cache it, and the
    /// second read misses the type registered after the first.
    #[tokio::test]
    async fn the_catalog_resource_renders_the_registry_as_it_stands_at_each_read() {
        let class_import_path =
            "streamlib_api_server::catalog_resource_probe::RegisteredBetweenReads";
        let entry_for_the_probe = |catalog: &Value| {
            catalog["nodes"]
                .as_array()
                .expect("a node type list")
                .iter()
                .find(|entry| entry["type"] == class_import_path)
                .cloned()
        };

        let before = resource_document(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "streamlib://node-catalog",
        )
        .await;
        assert!(entry_for_the_probe(&before).is_none());

        let config_schema = json!({
            "type": "object",
            "properties": { "strength": { "type": "number", "default": 0.5 } }
        });
        PROCESSOR_REGISTRY
            .register_descriptor_only(
                ProcessorDescriptor::new(
                    ProcessorClassShortName::new("RegisteredBetweenReads").unwrap(),
                    ProcessorClassImportPath::new(class_import_path).unwrap(),
                    "registered after the first read",
                )
                .with_config_schema(config_schema.clone())
                .with_input(
                    PortDescriptor::new("video_from_upstream", "", true)
                        .with_delivery_profile("newest"),
                ),
            )
            .expect("the probe's path is registered by this test alone");

        let after = resource_document(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "streamlib://node-catalog",
        )
        .await;
        let entry = entry_for_the_probe(&after).expect("the type registered between reads");
        assert_eq!(entry["config_schema"], config_schema);
        assert_eq!(entry["inputs"][0]["name"], "video_from_upstream");
        assert_eq!(entry["inputs"][0]["delivery_profile"], "newest");
    }

    #[tokio::test]
    async fn the_graph_resource_renders_the_graph_as_it_stands_at_each_read() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let exported_graph = runtime.exported_graph.clone();

        let before = resource_document(runtime.clone(), "streamlib://graph").await;
        assert_eq!(before, json!({ "nodes": [], "links": [] }));

        *exported_graph.lock() = two_linked_nodes_graph();
        let after = resource_document(runtime, "streamlib://graph").await;
        assert_eq!(after, two_linked_nodes_graph());
    }

    #[tokio::test]
    async fn reading_a_resource_the_node_does_not_serve_is_refused_naming_the_uri() {
        let refusal = resource_read_outcome(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "streamlib://contracts",
        )
        .await
        .expect_err("an unserved uri is refused");
        assert_eq!(refusal.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(
            refusal.message.contains("streamlib://contracts"),
            "{refusal:?}"
        );
    }

    #[tokio::test]
    async fn prompts_list_names_the_four_recipes_and_their_arguments() {
        let result =
            listed_prompts_result(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;
        let prompts = result["prompts"].as_array().expect("a prompts array");

        let described: Vec<(String, Vec<(String, bool)>)> = prompts
            .iter()
            .map(|prompt| {
                let arguments = prompt["arguments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|argument| {
                        (
                            argument["name"].as_str().unwrap().to_string(),
                            argument["required"].as_bool().unwrap(),
                        )
                    })
                    .collect();
                (prompt["name"].as_str().unwrap().to_string(), arguments)
            })
            .collect();
        let owned = |name: &str, arguments: &[(&str, bool)]| {
            (
                name.to_string(),
                arguments
                    .iter()
                    .map(|(argument, required)| (argument.to_string(), *required))
                    .collect::<Vec<_>>(),
            )
        };
        let mut expected = vec![
            owned(
                "insert_node_between_linked_nodes",
                &[("link_id", true), ("type", true)],
            ),
            owned(
                "fan_output_to_another_consumer",
                &[("from_node", true), ("from_port", true), ("type", true)],
            ),
            owned(
                "show_channel_on_virtual_camera",
                &[
                    ("from_node", true),
                    ("from_port", true),
                    ("camera_name", false),
                ],
            ),
            owned(
                "look_at_what_a_channel_carries",
                &[("from_node", true), ("from_port", true)],
            ),
        ];
        let mut described = described;
        described.sort();
        expected.sort();
        assert_eq!(described, expected);
    }

    /// A prompt is a recipe over the tool set, never a verb of its own: every
    /// step of every recipe calls a tool `tools/list` serves.
    #[tokio::test]
    async fn every_step_of_every_prompt_calls_a_tool_the_node_serves() {
        register_a_virtual_camera_sink_probe_once();
        let served = served_tool_names();

        for (prompt_name, arguments) in [
            (
                "insert_node_between_linked_nodes",
                json!({ "link_id": "link-pattern-to-window", "type": "effects:Blur" }),
            ),
            (
                "fan_output_to_another_consumer",
                json!({ "from_node": "pattern", "from_port": "video", "type": "effects:Blur" }),
            ),
            (
                "show_channel_on_virtual_camera",
                json!({ "from_node": "pattern", "from_port": "video" }),
            ),
            (
                "look_at_what_a_channel_carries",
                json!({ "from_node": "pattern", "from_port": "video" }),
            ),
        ] {
            let text = prompt_text(stub_serving_two_linked_nodes(), prompt_name, arguments).await;
            let called = tool_names_the_numbered_steps_call(&text);
            assert!(!called.is_empty(), "{prompt_name} lists no steps:\n{text}");
            for tool_name in &called {
                assert!(
                    served.contains(tool_name),
                    "{prompt_name} calls `{tool_name}`, which the node does not serve:\n{text}"
                );
            }
        }
    }

    #[tokio::test]
    async fn the_insert_prompt_splices_the_named_link_in_an_order_that_never_leaves_it_unfed() {
        let text = prompt_text(
            stub_serving_two_linked_nodes(),
            "insert_node_between_linked_nodes",
            json!({ "link_id": "link-pattern-to-window", "type": "effects:Blur" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            [
                "add_node",
                "graph",
                "connect",
                "connect",
                "disconnect",
                "graph"
            ]
        );
        for named in [
            "`effects:Blur`",
            "`pattern`",
            "`window`",
            "`link-pattern-to-window`",
        ] {
            assert!(
                text.contains(named),
                "the recipe must name {named}:\n{text}"
            );
        }
    }

    /// The type an agent is about to add is described from the catalog as it
    /// is now, so the config keys it passes are the ones the node will take.
    #[tokio::test]
    async fn a_registered_types_catalog_entry_is_rendered_into_the_prompt() {
        let class_import_path = "streamlib_api_server::prompt_catalog_probe::GrayscaleEffect";
        PROCESSOR_REGISTRY
            .register_descriptor_only(
                ProcessorDescriptor::new(
                    ProcessorClassShortName::new("GrayscaleEffect").unwrap(),
                    ProcessorClassImportPath::new(class_import_path).unwrap(),
                    "a prompt-rendering probe",
                )
                .with_config_schema(json!({
                    "type": "object",
                    "properties": { "grayscale_strength_probe_key": { "type": "number" } }
                })),
            )
            .expect("the probe's path is registered by this test alone");

        let registered = prompt_text(
            stub_serving_two_linked_nodes(),
            "fan_output_to_another_consumer",
            json!({ "from_node": "pattern", "from_port": "video", "type": class_import_path }),
        )
        .await;
        assert!(
            registered.contains("grayscale_strength_probe_key"),
            "a registered type's config schema must reach the recipe:\n{registered}"
        );

        let unregistered = prompt_text(
            stub_serving_two_linked_nodes(),
            "fan_output_to_another_consumer",
            json!({ "from_node": "pattern", "from_port": "video", "type": "never_imported:Effect" }),
        )
        .await;
        assert!(
            unregistered.contains("not in this node's catalog yet"),
            "an unregistered type must be said to be absent, not described:\n{unregistered}"
        );
    }

    #[tokio::test]
    async fn the_virtual_camera_prompt_adds_the_registered_sink_on_its_own_input_port() {
        register_a_virtual_camera_sink_probe_once();

        let text = prompt_text(
            stub_serving_two_linked_nodes(),
            "show_channel_on_virtual_camera",
            json!({ "from_node": "pattern", "from_port": "video", "camera_name": "Desk \"cam\"" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            ["add_node", "connect", "graph"]
        );
        assert!(
            text.contains(crate::mcp_prompts::VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH),
            "{text}"
        );
        assert!(text.contains("`to_port`: `video`"), "{text}");
        assert!(
            text.contains(r#"{"name":"Desk \"cam\""}"#),
            "the camera name must reach the config as escaped JSON:\n{text}"
        );
    }

    #[tokio::test]
    async fn the_look_prompt_taps_the_channel_the_output_publishes_on() {
        let text = prompt_text(
            stub_serving_two_linked_nodes(),
            "look_at_what_a_channel_carries",
            json!({ "from_node": "pattern", "from_port": "video" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            ["tap", "exchange"]
        );
        assert!(
            text.contains("`channel`: `rig-desk-a1b2/pattern/video`"),
            "the channel is the runtime's name, the node's name, then the port:\n{text}"
        );
        assert!(
            text.contains(&format!(
                "{}-byte frame header",
                streamlib::sdk::iceoryx2::FRAME_HEADER_SIZE
            )),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_prompt_request_naming_nothing_the_node_has_is_refused_by_name() {
        for (case, prompt_name, arguments, named) in [
            (
                "an unknown prompt",
                "delete_everything",
                json!({}),
                "delete_everything",
            ),
            (
                "a missing required argument",
                "insert_node_between_linked_nodes",
                json!({ "link_id": "link-pattern-to-window" }),
                "`type`",
            ),
            (
                "a link the graph does not have",
                "insert_node_between_linked_nodes",
                json!({ "link_id": "no-such-link", "type": "effects:Blur" }),
                "no-such-link",
            ),
            (
                "a node the graph does not have",
                "look_at_what_a_channel_carries",
                json!({ "from_node": "nosuchnode", "from_port": "video" }),
                "nosuchnode",
            ),
            (
                "a port that is not one of the node's outputs",
                "look_at_what_a_channel_carries",
                json!({ "from_node": "window", "from_port": "video" }),
                "video",
            ),
            (
                "an argument that is not a string",
                "look_at_what_a_channel_carries",
                json!({ "from_node": 7, "from_port": "video" }),
                "7",
            ),
        ] {
            let refusal = prompt_outcome(stub_serving_two_linked_nodes(), prompt_name, arguments)
                .await
                .expect_err(case);
            assert_eq!(
                refusal.code,
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "{case}: {refusal:?}"
            );
            assert!(
                refusal.message.contains(named),
                "{case} must be refused naming `{named}`: {refusal:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_fan_prompt_adds_one_consumer_and_wires_it_to_the_named_port() {
        let text = prompt_text(
            stub_serving_two_linked_nodes(),
            "fan_output_to_another_consumer",
            json!({ "from_node": "pattern", "from_port": "video", "type": "effects:Blur" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            ["add_node", "graph", "connect", "graph"]
        );
        assert!(
            text.contains("`from_node`: `pattern`, `from_port`: `video`"),
            "{text}"
        );
    }

    /// A consumer reading another profile than the ones an output port already
    /// feeds is fanned like any other. Mental revert: restore the refusal and
    /// the recipe is an error naming both profiles.
    #[tokio::test]
    async fn the_fan_prompt_wires_a_consumer_reading_another_profile_than_the_port_already_feeds() {
        let ordered_input_probe = register_a_sole_input_probe_once("ordered");

        let text = prompt_text(
            stub_serving_two_linked_nodes(),
            "fan_output_to_another_consumer",
            json!({ "from_node": "pattern", "from_port": "video", "type": ordered_input_probe }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            ["add_node", "graph", "connect", "graph"]
        );
    }

    /// The virtual camera's `newest` input joins a port whatever its other
    /// consumers read. Mental revert: restore the refusal and a port already
    /// feeding an `ordered` consumer refuses the recipe.
    #[tokio::test]
    async fn the_virtual_camera_prompt_wires_onto_a_port_that_feeds_an_ordered_consumer() {
        register_a_virtual_camera_sink_probe_once();
        let runtime = ControlPlaneMcpDispatchStubRuntime::new();
        let mut graph = two_linked_nodes_graph();
        graph["nodes"][1]["ports"]["inputs"][0]["delivery_profile"] = json!("ordered");
        *runtime.exported_graph.lock() = graph;

        let text = prompt_text(
            Arc::new(runtime),
            "show_channel_on_virtual_camera",
            json!({ "from_node": "pattern", "from_port": "video" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            ["add_node", "connect", "graph"]
        );
    }

    /// A probe with one input declaring `delivery_profile`, registered once per
    /// profile for the test binary.
    fn register_a_sole_input_probe_once(delivery_profile: &'static str) -> String {
        static REGISTERED_PROFILES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
        let class_import_path = format!(
            "streamlib_api_server::prompt_delivery_profile_probe::{delivery_profile}::SoleInputProbe"
        );
        let mut registered_profiles = REGISTERED_PROFILES.lock();
        if !registered_profiles.contains(&delivery_profile) {
            PROCESSOR_REGISTRY
                .register_descriptor_only(
                    ProcessorDescriptor::new(
                        ProcessorClassShortName::new("SoleInputProbe").unwrap(),
                        ProcessorClassImportPath::new(&class_import_path).unwrap(),
                        "a probe with one input",
                    )
                    .with_input(
                        PortDescriptor::new("bags_from_upstream", "", true)
                            .with_delivery_profile(delivery_profile),
                    )
                    .with_output(PortDescriptor::new(
                        "bags_to_downstream",
                        "",
                        true,
                    )),
                )
                .expect("each probe path is registered by this helper alone");
            registered_profiles.push(delivery_profile);
        }
        class_import_path
    }

    /// Consumers of one output port read it under whatever profiles they each
    /// declare, so a type reading another profile than the link it is spliced
    /// into still takes the zero-gap order: both new links first, the old one
    /// last. Mental revert: take the link out first again and the target sees a
    /// gap no refusal calls for.
    #[tokio::test]
    async fn inserting_a_type_that_reads_another_profile_than_the_link_connects_before_it_disconnects()
     {
        for (inserted_profile, replaced_profile) in [("newest", "ordered"), ("ordered", "newest")] {
            let inserted_probe = register_a_sole_input_probe_once(inserted_profile);
            let runtime = ControlPlaneMcpDispatchStubRuntime::new();
            let mut graph = two_linked_nodes_graph();
            graph["nodes"][1]["ports"]["inputs"][0]["delivery_profile"] = json!(replaced_profile);
            *runtime.exported_graph.lock() = graph;

            let text = prompt_text(
                Arc::new(runtime),
                "insert_node_between_linked_nodes",
                json!({ "link_id": "link-pattern-to-window", "type": inserted_probe }),
            )
            .await;

            assert_eq!(
                tool_names_the_numbered_steps_call(&text),
                [
                    "add_node",
                    "graph",
                    "connect",
                    "connect",
                    "disconnect",
                    "graph"
                ],
                "a `{inserted_profile}` type into a `{replaced_profile}` link:\n{text}"
            );
        }
    }

    /// A class the agent just wrote is not in the catalog until its first add,
    /// and nothing about the recipe waits on it: the order is the zero-gap one
    /// and no note asks the agent to read a profile before wiring.
    #[tokio::test]
    async fn an_insert_of_an_uncatalogued_type_connects_before_it_disconnects_with_no_profile_note()
    {
        let text = prompt_text(
            stub_serving_two_linked_nodes(),
            "insert_node_between_linked_nodes",
            json!({ "link_id": "link-pattern-to-window", "type": "effects:WrittenJustNow" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            [
                "add_node",
                "graph",
                "connect",
                "connect",
                "disconnect",
                "graph"
            ]
        );
        assert!(
            !text.contains("delivery profile") && !text.contains("remove_node"),
            "nothing about the insert depends on the new type's profile:\n{text}"
        );
    }

    #[tokio::test]
    async fn inserting_into_a_link_whose_target_takes_one_inbound_link_removes_the_link_first() {
        let runtime = ControlPlaneMcpDispatchStubRuntime::new();
        let mut graph = two_linked_nodes_graph();
        graph["nodes"][1]["ports"]["inputs"][0]["delivery_profile"] = json!("ordered");
        graph["nodes"][1]["ports"]["inputs"][0]["audio_window"] =
            json!({ "resolved_from": "match_device" });
        *runtime.exported_graph.lock() = graph;

        let text = prompt_text(
            Arc::new(runtime),
            "insert_node_between_linked_nodes",
            json!({ "link_id": "link-pattern-to-window", "type": "effects:NeverImported" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            [
                "add_node",
                "graph",
                "disconnect",
                "connect",
                "connect",
                "graph"
            ]
        );
        assert!(text.contains("audio window contract"), "{text}");
    }
}
