# Data plane: cast, not contract

Rationale for the `[data-plane-cast-not-contract]` entries in
`docs/plan/ARCHITECTURE.md` §Processor model & scheduling, decided 2026-07-29.

## Trigger

Read this before designing anything that compares a producer's and a consumer's schema
declarations, adds a version check to the data path, refuses or warns a `connect` based
on types, or attaches channel policy to a schema definition.

## Decision

A link is pure plumbing — output port → input port, carrying a bag. Typing is
unilateral: the producer declares what it writes, the consumer declares what it reads,
and the two declarations are hints that are never compared. Consuming is a cast at read
time (in the spirit of TypeScript `as` or parsing an arbitrary object with zod): success
is decided by the bag's actual shape, not by matching declared identities. The engine
mediates no schema agreement — connect always wires (an advisory log line when two
declared hints differ is acceptable; refusal is not), nothing matches tags per read, the
wire tag survives only as inert observability metadata, and versions never appear at the
code layer: version is a resolution-time concern (install/link + lockfile, the
node_modules model).

Channel policy (delivery profile, ring depth, overflow) is declared port-locally at the
consuming input port — policy is not type information and never rides a schema. A
concretely-typed input port with no declared delivery profile is a wiring error, not a
silent default.

## Rejected alternatives

- **Engine-mediated schema agreement (strict/loose connect, per-read tag matching)** —
  re-couples the two ends the design keeps independent, and breaks down across languages
  and across nodes, where the other end's declarations aren't reachable.
- **Version matching on the data path** — created a bug class of its own (version-blind
  sentinel vs version-carrying peers compared with `==`), and contradicts the
  node_modules model where using code never sees versions.
- **Schema-carried channel policy** — policy piggybacking on type identity; has no
  cross-host carrier under mesh transports, so each endpoint must be able to derive its
  policy locally from its own port declaration.
- **Silent delivery-profile default** — sample-stream payloads (audio, encoded video)
  silently degraded to latest-wins would drop data without any visible error.

## Consequences

- The trade is explicit: less static strictness for better developer experience and
  simpler language-to-language and node-to-node interop — a cast either works or it
  doesn't, regardless of who produced the bag.
- Type mismatches surface as runtime read failures at the consumer, not wiring errors.
- The existing agreement machinery, strict-connect posture, per-read expected-schema
  plumbing, and load-bearing wire-tag version fields are unintended shape to be removed;
  per §Product's zero-ceremony bar this removal is MVP-blocking work.
- The wire tag stays for tap, debugging, and catalog display only.
