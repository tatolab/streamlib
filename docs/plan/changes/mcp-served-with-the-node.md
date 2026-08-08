# Change: mcp-served-with-the-node

MCP has exactly one transport: the node's own control plane. The `streamlib mcp` CLI
verb, its stdio JSON-RPC transport, and its `--attach` bridge are deleted. Owner ruling,
2026-08-08, taken during `/implement #1712` when the verb's remaining purpose was
examined against the mutation-verb trim that ticket performs.

No ADR: this touches no plugin ABI, no RHI, no engine IPC wire format, and no processor
model surface. It removes a control-plane *carrier*; the MCP tool vocabulary is unchanged
by this file (it is trimmed to observation shape by `importable-python-library-ripout`).

Recon verified every cited file:line against the tree on 2026-08-08.

## Behavior after this change

An MCP host reaches StreamLib by pointing at a running node's control-plane URL — the
same `POST /mcp` the api-server already mounts when the node boots. There is nothing to
start, nothing to attach, and no second lifecycle to reason about: `streamlib dev` is the
whole setup step, and the agent's own config carries the node's URL.

This is not new plumbing. `handlers.rs:128` mounts `/mcp` unconditionally on every
api-server router, so every node that hosts a control plane already serves MCP today. What
this change removes is the *second* way to reach the same dispatch.

## Why the verb has no remaining purpose

The `mcp` verb has two modes (`tools/streamlib-cli/src/commands/mcp.rs:32-37`), and the
pivot has taken the ground out from under both:

- **In-process** (`mcp.rs:44-68`) builds a fresh empty `Runner::with_auto_build()` and
  serves MCP against it. Its entire point was that an agent could then *populate* it with
  `submit_processor` / `connect`. Those verbs are deleted by
  `importable-python-library-ripout` (§Control plane: "code is the source of truth; the
  edit loop is `dev`, not live mutation"), leaving an engine that boots empty and can
  never be filled — a GPU context and an engine boot in exchange for an empty `graph`, a
  `tap` with no channels, and `logs` of nothing. `RunnerAutoBuild` is itself a REMOVED
  pattern in that change, so the mode does not survive it regardless.
- **`--attach <url>`** (`mcp.rs:86-124`) is a byte-for-byte pipe from stdio to the very
  `POST {url}/mcp` an MCP host can speak to directly over streamable HTTP. It carries no
  behavior the endpoint does not already have — it is a transport adapter for hosts that
  only spoke stdio.

Keeping either would also contradict §Control plane's existing decided line: "the control
plane exists to observe and drive *running* nodes, not to embed."

## MODIFIED

- **§Control plane & observability, the CLI-surface bullet** — strike `mcp` from the verb
  list. The list becomes `new` / `dev` / `run` plus the observation verbs `nodes` /
  `graph` / `tap` / `logs`. Nothing else in that bullet changes.

- **§Control plane & observability, the one-control-plane bullet** — append the transport
  statement: MCP is served by the node's control plane at `POST /mcp`, mounted with the
  node and sharing its lifecycle; it has exactly one transport, and no CLI verb, stdio
  server, or bridge process stands between a host and that endpoint. An MCP host is
  configured with a running node's URL.

- **`docs/plan/diagrams/system.mmd:17`** — the `cli` node label reads `new / dev / run ·
  nodes / graph / tap / logs / mcp`; drop the trailing `/ mcp`. The `ctl` node label
  (line 16) already carries MCP as part of the api-server's surface and is correct as-is.

- **`docs/plan/changes/importable-python-library-ripout.md:15-16`** — annotate the clause
  "The CLI `mcp` verb and `Dockerfile`/`docker/` re-point at the wheel-hosted runtime" as
  superseded in its `mcp` half. Its `Dockerfile`/`docker/` half is untouched. Its line 172
  parenthetical ("mutation verbs trimmed from `control.rs` and `mcp.rs`") is satisfied
  more strongly by deletion of the file, not weakened by it.

## REMOVED

One bare pattern per bullet — the ship gate greps the whole line verbatim, so no bullet
here carries a slash, a parenthetical, or a second item.

- REMOVED: `serve_stdio_jsonrpc`
- REMOVED: `tools/streamlib-cli/src/commands/mcp.rs`
- REMOVED: `Commands::Mcp`
- REMOVED: `for_stdio_protocol`
- REMOVED: `PrettyMirrorStream`
- REMOVED: `pretty_mirror_stream`

### What the last three bullets are

A consequence chain, not scope creep — each item's sole reason to exist is the verb above
it, verified by reverse-dependency sweep:

`StreamlibLoggingConfig::for_stdio_protocol` (`core/logging/config.rs:137`) exists so a
subcommand speaking a byte protocol on stdout can push the pretty log mirror to fd 2. Its
only caller is `main.rs:555`, the `mcp` arm of `logging_config_for`. It is the only
producer of `PrettyMirrorStream::Stderr` (`config.rs:139`), whose only reader is
`init.rs:255`. With `Stderr` unreachable, `PrettyMirrorStream` is a one-variant enum and
`StreamlibLoggingConfig::pretty_mirror_stream` (`config.rs:59`) is a dial with one legal
value — a no-op field on a public struct, which engine doctrine prohibits pre-1.0.

The field is set to `PrettyMirrorStream::Stdout` at ~13 sites, all of them tests plus one
test binary (`core/logging/tests.rs`, `tests/bin/log_emit_1000.rs`); removal is mechanical
there. `streamlib::sdk::logging::StreamlibLoggingConfig` is public API, so this is a
breaking change to it — permitted pre-1.0, and named here so it is not discovered in a
diff.

## Consequences to route, not decide

- **Exposure posture.** The in-process stdio path was auth-free by construction — a local
  child process, no socket. After this change the only path to MCP is the node's HTTP
  surface, which is bearer-gated only when a token is configured (`handlers.rs:128-134`;
  `mcp_auth_token: None` mounts no auth layer) on a node that binds `0.0.0.0` by default
  per `[control-plane-bind-posture]`. This is already the status quo for `graph`, `tap`,
  and `logs` — MCP joins a surface that bullet already governs, and this change decides
  nothing about it. §Control plane's **OPEN** bullet on auth and remote-access posture
  remains the only place that may narrow it, untouched here.

- **Ticket #1712's stated constraint is void.** Its body carries "Sequencing prerequisite
  of the contract deletion (#1715) — the CLI `mcp` verb must keep working throughout."
  There is no verb to keep working. Owed to `/reconcile-tracker`.

- **`mvp-app-experience.md:49`** asserts "The `mcp` in-process path (`mcp.rs:44-68`) does
  none of this today and stays as-is." That file is already blanket-annotated as partially
  superseded and instructs "derive nothing new from this file", so it needs no separate
  annotation; recorded here so the contradiction is not read as live guidance.

## Ticketing note

This is a subtraction inside work already ticketed. `/derive-tickets` should fold it into
**#1712** rather than mint a new ticket: the api-server trim, the CLI retirement, and the
wheel's verb set are all that ticket's diff, and the wheel never grows an `mcp` verb it
would then have to lose. The logging-config chain rides the same PR that deletes the verb —
it does not compile otherwise.
