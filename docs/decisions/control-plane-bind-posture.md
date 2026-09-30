# Control-plane exposure is an auth question, not a bind question

Rationale for the `[control-plane-bind-posture]` entries in `docs/plan/ARCHITECTURE.md`
§Control plane & observability, decided 2026-08-04.

## Trigger

Read this before changing what address a node listens on by default, before giving `dev`
a different network posture from `run`, and before treating a narrow bind as the thing
that protects a node.

## Decision

`dev` and `run` bind the control plane identically — all interfaces (`0.0.0.0`) by
default, narrowed per invocation by `--host`. There is no dev-only exposure posture.

Scoping exposure down belongs entirely to the OPEN auth and remote-access posture. A
node another host can reach is bound wide by definition, so the bind address cannot be
the lever that scopes exposure: it is a reachability control, and the system's target
shape — nodes discovering and driving each other across a mesh — requires reachability.
What remains is *who may call*, which is authentication and authorization. Until that is
decided, nothing narrows the default.

> ~~`dev` binds loopback by default.~~ — Superseded 2026-08-04 by this record. The
> behaviour never existed: the retired Rust `run`/`dev` bound all interfaces, and the
> wheel's launcher preserves that. The plan text was the thing out of step, and deciding
> a bind default ahead of the auth posture was premature.

## Rejected alternatives

- **`dev` loopback, `run` wide** — makes the dev loop unable to reach a mesh, the one
  thing a second host needs to see; a developer's first cross-machine test then fails
  for a reason the plan invented.
- **Bind default and auth posture decided separately** — two dials for one concern, and
  the bind dial would be settled first while providing none of the protection, inviting
  a narrow default to stand in for the missing auth story.
- **Narrow default with a mesh opt-in flag** — a new configuration dial, which the
  zero-ceremony bar forbids, and it still answers reachability rather than authority.

## Consequences

- A node on a shared network is reachable by anything that can route to it until the
  auth posture lands. Accepted knowingly: the alternative is a dev loop that cannot do
  what the product is for.
- The auth posture now carries the whole exposure question, including the mesh-facing
  half — it cannot be settled as authentication mechanics alone.
- `--host` stays the per-invocation narrowing lever for anyone who wants it today; it is
  an override, never a default.
