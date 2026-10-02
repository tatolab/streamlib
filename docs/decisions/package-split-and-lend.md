# The package split and the lend

Rationale for the `[package-split-and-lend]` entries in `docs/plan/ARCHITECTURE.md` §Packages &
extension model and §Processor model & scheduling, decided 2026-10-02 — step 3 of the
one-runtime-per-machine pivot.

## Trigger

Read this before putting anything of the engine into a stream's venv, before making a stream
state which runtime it needs, before importing project code in the runtime process, and before
shipping a `tatolab/__init__.py`.

## Decision

Two packages under one PEP 420 namespace. `tatolab-stream` (`tatolab.stream`) is pure Python and
is everything a stream module imports, so a stream compiles with no runtime present.
`tatolab.runtime` is the engine's native part and ships only with the runtime. The runtime lends
its lend directory — the one holding `tatolab/runtime/` — to each processor interpreter it starts
from the stream's venv, prepended to `PYTHONPATH`. A stream never names a runtime version; the runtime refuses by name what it does
not understand in a graph, and a newer runtime loads every older graph. A stream's environment
is recorded beside its graph, never inside it.

## Rejected alternatives

- **Every stream venv pins the exact runtime** (today's shape). Every runtime update reinstalls
  every stream, and the heaviest artifact lands in every venv on low-power devices.
- **An "API level"** a runtime declares and a stream library requires. A second version number
  beside the real ones, carried by streams, for a check the runtime already makes by refusing
  what it does not understand. Owner, 2026-10-02: a stream loads into whatever runtime is on the
  machine; there is no version dependency to declare.
- **Guarding against a stale runtime copy in a stream's venv.** Residue of the pre-pivot shape:
  the runtime is never a pip package, and the old all-in-one wheel is named `streamlib`, which
  does not share the namespace. The exact-build handshake stays as the backstop; nothing new.
- **A versioned wire or a stable C ABI between runtime and interpreter.** The deleted plugin
  ABI returning; the lend keeps one build on both sides by construction.
- **The runtime process importing project code to learn a node's ports.** It would bind the
  runtime to one environment; describing happens in the stream's own interpreter.

## Consequences

- Compiling a stream runs the project's interpreter; the CLI and the app both hand the runtime a
  graph, a project directory and an interpreter path, never source.
- After this step the engine never comes from pip. Until the installer exists, developers and CI
  run the runtime from a local build.
- The regular-package rules (no `tatolab/__init__.py`; `tatolab/runtime` regular) are what keep
  the merge deterministic (PEP 420: a regular package ends the path scan); #2561's research.
- The lent directory carries whatever the native part resolves relative to itself — on macOS
  the bundled Vulkan driver — so a processor interpreter finds it from the lend.
- The graph is a durable contract read by newer runtimes; a field a runtime does not know is a
  refusal by name, never silently ignored.
