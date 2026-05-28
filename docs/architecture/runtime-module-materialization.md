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
        ┌──────────── streamlib-build-orchestrator ────────────┐
        │  PolyglotBuildOrchestrator : BuildOrchestrator        │
        │   rust   → cargo build + stage cdylib (fingerprint    │
        │            decides staleness — never mtime)           │
        │   python → stage python/ + pyproject + wheels         │
        │   deno   → stage deno/ + deno.json                    │
        │   schema → stage streamlib.yaml + schemas/            │
        │  → <STREAMLIB_HOME>/build-cache/<profile>/<org>__<name>/│
        │    (build-to-temp + atomic rename + .streamlib-build  │
        │     sidecar: abi_version, triple, profile)            │
        └───────────────────────────────────────────────────────┘
```

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
- `IfStale` — (re)build iff the build tool's own fingerprint reports
  changed inputs (near-instant when clean). The dev / runtime-authoring
  default.
- `AlwaysBuild` — invoke the tool unconditionally (the tool may still
  short-circuit its compilation).

### Staleness is the build tool's fingerprint — never mtime

This is the load-bearing correctness rule. The original trap was a
staleness bug whose triggering edit lived in a **transitive dependency**
of the cdylib (an engine-reachability change), not the package's own
source. An mtime check (`source newer than .so?`) misses exactly that
edit class and would re-ship the bug. cargo's own fingerprint already
tracks the full transitive dep graph + features + rustflags + rustc
version, so `IfStale` is implemented by **always invoking the tool and
letting it short-circuit** — there is no engine-side staleness predicate
to be wrong. Editing `streamlib-plugin-abi` (the FFI vtable contract)
rebuilds every dependent plugin on next load for the same reason: cargo
sees the dep graph changed.

### No-orchestrator behavior: fail loud, no branching

If a build-requiring policy (`IfStale` or `AlwaysBuild`) is reached with
**no** orchestrator wired, the load fails loud
(`BuildRequiredButNoOrchestrator`) — consistently, with no branching on
package shape. Future agents get a clear signal instead of a
silently-loaded, possibly-stale artifact. A no-build deployment uses
`NeverBuild` / `InstalledCache` / `.slpkg`; a building one wires an
orchestrator (the SDK `auto-build` default does so).

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

Parallelism is whatever the tools allow. Cross-language builds run
concurrently (a Rust `cargo build`, a Python wheel, a Deno bundle have
no shared lock). Rust-vs-Rust against one workspace `target/` serialize
on cargo's build lock — batching co-located Rust packages into one
`cargo build -p A -p B` invocation is the way to parallelize those, and
is a future refinement of the orchestrator.

## Profile parity

The orchestrator builds packages with the **host binary's** compiled
profile (`cfg!(debug_assertions)` → dev, else release), keyed into the
cache slot (`build-cache/<profile>/…`) so dev and release artifacts
don't clobber each other. This is a consistency / perf-sanity choice,
not an ABI requirement — `#[repr(C)]` makes debug and release cdylibs
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
- Default orchestrator: `libs/streamlib-build-orchestrator/`.
- SDK wiring: `libs/streamlib-sdk/` (`auto-build` feature).
- Plugin ABI + load-time handshake: `libs/streamlib-plugin-abi/`
  (`STREAMLIB_ABI_VERSION`, `PluginDeclaration.abi_version`).
