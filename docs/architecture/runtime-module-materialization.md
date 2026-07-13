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
│               | Git{url,rev,build} | Url{url,build,checksum} }                          │
│    resolve → ResolvedSource::Ready(dir)            (no build needed)                    │
│            → ResolvedSource::NeedsBuild(BuildRequest)  (build required)                 │
│    recursive transitive-dep walk (cycle-detected), identity + semver validation,       │
│    schema + processor STAGING (dlopen the cdylib via STREAMLIB_PLUGIN), then ONE        │
│    whole-load commit into the global registries — see "Transactional registration".    │
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
        │   routine `streamlib pkg build` uses) targeting an extracted    │
        │   StagedDir:                                               │
        │     rust   → cargo build → cdylib at lib/<triple>/         │
        │     python → full source tree (.py + data/assets +         │
        │              pyproject + uv.lock) — NO wheel               │
        │     deno   → entrypoint under deno/                        │
        │     always → streamlib.yaml + schemas/                     │
        │  → <STREAMLIB_HOME>/.streamlib/cache/packages/<name>-<ver>/│
        │    (build-to-temp + atomic rename + .streamlib-build       │
        │     sidecar: abi_version, triple, profile, inputs_hash)    │
        │    — byte-identical to extracting a .slpkg / GitHub install│
        └────────────────────────────────────────────────────────────┘
```

There is ONE materialization path, shared with `streamlib pkg build` and
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
models + `pyproject.toml`, and `uv.lock` if present) and builds no wheel.
The install side runs `uv pip install -e <staged dir>`, which resolves
dependencies from `pyproject.toml` (not the lockfile — `uv pip install`
doesn't consume `uv.lock`; it's bundled to travel with the package). The
dependency venv is therefore keyed by `pyproject.toml` contents, so
editing a `.py` reuses the venv (deps install once — never a per-edit
reinstall) and the edited source is read directly from `PYTHONPATH`. Shipping identical source in dev and in the `.slpkg` also means
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
| `InstalledCache` | `<STREAMLIB_HOME>/.streamlib/cache/packages/…` | only if Rust source + no matching prebuilt |
| `Path { path, build }` | a directory with `streamlib.yaml` | per `build` |
| `Slpkg { path }` | a `.slpkg` archive (engine extracts) | only if Rust source + no matching prebuilt |
| `Git { url, rev, build }` | a git checkout (engine fetches) | per `build` |
| `Url { url, build, checksum }` | a remote `.slpkg` (engine fetches `file://`/`http(s)://`, optional checksum pin) | prefer-prebuilt, else per `build` |

`add_module(ident)` is the conservative default — `Strategy::InstalledCache`.
Anything rebuildable-from-source via `Path`/`Git` is requested
**explicitly** with a `BuildPolicy`, so a stale artifact can't be silently
loaded.

### A `.slpkg` carries source and/or a prebuilt — host prefers prebuilt, else builds

A `.slpkg` (and an `InstalledCache` entry) is a box that can hold **source
and/or a prebuilt cdylib**, like a pip package shipping both a wheel and an
sdist. `assemble_artifact` bundles, for a Rust package, *both* the crate
source (`Cargo.toml` + `src/` …) and the prebuilt cdylib for the packing
host's triple. On load, the resolver (`source_for_resolved_dir`) decides:

```
manifest has Rust processors?
  no  → load as-is (Python/Deno/schema run from source)
  yes → lib/<this-host-triple>/*.so present?
          yes → load it           (compiler-free, instant)
          no  → Cargo.toml present?
                  yes → build it on this host → cdylib
                  no  → load as-is → dlopen fails loud (no artifact, no source)
```

So one artifact runs everywhere: a host on the packing triple uses the
prebuilt with **no compiler**; a host on a different triple (or one handed
a source-only box) compiles the bundled source for itself. The
compiler-free frozen deployment is preserved — it just requires the box to
carry a prebuilt for its triple; a source-only box on a host with no
orchestrator fails loud (`BuildRequiredButNoOrchestrator`). A Rust crate
whose Cargo deps are path/workspace-only builds only where those resolve
(the same constraint crates.io enforces) and relies on its bundled
prebuilt for its own triple.

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

