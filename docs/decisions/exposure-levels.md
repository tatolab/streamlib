# Exposure is a live, three-level permission at a stream's edge

Rationale for the exposure entry in `docs/plan/ARCHITECTURE.md` §Networking, decided 2026-10-04
in an align on exposure, superseding the binary exposure decided 2026-09-30 under the
one-runtime-per-machine pivot.

## Trigger

Read this before gating anything inside a stream on exposure, before adding an exposure check
that only takes effect on restart, before keying a permission on something other than a port's
level, and before passing an exposure level as a string.

## Decision

Every output port of a stream is internal (the default), private or public. Inside a stream,
any node links to any port and exposure is never consulted. Private lets other streams and code
on the same machine read the port, so one stream's hardware camera can feed others. Public adds
a URL reachable off the machine, which other machines and tools pull. Exposure
is a permissions map the runtime consults where a read crosses a stream's edge, and it can be
changed live.

## Why

- **Visibility, not logic.** The owner's model is a dial on who else may reach a port. A stream's
  own graph never depends on its exposure, so changing exposure never changes what a stream does.
  It only changes who outside it can read.
- **Live, so it is checked at the edge.** Zenoh's own access control is fixed when its session
  opens. A live permission therefore lives in the engine, at the points a read crosses a stream's
  edge, like a proxy in front of every port. A change reaches the next check, cuts off readers it
  no longer allows, and needs no restart of the runtime or of any stream. (Zenoh is removed as of
  2026-10-04, `moq-on-the-tailnet.md`; the edge check stands on its own.)
- **`expose(output)` means private.** The common case is sharing on the machine, and private keeps
  the 2026-09-30 rule that nothing leaves the machine unasked: leaving takes an explicit public.
- **An enum, not a string.** A string level is a typo the type checker cannot see; an enum member
  is checked by pyright and by the stub test.
- **The internet is not the runtime's.** A public port's URL is what Tailscale serve or funnel
  publishes, so the runtime has nothing to build for internet exposure.

  > Amended 2026-10-04 by `moq-on-the-tailnet.md`: that holds for the HTTP. Live data reaches the
  > internet through a relay the machine has joined, which offers every public port there;
  > Funnel carries TCP only. The levels are unchanged.

- **Pull, never push.** A source wiring itself into a reader on another machine is a server
  wiring its URL into a client's browser. The reader's side decides what it consumes: it finds an
  exposed port and pulls it. This also matches the local-API rule that control never crosses
  machines, which a link request quietly did.

- **No tap.** A tap that reads any port is a door around the permission. Reading a port from
  outside its stream goes through exposure, so every read is listed and revocable; a stream's
  insides are seen through its own logs.

## Rejected alternatives

- **Any runtime may push or wire two others (2026-09-14).** It existed so agents could push data
  dynamically. Under pull-only, an agent pulls on the machine where the data is wanted.

- **Binary exposure, with streams on one machine linking freely (2026-09-30).** It gave the
  machine two rules: any stream could read any other stream's ports, while the local URL listener
  served only exposed ones. It also left no way for a stream to keep a port to itself on a
  shared machine.

## Consequences

- The `tap` and `exchange` verbs and their MCP tools are retired once a private read can stand
  in for them, including the repo's GPU verification. That is a later change. (Settled
  2026-10-04 by `moq-on-the-tailnet.md`: the later change is the sharing step, which builds the
  snapshot and sample forms the repo's verification reads from a private port.)

- The link request (push and third-party wiring) is retired: `request_link_on_remote_input_runtime`,
  the remote-destination spellings in Python and MCP, the link-request queryable, and `graph`'s
  `created_by_runtime_name` and `link_requests_awaiting_runtime`. Removing them is a later change.

- `expose` on the CLI, the app and the local API edits the live map. Whether those live edits
  persist across a restart is the runtime-hosting change's to settle.
- The URL listener serves private and public ports on loopback, and only public ones on a
  ~~LAN or~~ tailnet address (no LAN address is served: amended 2026-10-04 by
  `moq-on-the-tailnet.md`).
