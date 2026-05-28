# Runtime module materialization

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).

## What this is

How `Runner` turns a module identity into a loaded, registered package
at runtime — finding the source, optionally **building it from source**,
and loading the staged result. The same mechanism serves the dev inner
loop, an AI agent / CLI / daemon authoring new Rust/Python/Deno packages
on the fly, and a frozen `.slpkg`-only production deployment. There is
ONE path; the only axis that varies is whether a builder is wired.

The engine is a pure substrate: it resolves *where* a package's source
lives and *loads* the staged result, but it **never invokes a toolchain
itself**. Building is an injected capability — a `BuildOrchestrator` the
deployment chooses to wire (or not).

## The two layers

```
 caller (runner app / CLI / daemon / AI host)
   │
   │  runtime.add_module(ident)                         ← conservative: InstalledCache only
   │  runtime.add_module_with(ident, Strategy)          ← explicit source + build policy
   │     └─ returns AddedModule : Future (eager — work spawns at call time)
   │  runtime.await_modules([a, b, …], on_event).await  ← concurrent, interleaved progress
   │  runtime.add_module_blocking(ident)                ← sync convenience (typed err in async ctx)
   ▼
┌──────────────────────── streamlib-engine (RESOLUTION + LOAD) ─────────────────────────┐
│  Pure filesystem / cache / git — NEVER cargo/pip/deno.                                 │
│    Strategy { InstalledCache | Path{path,build} | Slpkg{path}                          │
│               | Git{url,rev,build} | Url{url,build} }                                  │
│    resolve → ResolvedSource::Ready(dir)            (no build needed)                    │
│            → ResolvedSource::NeedsBuild(BuildRequest)  (build required)                 │
│    recursive transitive-dep walk (cycle-detected), identity + semver validation,       │
│    schema + processor registration (dlopen the cdylib via STREAMLIB_PLUGIN).           │
│                                                                                        │
│  When NeedsBuild and an orchestrator IS wired → call materialize() (on spawn_blocking).│
│  When NeedsBuild and NO orchestrator → fail loud (BuildRequiredButNoOrchestrator).     │
│  declares: trait BuildOrchestrator + BuildRequest/BuildSource/StagedArtifact/          │
│            BuildEvent/BuildEventSink/BuildError; holds Option<Arc<dyn BuildOrchestrator>>│
└──────────────────────────────────────────┬─────────────────────────────────────────────┘
                                            ▼  (injected; lives OUTSIDE the engine)
        ┌──────────── streamlib-build-orchestrator ─────────────────┐
        │  PolyglotBuildOrchestrator : BuildOrchestrator             │
        │   calls streamlib-pack::assemble_artifact (the SAME        │
        │   routine `streamlib pack` uses) targeting an extracted    │
        │   StagedDir:                                               │
        │     rust   → cargo build → cdylib at lib/<triple>/         │
        │     python → full source tree (.py + data/assets +         │
        │              pyproject + uv.lock) — NO wheel               │
        │     deno   → entrypoint under deno/                        │
        │     always → streamlib.yaml + schemas/                     │
        │  → <STREAMLIB_HOME>/cache/packages/<name>-<version>/       │
        │    (build-to-temp + atomic rename + .streamlib-build       │
        │     sidecar: abi_version, triple, profile, inputs_hash)    │
        │    — byte-identical to extracting a .slpkg / GitHub install│
        └────────────────────────────────────────────────────────────┘
```

There is ONE materialization path, shared with `streamlib pack` and
with installing from a `.slpkg` / GitHub repo. The orchestrator assembles
the *complete* artifact (Rust cdylib, full Python source, Deno bundle,
schemas) via [`streamlib-pack`] and stages it as an extracted directory
into the package cache — the same `cache/packages/<name>-<version>/`
location an extracted `.slpkg` lands in. Because dev, release, and
install-from-anywhere produce the identical artifact shape, a package
that loads in dev cannot silently break when distributed. The shared
assembly lib is the programmatic equivalent of the manual pack/build/
install steps, which is also what a future build *daemon* calls through
the same `BuildOrchestrator` seam.

**Python ships as source, not a wheel.** A Python processor runs from
its source dir (`PYTHONPATH = <staged package dir>`), never from a
pip-installed copy — so the engine only needs the package's
*dependencies* installed, not the package itself wheel-packed. Assembly
therefore bundles the **full source tree** (every `.py` + data / assets /
models + `pyproject.toml` + `uv.lock`) and builds no wheel. The install
side caches a dependency venv keyed by the dependency closure
(`pyproject.toml` contents), so editing a `.py` reuses the venv (deps
install once — never a per-edit reinstall) and the edited source is read
directly. Shipping identical source in dev and in the `.slpkg` also means
there is no "imports in dev, missing from the wheel" packaging skew. (A
package that pre-ships `python/wheels/*.whl` keeps it — the full-source
copy includes it and the install side may prefer it — but assembly never
*builds* one.)

