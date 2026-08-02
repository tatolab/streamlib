# Change: mvp-app-experience

> **Partially superseded 2026-08-02 by `importable-python-library.md`.** Its
> package-source, discovery-scan, string-id, and subprocess-execution sections are dead;
> the `app.py`/`setup(rt)` convention, `streamlib new`, and class-form `rt.add` survive
> and are restated by the pivot change. `/reconcile-tracker` sorts its tickets
> accordingly; derive nothing new from this file.

Implements §Product's sentence terms — `streamlib new`, the `app.py`/`setup(rt)` entry
convention, app-local processors, string-or-class `rt.add` — and §Control plane's
"`run`/`dev` host the runtime in-process" entry. ADRs: `product-mvp-sentence.md`,
`single-binary-launch.md` (both exist; no new ADR — no ABI/RHI/wire touch). Recon
verified all cited APIs at file:line on 2026-07-30.

## Behavior after this change

`pip install streamlib` → `streamlib new my-app` → `streamlib dev` shows the user's
camera live in a window, before they write any code. The scaffolded app is an entry file
plus a wired-in editable effect processor — no manifest. `dev`/`run` with no args find
`app.py`'s `setup(rt)` at the anchor dir (`-f <file>` overrides). The launched node
hosts the control plane and appears in `streamlib nodes`, so `graph`/`tap` work against
it. App-local processors under `<app>/processors/` register as `@app/local/<Name>` and
are addressable from `setup(rt)` by string id or imported class.

## ADDED

- ADDED: `streamlib new [dir]` — batteries-included app scaffold. Interactive with
  every prompt flag-backed (same posture as the planned `plugin new`). Emits `app.py`
  (a `setup(rt)` wiring camera → effect → display), `processors/<effect>.py` (working,
  editable, `@processor`-decorated with no explicit identity → `@app/local/<Name>`),
  and `.gitignore` — **no `streamlib.yaml`**, landing in the manifest-less app bucket
  `add.rs:183-191` already treats as "an app is code, not a manifest". At scaffold
  time it installs `@tatolab/camera` and `@tatolab/display` via
  `AppModulesDir::acquire_from_package_source` (`app_modules.rs:1156`) — the
  version-coordinate path; `AddPackageSource::detect` rejects bare coordinates by
  design — then builds slots via `build_added_slot_or_rollback`
  (`build_on_place.rs:82`) with a scaffold-appropriate `BuildEventSink` (the trait is
  already object-safe; only `ConsoleBuildEventSink` assumes a terminal).
- ADDED: the entry convention — `streamlib run` / `streamlib dev` with no args resolve
  `app.py` at the anchor dir (`add.rs:175-180` semantics: `--dir` else cwd, no
  walk-up) and call its `setup(rt)`; `-f <file>` overrides. Extends #1600/#1601, which
  are `-f`-only today. Nothing currently reads any app entry convention
  (recon-verified; `STREAMLIB_ENTRYPOINT` is per-processor-subprocess, not per-app).
- ADDED: `run`/`dev` host the control plane in-process — absorb the
  `streamlib-runtime` boot recipe (`runtime/streamlib-runtime/src/main.rs:68-112`:
  register `ApiServerProcessor`, `add_processor` with host/port/log-path config,
  `start`, `wait_for_signal`) into the CLI-hosted path, so the node writes a registry
  entry (`api_server.rs:260-264`) and is drivable by `graph`/`tap`/`streamlib nodes`.
  The `mcp` in-process path (`mcp.rs:44-68`) does none of this today and stays as-is.
- ADDED: app-local processor loading — the runtime loads `<app>/processors/` as the
  app's local unit through the existing extractor-driven package path, the same shape
  `register_processor_from_source` already synthesizes (staged one-package tree with
  `processors/` beneath it, `from_source.rs:44-59`), registering each discovered
  processor as `@app/local/<Name>`. Today nothing reads `@app/local/*` at runtime —
  it is authoring-time synthesis only (`decorators.py:187-204`; zero hits in
  `runtime/`) — so this is new module-loader surface, not reuse. `@session/…` minting
  stays exactly as-is for live submissions and `add_local::<P>` host types
  (`session.rs:8-9` states the two identities are deliberately distinct).
