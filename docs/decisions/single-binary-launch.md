# One shipped binary, embeddable library

Rationale for the `[single-binary-launch]` entry in `docs/plan/ARCHITECTURE.md`
§Control plane & observability, decided 2026-07-29.

## Trigger

Read this before adding a second executable, before wiring `run`/`dev` to spawn a
separate runtime process, or when someone asks whether streamlib can be embedded in
another application.

## Decision

> ~~One shipped binary — `streamlib` — bundles the CLI, the runtime host, and build
> orchestration. `streamlib run` and `streamlib dev` host the runtime in-process (the
> Rust path compiles a generated-main harness around the user's entry; Python/TS entries
> run through the subprocess SDK bound to the control plane).~~ — Superseded 2026-08-02
> by `importable-python-library.md`. The shipped artifact is the PyPI wheel (Python API +
> CLI + engine via PyO3); build orchestration is deleted entirely; Python entries run
> in-process via the wheel, not through a subprocess SDK. The generated-main Rust harness
> is dead — a Rust app is a plain cargo project.

The standalone streamlib-runtime binary retires. What survives of this decision: there
is still exactly one CLI, `run`/`dev` still host the runtime in-process as a thin
runner, and there is no version skew between "the CLI" and "the runtime" — both ship in
the one wheel.

> ~~Non-Rust hosts embed by driving a runtime through the client-SDK / control-plane
> path.~~ — Superseded 2026-08-02 by `importable-python-library.md`. Exactly backwards
> now: Python is the primary host and embeds the engine in-process by importing the
> wheel. The control plane exists to observe and drive running nodes, not to embed.
> Rust embedding is unchanged: the engine remains an ordinary embeddable library.

## Rejected alternatives

- **Two binaries (CLI spawns a runtime executable)** — two hosts drift, two artifacts
  ship, and the subprocess indirection buys nothing the in-process host doesn't already
  provide.
- **Binary-only runtime (no library path)** — kills legitimate Rust embedding; the
  library is the substrate, the binary is one consumer of it.

## Consequences

- ~~PyPI ships exactly one artifact.~~ — Superseded 2026-08-02 by
  `importable-python-library.md`: two artifacts, one version (wheel + `streamlib`
  crate), repo-hosted until the rename. Still no version skew between "the CLI" and
  "the runtime" — both live in the wheel.
- Retiring the standalone runtime binary is engine-tree work to schedule; its
  boot/registry behaviors move into the CLI-hosted path.
- ~~The client SDK (launch + control from Python/TS) is a real surface the plan now
  depends on for the embedding story.~~ — Superseded 2026-08-02 by
  `importable-python-library.md`: Python embeds by importing the wheel; the control
  plane observes running nodes, it does not embed.
