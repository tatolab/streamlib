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

## Decided at the proposal (owner, 2026-10-02)

Two forks the change proposal (`docs/plan/changes/package-split-and-lend.md`) surfaced, ruled
before approval.

**The runtime process is native `tatolabd` from this step on.** Once a stream's venv holds no
engine, something else has to host it, and the native `tatolabd` of step 4 did not exist yet.
`tatolabd` is built now, hosting one stream, started in the foreground by `tatolab run`; the
`tatolab` CLI is native beside it, since the installer ships no Python for a Python CLI to run
on. Nothing registers a service: long term, installing and managing the runtime moves into the
app, and a service now would cause chaos. Step 4 grows this binary to many streams; it does not
replace it. Rejected:
- *A launcher and an empty environment of the runtime's own until step 4.* A bridge built to be
  thrown away. Bridges are never retired: the next session builds on them and reads them as the
  model, and the tree sprawls with an old way beside the new one.
- *The stream's venv interpreter, lent the runtime portion, as the runtime process.* One runtime
  process cannot run from many venvs, so step 4 would delete it.

**No package extends the engine; the capability-extension hook is deleted.** A native runtime
process runs no Python, so the hook's runtime-process call site could not stand. The runtime is
the host and streams are its guests. The decided runtime serves every stream on the machine, from
different people and teams; this step hosts one stream per `tatolabd` and step 4 builds the
sharing, but a door into the engine built now would carry into it, where code inside the runtime
could crash, read or send out every stream's data. Daemons extend the same way: Docker's plugins are separate processes behind a socket, and
Tailscale's CLI and apps drive `tailscaled` over its LocalAPI. A package sets itself up where its
nodes run, at import or on first use; a node's lifecycle methods are unchanged; an outside
program watches the local API's events; a capability the engine needs enters as a built-in.
Rejected:
- *Hooks in processor interpreters only.* What the shipped hooks do there (a TLS provider, a
  network thread pool) a package does at import, so the mechanism carries nothing.
- *An interpreter embedded in `tatolabd` for hooks.* It puts back the Python environment beside
  the runtime that the first ruling declined, to serve capabilities nobody has designed.