- ADDED: string-id `rt.add` on the client surfaces. The Python/Deno `setup(rt)` handle
  (#1601) accepts `"@org/package/Type"` (Python decorators already parse this string
  shape at authoring time) and the imported class (reads the identity the decorator
  attached). Rust gains `TryFrom<&str> for ProcessorTypeReference` **in the engine's
  processor module** — today the only conversion is `From<SchemaIdent>`
  (`processor_type_reference.rs:80-84`). The `streamlib-idents` no-parse doctrine
  (`ident.rs:314-324` compile_fail locks) stands untouched: the parse lives at the
  wiring surface, never on `SchemaIdent`/identity types.
- ADDED: `dev`'s watch loop needs a filesystem-watch dependency — none exists in the
  workspace (recon: no `notify`/`watchexec`/etc. in `Cargo.lock`). Dependency choice
  at implementation; pinned per the git-deps rule if git-sourced.

## MODIFIED

- MODIFIED: #1600 / #1601 ticket scope — the no-args `app.py` convention layers onto
  their `-f` harnesses (bodies updated at derive time, originals preserved).
- MODIFIED: `dev` hot-reload sequencing constraint (for #1602, already rewritten to
  app scope): `replace` is type-level — a running instance keeps prior source until
  removed and re-instantiated (`main.rs:189-192`), so the app-local edit loop is
  replace → remove → re-add → re-connect, not replace alone.
- MODIFIED: `logging_config_for` (`main.rs:549-555`) — `new`/`run`/`dev` take the
  default stdout mirror; only stdio-protocol verbs route to stderr. No structural
  change, listed for completeness.

## REMOVED

- None. This change is purely additive; the standalone runtime binary's retirement
  belongs to the engine-topology change (next bundle), not this one.

## Default package source — DECIDED by owner (2026-07-30, in-session)

`streamlib new` must install `@tatolab/camera` + `@tatolab/display`, but
`PackageSource::from_env()` (`package_source.rs:86-93`) has no default by design. The
owner resolved the gap: **the CLI ships a baked-in default first-party package-source
URL pointing at a CI-published static tree on GitHub Pages**; `STREAMLIB_PACKAGE_SOURCE`
overrides it (dev loops unchanged); the host behind the URL is migratable (Releases /
R2) in a one-line default change with a CLI release. Not a registry — files on a dumb
host, published by CI from the tree `pkg publish` already emits.

- ADDED: the default-URL constant and its resolution order (`env override → default`)
  at the package-source seam; the "unset env is no source" doc
  (`package_source.rs:84-88`) is superseded for the first-party default only —
  explicit overrides still win, and a *configured-but-unreachable* source still errors
  rather than silently falling back.
- ADDED: a release-workflow step publishing the static package-source tree to the
  Pages branch (automated; no manual upkeep). Rejected alternatives — bundle-in-wheel
  (fat artifact, frozen versions, second install path) and require-env-first (ceremony
  as an environment variable) — recorded here.
- NOTE: the matching one-entry §Distribution & versioning DECIDED line ("first-party
  packages resolve from a CI-published static tree behind a baked-in default URL,
  env-overridable, host-migratable") is recorded at the next `/align` touching that
  section — this change file is its interim authority.

## Out of scope (adjacent, already tracked or later bundles)

- #1600 (`run -f entry.rs`), #1601 (Py/TS handle + run), #1602 (`dev`), #1603/#1604
  (plugin scaffolds) — existing M39 tickets this change composes with; only the scope
  deltas above touch them.
- PyPI single-binary packaging, api-server → `runtime/` move, `streamlib-runtime`
  binary retirement — the engine-topology change (bundle 3).
- `@session` semantics, schema-agreement work — `schema-agreement-ripout` (in flight).
- Scaffold-time eager builds' orchestrator side effects (`PolyglotBuildOrchestrator::
  default()` probing) — verified at implementation; `new` may build eagerly so first
  `dev` is fast, matching create-next-app's install-at-scaffold behavior.
