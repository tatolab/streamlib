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
a URL reachable off the machine, the bundled relay, and other machines over the mesh. Exposure
is a permissions map the runtime consults where a read crosses a stream's edge, and it can be
changed live.

## Why

- **Visibility, not logic.** The owner's model is a dial on who else may reach a port. A stream's
  own graph never depends on its exposure, so changing exposure never changes what a stream does.
  It only changes who outside it can read.
- **Live, so it is checked at the edge.** Zenoh's own access control is fixed when its session
  opens. A live permission therefore lives in the engine, at the points a read crosses a stream's
  edge, like a proxy in front of every port. A change reaches the next check, cuts off readers it
  no longer allows, and needs no restart of the runtime or of any stream.
- **`expose(output)` means private.** The common case is sharing on the machine, and private keeps
  the 2026-09-30 rule that nothing leaves the machine unasked: leaving takes an explicit public.
- **An enum, not a string.** A string level is a typo the type checker cannot see; an enum member
  is checked by pyright and by the stub test.
- **The internet is not the runtime's.** A public port's URL is what Tailscale serve or funnel
  publishes, so the runtime has nothing to build for internet exposure.

## Rejected alternatives

- **Binary exposure, with streams on one machine linking freely (2026-09-30).** It gave the
  machine two rules: any stream could read any other stream's ports, while the local URL listener
  served only exposed ones. It also left no way for a stream to keep a port to itself on a
  shared machine.
- **A per-runtime grant created by a stream's own outbound link (PR #2622, unreviewed).** It
  needed a separate grant table, the asker's self-reported name on the wire, and a lifetime rule.
  The owner's model has no such grant: the levels are the whole policy.

## Consequences

- PR #2622's grant mechanism is dropped. Its offer and egress checks are rebuilt against levels.
- `expose` on the CLI, the app and the local API edits the live map. Whether those live edits
  persist across a restart is the runtime-hosting change's to settle.
- The URL listener serves private and public ports on loopback, and only public ones on a LAN
  or tailnet address.