[`streamlib-pack`]: https://docs.rs/streamlib-pack

The SDK wires this behind the `auto-build` feature (on by default).
Because `Runner` lives in the engine and the default orchestrator lives
downstream (the engine can't depend on it — it shells out to cargo), the
SDK provides the wiring, not the engine: the common path is
`Runner::with_auto_build()` (an SDK extension trait, `RunnerAutoBuild`,
that calls `Runner::new_with_orchestrator(PolyglotBuildOrchestrator::default())`).
`Runner::new()` stays orchestrator-free for frozen / custom deployments;
`new_with_orchestrator(impl)` injects a non-default builder (e.g. a future
IPC build-service). `cargo build --no-default-features` excludes the
orchestrator crate from the dependency graph entirely, so a frozen
deployment is **provably compiler-free** (`cargo tree` shows zero build
tooling), not merely "doesn't call cargo at runtime."

## `Strategy` and `BuildPolicy`

`Strategy` (in `streamlib::sdk::runtime`) names where a module's source
comes from:

| Variant | Source | Builds? |
|---|---|---|
| `InstalledCache` | `~/.streamlib/cache/packages/…` | never |
| `Path { path, build }` | a directory with `streamlib.yaml` | per `build` |
| `Slpkg { path }` | a `.slpkg` archive (engine extracts) | never |
| `Git { url, rev, build }` | a git checkout (engine fetches) | per `build` |
| `Url { url, build }` | a remote archive | per `build` (orchestrator fetches) |

`add_module(ident)` is the conservative default — `Strategy::InstalledCache`,
never builds, fails loud (`ModuleNotFound`) if absent. Anything
rebuildable-from-source is requested **explicitly** through `Path`/`Git`,
so a stale artifact can never be silently loaded.

`BuildPolicy`:

- `NeverBuild` — load the staged artifact as-is; never invoke a builder.
- `IfStale` — (re)build iff inputs changed (see staleness below). The
  dev / runtime-authoring default.
- `AlwaysBuild` — build unconditionally (each tool may still short-circuit
  its own compilation).

### Staleness — language-agnostic, never mtime

`IfStale` staleness splits by whether the package contains Rust, because
the two cases have different correct oracles:

- **No Rust** (Python / Deno / schemas-only): the orchestrator's **own
  content fingerprint of the package's source inputs** — every source
  file under the package dir (`python/`, `ts/`, `schemas/`, the
  manifests), excluding build artifacts (`target/`, staged `lib/`), VCS,
  and caches; recorded in the staged `.streamlib-build.json` sidecar. On
  the next load it recomputes: unchanged (+ matching ABI/triple/profile)
  → skip; changed → re-assemble + re-stage. A package-local fingerprint
  is *complete* here — nothing links code outside the package — so this
  works for a standalone package repo with no enclosing workspace, and
  never touches cargo.

- **Contains Rust**: the package **always** re-assembles, i.e. always
  runs `cargo build`. A Rust cdylib can link code *outside* the package
  dir (the engine, in a dev workspace) that a package-local fingerprint
  can't see — the original trap's triggering edit was exactly such a
  transitive change. cargo's own fingerprint is the only correct oracle
  (it catches own + transitive changes, incl. `streamlib-plugin-abi`
  edits, and short-circuits cheaply when clean). The content fingerprint
  is still recorded in the sidecar for the toolchain-context check.

mtime is never used.

The Python *venv* has its own gate. Because Python ships as source and
runs from `PYTHONPATH`, the dependency venv is keyed by the dependency
closure (`pyproject.toml` contents), so a `.py` edit reuses it (deps
install once). For the rarer case where a package pre-ships a wheel, the
venv cache key hashes the wheel **bytes** (not the filename) — a rebuilt
same-version wheel keeps the same filename, so a filename-keyed venv
would hit a stale install and silently run old code, re-opening the trap
one layer up.

### Fast-fail on missing tooling

Before invoking a builder, assembly preflights it (Rust → `cargo`). A
missing toolchain surfaces as an actionable error *before* any build
attempt rather than a raw spawn failure mid-build. Python needs no
build tool at assembly time (source is copied, not compiled); its
dependency install happens at load time via `uv`.

### No-orchestrator behavior: fail loud, no branching

If a build-requiring policy (`IfStale` or `AlwaysBuild`) is reached with
**no** orchestrator wired, the load fails loud
(`BuildRequiredButNoOrchestrator`) — consistently, with no branching on
package shape. Future agents get a clear signal instead of a
silently-loaded, possibly-stale artifact. A no-build deployment uses
`NeverBuild` / `InstalledCache` / `.slpkg`; a building one wires an
orchestrator (`Runner::with_auto_build()`).

## The eager async surface

`add_module*` return an `AddedModule` that implements `Future`. The load
runs on the runtime's existing tokio handle from the moment the call
returns (no second runtime — `Runner` already auto-detects owned vs
borrowed tokio). So issuing N loads kicks off up to N concurrent
resolutions/builds before the caller awaits anything.

- `await_modules([a, b, …], on_event)` drives a batch concurrently and
  fires `on_event(ModuleLoadEvent)` per event **interleaved across
  modules as they happen** (not one module at a time). Build logs stream
  through as `ModuleLoadEvent::BuildLog`.
- `add_module_blocking` / `add_module_with_blocking` are sync
  conveniences; they return `AddModuleError::BlockingCallFromAsyncContext`
  (never panic) when called from inside a tokio runtime.
- `AddedModule` exposes `ident()`, `progress()` (a broadcast receiver),
  and `cancel()`. Dropping it cancels the load (`#[must_use]` catches
  accidental fire-and-forget).
- `start()` hard-errors `ModulesStillLoading { idents }` if the graph is
  run while any load is still in flight.

## Build parallelism

Parallelism is whatever the tools allow. Cross-package work runs
concurrently (a Rust `cargo build`, a Python source copy, a Deno bundle
have no shared lock). Rust-vs-Rust against one workspace `target/` serialize
on cargo's build lock — batching co-located Rust packages into one
`cargo build -p A -p B` invocation is the way to parallelize those, and
is a future refinement of the orchestrator.

## Profile parity

The orchestrator builds packages with the **host binary's** compiled
profile (`cfg!(debug_assertions)` → dev, else release). The package cache
slot (`cache/packages/<name>-<version>/`) is shared with `.slpkg`
installs, so it is *not* keyed by profile; the `.streamlib-build.json`
sidecar records the profile, and a profile mismatch is treated as stale
(re-assemble). This is a consistency / perf-sanity choice, not an ABI
requirement — `#[repr(C)]` makes debug and release cdylibs
cross-loadable. Overridable via `PolyglotBuildOrchestrator::with_profile`.

## FFI / ABI sync

Engine ↔ plugin stay in lock-step on two complementary layers:

1. **Build-time** — in a workspace, cargo unifies `streamlib-plugin-abi`
   to one version, so a plugin the orchestrator builds links the exact
   ABI the running engine was built against. Editing the ABI crate
   rebuilds every dependent plugin on next load (the `IfStale`
   fingerprint sees the transitive dep change). Plugins can't drift from
   the engine for rebuildable sources.
2. **Load-time** — `export_plugin!` stamps
   `PluginDeclaration.abi_version = STREAMLIB_ABI_VERSION`; the dlopen
   shim validates it before invoking `register`. Frozen / cross-repo
   artifacts built against a stale ABI are rejected loud at load, never
   crash. The orchestrator's `.streamlib-build.json` sidecar records the
   ABI version as defense-in-depth.

## Scenario matrix

| Scenario | How it resolves |
|---|---|
| Local dev edit→run | runner wires `PolyglotBuildOrchestrator`; `Strategy::Path { build: IfStale }` rebuilds changed packages (incl. transitive) via cargo's fingerprint |
| AI agent / CLI / daemon authoring a package at runtime | same: write source → `add_module_with(Path/Git, IfStale)` → orchestrator builds → load. This is production, not a dev convenience |
| Frozen `.slpkg`-only container | no orchestrator wired (`--no-default-features`); loads `InstalledCache` / `Slpkg` only; provably compiler-free |
| "point at this GitHub repo for @org/pkg" | `Strategy::Git { url, rev, build }` — engine fetches the checkout, orchestrator builds it |
| Mixed (local + installed + slpkg) | each top-level call and each transitive dep derives its own `Strategy`; `await_modules` runs them concurrently |
| CI cold cache | orchestrator wired; cold builds run (and parallelize across languages) |

## Reference

- Engine API: `libs/streamlib-engine/src/core/runtime/module_loader/`
  (`source.rs` = `Strategy` + resolver, `build_orchestrator.rs` = the
  trait + request/result/event types, `added_module.rs` = the eager
  future, `recursive_walker.rs` = the transitive walk + materialize step,
  `mod.rs` = the `Runner` API).
- Default orchestrator: `libs/streamlib-build-orchestrator/` (calls
  `streamlib-pack` and stages into `cache/packages/<name>-<version>/`).
- Shared assembly: `libs/streamlib-pack/` (`assemble_artifact` —
  emits a `.slpkg` for `streamlib pack` or an extracted `StagedDir` for
  the orchestrator).
- Python venv (wheel-bytes cache key):
  `libs/streamlib-engine/src/core/compiler/compiler_ops/spawn_python_subprocess_op.rs`.
- SDK wiring: `libs/streamlib-sdk/` (`auto-build` feature).
- Plugin ABI + load-time handshake: `libs/streamlib-plugin-abi/`
  (`STREAMLIB_ABI_VERSION`, `PluginDeclaration.abi_version`).
