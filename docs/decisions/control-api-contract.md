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

> ~~A loopback WebSocket, when it lands, is off by default and gated by a token the runtime writes to
> an owner-only file and an `Origin` allow-list, against DNS rebinding.~~ — Superseded 2026-11-12 by
> the owner's ruling below.

The loopback WebSocket is always on and fully open (owner, 2026-11-12): the loopback listener
serves `/control/ws` with no token and no check of the `Host` or `Origin` a request gives, so a
headless runtime, a script and a web tool under a name of the person's own all reach it with no
setup. The owner accepted that any page in a browser on the machine can then drive the runtime,
and will layer a gate on as a dedicated design.

The TypeScript client is a client, not a typed transport (owner, 2026-11-12). The generator writes
one method per operation and one `on_<notification>` per notification, and a zod schema for every
component schema, every method's params and result and every notification's params. A method
checks its params before anything is sent and checks the answer before it resolves; a
notification is checked before a listener hears it. The generated JavaScript is type-checked
against the generated types, so a schema whose output its type does not accept fails the check;
the check proves the two compatible, not identical. A schema the contract closes
(`additionalProperties: false`) is strict and every other keeps fields it does not name, so a
client reads an answer that gained a field without a version bump. The same checks are exported
without I/O for a caller with a connection of its own, and the client runs over any transport
that sends one message and resolves its answer, and closes. An integer past `Number.MAX_SAFE_INTEGER` is read
and written as a `bigint`.

## Rejected alternatives

- **The MCP tool set as the contract** — it ties every client to one protocol, and a hand-kept
  list of tool names drifted from what the runtime serves while nothing could catch it.
- **A derive macro on the request type, or one `control_operations!` table macro** — plain trait
  impls give the same single declaration and the exhaustive match with no new macro to maintain.
- **The LSP metaModel shape** (`requests`, `notifications`, `structures`) — OpenRPC doc tooling
  does not read it.
- **OpenRPC's own generator (`@open-rpc/generator`)** — it names types by hash, checks params but
  never answers, reaches a runtime only by host and port, and ignores `x-notifications`.
- **zod's `z.fromJSONSchema` at run time** — it gives no static types and zod calls it
  experimental.
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
