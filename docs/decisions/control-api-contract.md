# One control API contract

Rationale for the control-api-contract change in `docs/plan/changes/control-api-contract.md`,
decided by the owner on 2026-11-10.

## Trigger

Read this before adding a control operation, adding a protocol or SDK that reaches the runtime,
writing a client by hand against the local API, or changing a request or result the runtime
answers.

## Decision

Every control operation is declared once, in the crate `streamlib-control-contract`, by one plain
`impl ControlOperation` naming its method, request type, result type, one-line doc, agent usage
note, the contract version it arrived in, its stability, its reference-docs group and the surfaces
that serve it. The impls are collected into one `const` table, `CONTROL_OPERATIONS`, and into the
enum `ControlOperationCall`, which the runtime's one dispatcher matches exhaustively: an operation
with no dispatch arm does not compile.

`cargo xtask generate-control-contract` renders the table as
`runtime/streamlib-control-contract/contract.json`, an OpenRPC 1.3.2 document whose schemas are
JSON Schema 2020-12 under `components.schemas`. CI fails when the committed file differs from the
registry. Every protocol (MCP now, JSON-RPC and a loopback WebSocket next), every generated client
and every reference page is a projection of that file; none holds operation logic or a hand-kept
list of operations. An MCP tool's description is its operation's agent usage note, and its input
schema is its request type's.

The contract has one integer version, bumped only by a breaking change: a removed or renamed
operation, a removed field, a narrowed type. An addition is marked by its `x-since` and needs no
bump. An `experimental` operation may change before it turns `stable` without one.

The request and result types are the action contract's own Rust types, referenced from the
engine-free crates that define them (`streamlib-project-repository`,
`streamlib-runtime-client-contract`) rather than moved, since those crates are already the shared
layer the CLI and the api-server both build on.

A loopback WebSocket, when it lands, is off by default and gated by a token the runtime writes to
an owner-only file and an `Origin` allow-list, against DNS rebinding.

## Rejected alternatives

- **The MCP tool set as the contract** — it ties every client to one protocol, and a hand-kept
  list of tool names drifted from what the runtime serves while nothing could catch it.
- **A derive macro on the request type, or one `control_operations!` table macro** — plain trait
  impls give the same single declaration and the exhaustive match with no new macro to maintain.
- **The LSP metaModel shape** (`requests`, `notifications`, `structures`) — OpenRPC doc tooling
  does not read it.
- **A hand-written client per consumer** — Studio's broke silently when tools were renamed; a
  client generated from the contract file fails at build time instead.

## Consequences

- A new operation is one impl, one request type, one dispatcher arm and a regenerated contract
  file; adapters pick it up from the table.
- OpenRPC has no notifications list, so the document carries them as `x-notifications` beside
  `methods`, keeping the file valid OpenRPC.
- The MCP input schemas take their titles from the request types (`TransactRequest`), so a tool's
  schema title changes when its operation moves into the registry; its name and result do not.
- The runtime unit ships the generated clients, so Studio builds against the copy in the unit it
  bundles, with no registry and no network.
