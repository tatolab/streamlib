// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared HTTP state, OpenAPI document, and request/response wire types.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use streamlib::sdk::json_schema::SchemaIdentOutput;
use streamlib::sdk::runtime::{ProcessorLanguage, RuntimeOperations};
use utoipa::OpenApi;

/// Shared HTTP handler state.
#[derive(Clone)]
pub(crate) struct AppState {
    pub runtime: Arc<dyn RuntimeOperations>,
    /// Runtime id — used to look up the matching MoQ session registry
    /// inside `@tatolab/moq` when the `moq` feature is enabled.
    #[cfg(feature = "moq")]
    pub runtime_id: String,
    pub openapi: utoipa::openapi::OpenApi,
}

// ============================================================================
// Request/Response Types with OpenAPI Schema
// ============================================================================

#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct CreateProcessorRequest {
    /// Structured processor identity — the four-field map form of
    /// `@org/package/Type@version`. The structured-everywhere rule
    /// applies on the HTTP API too — bare strings like
    /// `"CameraProcessor"` are rejected at deserialize time.
    pub processor_type: SchemaIdentOutput,
    /// Processor-specific configuration as JSON
    pub config: serde_json::Value,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct CreateConnectionRequest {
    /// Source processor ID
    pub from_processor: String,
    /// Source output port name
    pub from_port: String,
    /// Destination processor ID
    pub to_processor: String,
    /// Destination input port name
    pub to_port: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct IdResponse {
    /// The created resource ID
    pub id: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct ErrorResponse {
    /// Error message
    pub error: String,
}

/// Body returned alongside `422 Unprocessable Entity` when the caller
/// supplies a structurally-valid `SchemaIdent` whose type isn't registered.
/// The runtime is dynamic — types load and unload — so this is a normal
/// runtime miss, not a malformed request. The client gets a typed
/// discriminator (`error`), the offending ident, and the placeholder
/// processor id (the failed node is left in the graph in `Error` state for
/// observability via `GET /api/graph`).
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct UnknownProcessorTypeResponse {
    /// Typed error discriminator: always `"UnknownProcessorType"`.
    pub error: &'static str,
    /// The structured ident that didn't resolve.
    pub ident: SchemaIdentOutput,
}

/// Body returned alongside `404 Not Found` when a connection references a
/// processor id that doesn't exist in the graph.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct ProcessorNotFoundResponse {
    /// Typed error discriminator: always `"ProcessorNotFound"`.
    pub error: &'static str,
    /// The processor id that wasn't in the graph.
    pub processor_id: String,
}

/// Body returned alongside `422 Unprocessable Entity` when a connection
/// references a port name that doesn't exist on the named processor.
/// Distinct from `UnknownProcessorTypeResponse`: the processor exists,
/// but the port doesn't.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct ProcessorPortNotFoundResponse {
    /// Typed error discriminator: always `"ProcessorPortNotFound"`.
    pub error: &'static str,
    /// The processor id whose port lookup failed.
    pub processor_id: String,
    /// The port name that wasn't found.
    pub port_name: String,
    /// `"input"` or `"output"`.
    pub direction: &'static str,
}

/// OpenAPI-documentable mirror of [`ProcessorLanguage`], which derives serde
/// but not `utoipa::ToSchema`. Kept identical to the SDK enum's wire form
/// (lowercase; `deno` is accepted as an alias for `typescript`) and mapped
/// into it on the way in.
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ProcessorLanguageDto {
    /// Rust — rejected for live source submit (a full cargo build, not a
    /// live graph mutation); present for wire-form parity with the SDK enum.
    Rust,
    Python,
    #[serde(alias = "deno")]
    TypeScript,
}

// A derived `ToSchema` would drop the `deno` alias — utoipa reads serde
// `rename`/`rename_all` but not `alias` — leaving a spec-driven client unaware
// `deno` is accepted. Hand-implement the schema with the same 4-value enum the
// SDK's `ProcessorLanguage` JsonSchema advertises.
impl utoipa::PartialSchema for ProcessorLanguageDto {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::String)
            .description(Some(
                "Processor runtime language. `deno` is accepted as an alias for `typescript`.",
            ))
            .enum_values(Some(["rust", "python", "typescript", "deno"]))
            .into()
    }
}

impl utoipa::ToSchema for ProcessorLanguageDto {}

impl From<ProcessorLanguageDto> for ProcessorLanguage {
    fn from(dto: ProcessorLanguageDto) -> Self {
        match dto {
            ProcessorLanguageDto::Rust => ProcessorLanguage::Rust,
            ProcessorLanguageDto::Python => ProcessorLanguage::Python,
            ProcessorLanguageDto::TypeScript => ProcessorLanguage::TypeScript,
        }
    }
}

/// Which end of a link the newly-instantiated processor's port sits on, for
/// an optional `connect` wiring in a [`SubmittedProcessorSourceRequest`].
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SourceProcessorPortRole {
    /// `local_port` is the OUTPUT (upstream) end; it feeds the peer's input.
    Output,
    /// `local_port` is the INPUT (downstream) end; it is fed by the peer's output.
    Input,
}

