# One shipped binary, embeddable library

Rationale for the `[single-binary-launch]` entry in `docs/plan/ARCHITECTURE.md`
§Control plane & observability, decided 2026-07-29.

## Trigger

Read this before adding a second executable, before wiring `run`/`dev` to spawn a
separate runtime process, or when someone asks whether streamlib can be embedded in
another application.

## Decision

One shipped binary — `streamlib` — bundles the CLI, the runtime host, and build
orchestration. `streamlib run` and `streamlib dev` host the runtime in-process (the
Rust path compiles a generated-main harness around the user's entry; Python/TS entries
run through the subprocess SDK bound to the control plane). The standalone
streamlib-runtime binary retires.

Embeddability is unaffected, because the binary is packaging, not architecture: the
engine remains an ordinary embeddable Rust library that a host application (an
Isaac-Sim-style app, a custom tool) links and drives in-process, and non-Rust hosts
embed by driving a runtime through the client-SDK / control-plane path.

## Rejected alternatives

- **Two binaries (CLI spawns a runtime executable)** — two hosts drift, two artifacts
  ship, and the subprocess indirection buys nothing the in-process host doesn't already
  provide.
- **Binary-only runtime (no library path)** — kills legitimate Rust embedding; the
  library is the substrate, the binary is one consumer of it.

## Consequences

- PyPI ships exactly one artifact; there is no version skew between "the CLI" and "the
  runtime."
- Retiring the standalone runtime binary is engine-tree work to schedule; its
  boot/registry behaviors move into the CLI-hosted path.
- The client SDK (launch + control from Python/TS) is a real surface the plan now
  depends on for the embedding story.