## Transactional registration — staged commit

A top-level load never writes the process-global registries mid-walk.
Every registration the walk produces — schema bodies, processor
descriptors + constructors/vtables, dlopen'd plugin images — lands in a
per-load staging buffer (`ModuleLoadRegistrationStaging`), and a single
**whole-load commit** applies it after the ENTIRE transitive walk
succeeded. The invariant: **visible ⇒ permanently committed**. A load
that fails at any phase (manifest parse, schema read, dep walk, dlopen,
declared-but-not-registered validation, processor construction, or the
end-of-walk processor-collision gate below) drops the staging buffer —
the schema and processor registries are byte-equivalent to before the
attempt, so a retried `add_module` re-runs the full resolution with zero
"already registered" residue.

One end-of-walk gate runs before the commit lock is taken: a staged
subprocess (Python / TypeScript) processor whose composed
`processor_type` ident is **duplicated within the load** or **already
present in the global registry** fails the whole load loud with a typed
`AddModuleError` (`DuplicateProcessorTypeInModule` /
`ProcessorTypeAlreadyRegistered`). Without it, a manifest declaring two
same-named subprocess processors would stage the ident twice and the
second `register_dynamic` would error mid-commit — after the first
already applied — yielding a silently-incomplete load that returned Ok.
(Rust/vtable duplicates are idempotently deduped at commit and need no
gate.) This gate is what makes the commit itself
infallible-by-construction.

Cdylib registrations are intercepted at the host-services layer: the
loader installs a thread-local staging sink around the plugin's
`(decl.register)(...)` call (the registration prologue runs
synchronously on the same thread), so the host's `schema_register` /
`processor_register` callbacks stage into the active load instead of
writing the global registries; `schema_lookup` overlays the active
load's staged schemas so a prologue sees what its own load staged. No
plugin ABI change — a cdylib registering outside a module load (no
sink installed) still writes direct-to-global.

The commit runs under a process-wide commit lock, in order: retained
plugin images → schemas → processors → per-package ledger records +
resolution-memo flips → concurrent-load completion signals. Schemas
commit before processors so a racing reader can only ever observe
"schemas without processors" (reads as module-not-loaded-yet, benign
and retryable), never processors whose port schemas are missing. The
commit itself is infallible by construction — everything fallible
happened during the walk.

The resolution memo's per-package placeholders also flip at the
whole-load commit, which strengthens the concurrent-load contract: a
load that skipped a package another load had in flight succeeds only
when that owner's ENTIRE load commits. Two loads that each skip a
package the other owns wait on each other after their own walks; that
mutual skip is bounded by the skipped-in-flight timeout and surfaces
as a typed error. A same-load diamond re-encounter (A→{B,C}→D) is
recognized by owner-load-id and skipped with no wait entry — a load
never waits on itself.

### Dylib images are retained for the process lifetime

`dlclose` is never called — on load failure or on `remove_module`.
Registered processor vtables, `'static` descriptor strings handed
across the plugin ABI, and the host-service bridge state installed by
`install_host_services` all point into the mapped image; unmapping it
would dangle them behind safe interfaces. Retained images are recorded
(`RetainedPluginLibrary`: handle, path, first-loading package) and
deliberately **never deduplicated by path** — a rebuilt plugin at the
same path is a NEW image, and dropping the "duplicate" handle would
dlclose a live image out from under its vtables. The cost is bounded:
one mapped `.so` per (path, load) that actually dlopen'd, typically a
handful per process lifetime. `install_host_services` is idempotent,
so re-registering a retained image on a later load is safe.

## `remove_module` — unload as registration removal

`Runner::remove_module(ident)` unloads a committed package: its
processor types leave the processor registry, its package-owned schemas
(`@org/name/Type` ids) leave the schema registry, its ledger record and
the calling Runner's resolution-memo entry are cleared so a later
`add_module` re-resolves from scratch, and a
`RuntimeDidUnregisterProcessorType` event publishes per removed
processor type. The dylib image stays retained (above) — unload means
registration removal, not image unmapping. Legacy reverse-DNS schema
ids are deliberately left in place: they can be registered by multiple
packages, so removal of one owner must not break the others.

