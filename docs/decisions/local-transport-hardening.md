# The same-host transport is engine-owned, sized for late joiners, and honest about wiring

Rationale for the `[local-transport-hardening]` work on iceoryx2 and the parent↔helper
protocol, proposed 2026-09-14. The shutdown ladder it also builds has its own record,
`shutdown-ladder.md`.

## Trigger

Read this before:
- constructing an iceoryx2 node or reading iceoryx2 configuration anywhere;
- choosing a channel's depth or a link cap;
- adding a field to the parent→child wiring envelope;
- reporting a link as wired;
- changing what the helper handshake checks.

## Decision

- **The engine owns the iceoryx2 domain.** One engine function builds every node's configuration
  from the library defaults, never from iceoryx2's lookup path (the working directory, the home
  directory, `/etc`).
  - The domain is per OS user: prefix `sl{uid}_`, rooted in StreamLib's runtime directory.
- **One runtime directory, and it always resolves.** Everything that means nothing once the
  processes are gone — the iceoryx2 domain, the surface-sharing socket, the node registry — lives
  in one directory. On Linux it is `$XDG_RUNTIME_DIR/streamlib/` when that variable is set and
  non-empty; otherwise — empty or unset, and on macOS always — it is a `/tmp/streamlib-<uid>/` the
  engine creates owner-only and checks as a real directory the uid owns with no group or other
  bits. The check runs once as the runtime starts, before its first node, socket or registry
  write, and a failure refuses the start by name. What a runtime keeps — logs, caches — stays in
  the project's `.streamlib/`.
  - The engine refuses by name a root that would overrun the Unix-socket path budget.
  - A helper is told the root by its parent, and refuses to start untold.
  - Nodes carry names for inspection, never for identity.
- **A channel is created once, deep enough for any consumer.**
  - Every data service is created at the `ordered` depth. Each subscriber's ring is its own port's
    depth, so consumers of different delivery profiles share one output port.
  - The parent holds the service factories for the channel's life, so a helper that opens first
    cannot size it.
  - The envelope carries the service's creation depth and the port's depth separately.
- **Caps are measured, not arbitrary.**
  - Fan-out is 32 consumers plus the tap and fan-in is 256 links; `max_nodes` is twice the cap.
  - Borrowed samples 1, loaned samples 1, history 0: the engine never holds more.
- **A link onto a helper is wired when the helper says so.** The helper answers every late
  `wire_link` with a link-scoped reply on its own rpc tag, and only that reply stamps `wired`.
  `connect` does not wait for it: the link reads `pending` until the reply turns it `wired`, or
  `error` with the helper's reason, which `graph` shows until the link is disconnected.
- **The handshake checks the build, not a protocol number.** Parent and helper compare an engine
  build id compiled into the one native artifact: version, git sha, and a per-build nonce. A
  mismatch or an absent id is refused by name before any channel opens.

## Rejected alternatives

- **Ambient iceoryx2 configuration.** Three failures:
  - A robotics user's `iceoryx2.toml` silently resizes our channels.
  - An app that changes directory after start leaves parent and helper in disjoint domains, where
    no data flows and nothing errors.
  - A second OS user on the machine cannot run StreamLib at all.
- **A domain per runtime.** Each run leaks a management-segment file forever, and nothing sweeps a
  crashed runtime's domain.
- **Deriving the helper's root from the surface-socket variable.** It is Linux-only, and it couples
  two concerns.
- > ~~**A silent `/tmp` fallback on Linux.** It invites squatting, and no user needs it: the
  > runtime already requires the runtime directory.~~ — Superseded 2026-09-14 by the owner's
  > ruling that a runtime starts in containers and CI with nothing set. The fallback is checked
  > (owner-only, a real directory the uid owns), never silent, which closes the squatting hole.
- **Refusing to start without `XDG_RUNTIME_DIR`.** Containers and CI rarely set it; this repo's
  own CI sets it by hand only to get past that refusal.
- **A StreamLib-specific override variable.** A second dial for what `XDG_RUNTIME_DIR` already
  overrides.
- **The project's `.streamlib/`.** A nested project path overruns the socket budget (109 bytes for
  an example in this repo against 63), crash leftovers would accumulate there forever, and a
  project may sit on a network or bind mount where sockets and locks misbehave.
- **Sizing a channel by its first consumer's profile.** A running output port then refuses a
  deeper consumer, which contradicts channels sized for a destination that connects later.
- **Sizing a channel to a windowed port's mailbox depth.** iceoryx2 commits bookkeeping up front:
  141 MiB of publisher heap at 8,000 slots.
- **A smaller per-subscriber ring to save memory.** The sample pool is sized by the service depth
  and the subscriber count alone, so it saves nothing.
- **Keeping 8 destinations and 8 inbound links.** Both numbers were arbitrary, and a live console
  hit the inbound wall.
- **257 destinations.** About 300 MiB on an ordinary graph, and silent lost-chunk loss within
  reach of a normal process.
- **Re-homing a channel past its cap.** It removes the ceiling at a protocol and identity cost no
  graph needs today. It stays the path if one ever does.
- **iceoryx2 history to cover a late wire.** It replays stale bags and still does not prove the
  helper opened its port.
- **`connect` waiting for the helper's reply.** A helper reads commands only between callbacks,
  so a control-plane call would wait on user code in a long `process()`, which the isolation
  axis forbids. The caller reads the outcome in `graph` instead.
- **Keeping the protocol-version integer.** It is hand-bumped, and it passes a helper that imported
  a stale wheel or one built against a different iceoryx2 patch. Every service open then fails as a
  misleading corrupted-service error.

## Consequences

- On the fallback arm, leftovers stay in `/tmp` until reboot rather than logout.
- The stock `iox2` tool sees StreamLib's services only when given a matching configuration.
- Every node-construction site, tests and benches included, goes through the engine function. A
  source-walking gate refuses a raw builder anywhere else, because a partial migration hangs tests
  silently across two domains.
- A `newest` channel costs about 0.22 MiB more publisher heap than before. A 32-consumer channel
  costs about 0.82 MiB per publisher and 0.10 MiB per connection.
- A link onto a helper can read not-yet-wired for as long as the helper takes to reach its next
  command read.
- Every build is its own id, so a helper and parent built minutes apart from the same sha refuse
  each other. That is the point.