/// One optional post-instantiation wiring in a
/// [`SubmittedProcessorSourceRequest`]: connect a port on the
/// newly-instantiated processor to a port on a processor already in the graph.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct SourceProcessorConnection {
    /// Port name on the newly-instantiated processor.
    pub local_port: String,
    /// Whether `local_port` is the output or the input end of the link.
    pub role: SourceProcessorPortRole,
    /// The already-present peer processor's id (as returned by
    /// `POST /api/processor` or a prior source submit).
    pub peer_processor: String,
    /// The peer processor's port name.
    pub peer_port: String,
}

/// Body of `POST /api/processor/source`: submit processor source text for
/// live registration, then instantiate it and (optionally) wire it in.
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct SubmittedProcessorSourceRequest {
    /// The runtime language the source is authored in.
    pub language: ProcessorLanguageDto,
    /// The processor source text (a Python module / a TypeScript module).
    pub source: String,
    /// The `@session/<name>` package-name segment to mint the registration
    /// under. Omit to derive it from `processor_type_name`. One of
    /// `requested_name` / `processor_type_name` must be present.
    #[serde(default)]
    pub requested_name: Option<String>,
    /// The PascalCase processor type name the source defines. Omit to derive
    /// it from `requested_name`.
    #[serde(default)]
    pub processor_type_name: Option<String>,
    /// Config applied when the registered processor is instantiated into the
    /// graph. Defaults to an empty object.
    #[serde(default)]
    pub config: Option<serde_json::Value>,
    /// Optional wirings applied after instantiation, each connecting a port on
    /// the new processor to a port on an existing graph processor.
    #[serde(default)]
    pub connect: Vec<SourceProcessorConnection>,
}

/// Body of `POST /api/processor/source/replace`: swap a live
/// `@session/<name>` source registration for a replacement, transactionally
/// (a failed replacement restores the prior registration).
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct ReplaceProcessorSourceRequest {
    /// The `@session/<name>@<range>` module whose prior registration is
    /// removed before the replacement registers (wire form, e.g.
    /// `@session/widget@*`).
    pub target_session_module: String,
    /// The replacement source text.
    pub source: String,
    /// The replacement's runtime language.
    pub language: ProcessorLanguageDto,
    /// The replacement's `@session/<name>` package-name segment. Must resolve
    /// to the same `<name>` as `target_session_module` — a replace
    /// re-registers the same name, never renames.
    #[serde(default)]
    pub requested_name: Option<String>,
    /// The replacement's PascalCase processor type name.
    #[serde(default)]
    pub processor_type_name: Option<String>,
}

/// One committed port in a [`RegisteredProcessorPortsResponse`].
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct RegisteredPortResponse {
    /// The port name.
    pub name: String,
    /// The port's schema id — `"any"` or a fully-qualified
    /// `@org/package/Type@version`.
    pub schema: String,
    /// Input-port delivery-profile override; always absent on output ports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_profile: Option<String>,
}

/// One installed processor's committed port surface in a
/// [`RegisterProcessorSourceResponse`].
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct RegisteredProcessorPortsResponse {
    /// The processor's PascalCase short `Type` name.
    pub name: String,
    /// Input ports, in declaration order.
    pub inputs: Vec<RegisteredPortResponse>,
    /// Output ports, in declaration order.
    pub outputs: Vec<RegisteredPortResponse>,
}

/// Composite outcome of a source submit: `Added` when an instance was created
/// into the running graph, `Registered` when only the definition was registered
/// (no instantiable processor discovered).
#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RegistrationOutcome {
    /// An instance was instantiated into the running graph.
    Added,
    /// Only the definition was registered; no instantiable processor discovered.
    Registered,
}

/// Response to `POST /api/processor/source` and
/// `POST /api/processor/source/replace`: the minted registration ident plus
/// each installed processor's discovered ports, and — for a source submit —
/// the instantiated instance id and any created connection ids.
#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct RegisterProcessorSourceResponse {
    /// The minted `@session/<name>@0.0.N` registration module ident (NOT an
    /// `add_processor` instance id).
    pub module: String,
    /// The processors the registration installed, with their committed ports.
    pub processors: Vec<RegisteredProcessorPortsResponse>,
    /// The `add_processor` instance id, present when the composite
    /// instantiated the first discovered processor into the graph.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processor_id: Option<String>,
    /// Composite outcome: `added` when an instance was created into the running
    /// graph, `registered` when only the definition was registered (no
    /// instantiable processor discovered). Live per-instance state is observed
    /// via `GET /api/graph` and the `events_url` event stream.
    pub state: RegistrationOutcome,
    /// Link ids created by the optional `connect` wirings, in request order.
    pub connections: Vec<String>,
    /// The WebSocket URL carrying this runtime's live event stream.
    pub events_url: &'static str,
}

// ============================================================================
// OpenAPI Documentation
// ============================================================================

#[derive(OpenApi)]
#[openapi(
    paths(crate::handlers::tap_websocket_handler),
    info(
        title = "StreamLib Runtime API",
        version = "0.1.0",
        description = "Real-time streaming infrastructure API for managing processors, connections, and events",
        license(name = "BUSL-1.1")
    ),
    tags(
        (name = "graph", description = "Graph inspection and management"),
        (name = "processors", description = "Processor lifecycle management"),
        (name = "connections", description = "Connection management between processors"),
        (name = "registry", description = "Processor and schema registry"),
        (name = "schemas", description = "Schema definitions"),
        (name = "events", description = "Real-time event streaming via WebSocket")
    )
)]
pub(crate) struct ApiDoc;