Removal is refused with a typed error — leaving every registry exactly
as it was — when:

- no committed load matches the ident (or the loaded version doesn't
  satisfy the requested range; the error names what IS loaded),
- a load of the module is still in flight (top-level or as a dependency
  mid-walk),
- other loaded modules still require it (removal never cascades —
  remove the requirers first), or
- graph processors still instantiate its types. This check is
  TOCTOU-closing: the types are unregistered FIRST (a racing
  `add_processor` gets a registry miss, never a half-removed type),
  the graph is scanned, and on a hit the registrations are restored
  before the error returns. Nodes already marked pending-deletion are
  excluded — the compiler never spawns one. During the brief
  unregister-scan-restore window of a *refused* in-use removal, a
  concurrent same-type `add_processor` can transiently observe the
  registry miss and fail with the typed `UnknownProcessorType` — a
  recoverable `Err` (retry after the removal returns), consistent with
  the same-process-registry multi-Runner limitation noted below, not a
  registry corruption.

The commit ledger (`LOADED_MODULE_REGISTRATION_LEDGER`, process-global,
keyed by `@org/name`) records what each committed package registered —
schema ids, processor idents, dylib paths, and which loaded packages
require it — and is the source of truth removal consumes. Note the
registries and the ledger are process-global while the resolution memo
is Runner-scoped: with multiple `Runner`s in one process, a removal on
one Runner unregisters types globally but only clears the calling
Runner's memo. Single-Runner-per-process is the supported shape today;
the asymmetry is a documented limitation, not a designed feature.

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
| Frozen container, prebuilt `.slpkg` for its triple | no orchestrator wired (`--no-default-features`); `Slpkg`/`InstalledCache` finds the matching prebuilt cdylib and loads it; provably compiler-free |
| `.slpkg` from another platform (or source-only) on a fresh triple | extract → no matching prebuilt → orchestrator `cargo build`s the bundled source for this host → load. One artifact, every platform |
| "point at this GitHub repo for @org/pkg" | `Strategy::Git { url, rev, build }` — engine fetches the checkout, orchestrator builds it |
| "install this `.slpkg` from a URL" | `Strategy::Url { url, build, checksum }` — engine fetches the archive (`file://`/`http(s)://`) into the resolver cache, verifies the optional checksum, extracts, then resolves prefer-prebuilt-else-build like a local `.slpkg`. Re-fetch of the same URL skips the download |
| Mixed (local + installed + slpkg) | each top-level call and each transitive dep derives its own `Strategy`; `await_modules` runs them concurrently |
| CI cold cache | orchestrator wired; cold builds run (and parallelize across languages) |

## Reference

- Engine API: `libs/streamlib-engine/src/core/runtime/module_loader/`
  (`source.rs` = `Strategy` + resolver, `build_orchestrator.rs` = the
  trait + request/result/event types, `added_module.rs` = the eager
  future, `recursive_walker.rs` = the transitive walk + materialize step,
  `staging.rs` = the per-load registration staging buffer + thread-local
  cdylib sink + whole-load commit + plugin-image retention, `ledger.rs` =
  the committed-load ledger `remove_module` consumes, `mod.rs` = the
  `Runner` API incl. `remove_module`).
- Default orchestrator: `libs/streamlib-build-orchestrator/` (calls
  `streamlib-pack` and stages into `cache/packages/<name>-<version>/`).
- Shared assembly: `libs/streamlib-pack/` (`assemble_artifact` —
  emits a `.slpkg` for `streamlib pkg build` or an extracted `StagedDir` for
  the orchestrator).
- Python venv provisioning (the tail of `materialize`: `uv venv` →
  `uv pip install` → `streamlib/_generated_` codegen → `compileall`,
  produced into the staged `{name}-{version}/.venv` under the
  orchestrator's single fingerprint):
  `libs/streamlib-build-orchestrator/src/python_venv.rs`.
- SDK wiring: `libs/streamlib-sdk/` (`auto-build` feature).
- Plugin ABI + load-time handshake: `libs/streamlib-plugin-abi/`
  (`STREAMLIB_ABI_VERSION`, `PluginDeclaration.abi_version`).
