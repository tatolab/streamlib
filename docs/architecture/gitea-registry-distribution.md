# Unified Gitea registry — distribution & dependency resolution

> **Decided architecture, validated by POC, in active migration.** This is the
> committed shape for how every StreamLib-authored and -customized artifact is
> distributed and resolved. It is documented ahead of full implementation **on
> purpose** — so the issues that land it follow one design instead of
> re-inventing resolution. Sections tagged **(finalized in #N)** are being
> implemented by that issue, which completes the corresponding section as it
> merges. Work lives under the **"Gitea Package Registry"** milestone.
>
> This doc is an explicit exception to the "architecture docs describe merged
> code only" discipline — granted by the principal architect so the design is
> known up front. Validated-vs-in-flight status is tracked in the last section.

## The model in one picture

```
                self-hosted Gitea  (org: tatolab)   ──lifts to──▶  cloud Gitea (hosted backend)
   ┌───────────────────────────────────────────────────────────┐
   │  cargo registry     pypi registry     npm registry         │  ← SDK libraries
   │  (rust SDK crates)  (python SDK)      (deno SDK)            │    resolved by version
   │                                                            │
   │  generic registry  ── source-only .slpkg (via streamlib pack) ─┐ ← packages
   └───────────────────────────────────────────────────────────┘  │   (polyglot)
            ▲ publish (release step)            ▲ resolve by version │
            │                                   │                     │
   streamlib monorepo                     consumers: packages, examples,
   (builds itself in-place;              third-party plugins, the installed
    publishes versioned crates)          runtime building a module — all
                                         resolve from Gitea, zero relative paths
```

Truly-external untouched deps (serde, tokio, …) keep resolving from their
normal public registries — only StreamLib-authored **or customized** artifacts
go through Gitea.

## Two kinds of artifact

| Kind | What | Registry | Produced by |
|---|---|---|---|
| **SDK library** | the code a package compiles/runs *against*: rust `streamlib` crate chain, python `streamlib` pkg, deno `@tatolab/streamlib-deno` | cargo / pypi / npm | normal per-language publish |
| **Package** | a streamlib package (polyglot: rust + python + deno), loaded by `runtime.add_module` | **generic** (as `.slpkg`) | **`streamlib pack`, source-only** |

The distinction is load-bearing: SDK libraries are versioned registry
dependencies; packages are **source-only `.slpkg`s** in the generic registry
whose code resolves the SDK libraries by version when built on the host.

## Namespace — the `tatolab` org (finalized in #1115)

Everything lives under a Gitea **org** named `tatolab`, matching the
`@tatolab/...` package naming:

- cargo: `/api/packages/tatolab/cargo` (sparse index)
- pypi: `/api/packages/tatolab/pypi`
- npm: scope `@tatolab` → `/api/packages/tatolab/npm`
- generic (`.slpkg`): `/api/packages/tatolab/generic/<name>/<version>/<file>`

> The validating POC published under a Gitea *user* named `streamlib` by
> mistake (`streamlib/_cargo-index`). `_cargo-index` is Gitea's internal
> per-owner sparse-index repo — normal mechanism; under the org it becomes
> `tatolab/_cargo-index`. #1115 stands up the org, re-publishes under it, and
> removes the POC artifacts.

## Resolution: by version, never paths

No consumer ever sees a relative `path` dep or a git `[patch]`. Each language
uses its native resolution, pointed at Gitea via container-level config:

### Rust (finalized in #1105)

Internal cross-crate deps use the standard cargo "publish a workspace" form:

```toml
streamlib-engine = { path = "../streamlib-engine", version = "0.4.30", registry = "gitea" }
```

- `path` is a **dev-only affordance** — cargo strips it from the published
  manifest (exactly how tokio et al. publish), so consumers see only
  `version` + `registry`. Local monorepo builds use the path (instant edits,
  no republish).
- `registry = "gitea"` is **required** — without it cargo records the dep as
  crates.io and `cargo publish` fails (`location searched: crates.io index`).
- The repo `.cargo/config.toml` declares `[registries.gitea]`; read is
  anonymous (publish needs a token).

### Python (finalized in #1117) / Deno (finalized in #1118)

- Python: `import streamlib` resolves the SDK from Gitea's pypi index via
  container-level `UV_INDEX` / `pip.conf` — no editable/path installs.
- Deno: a stable bare `import "streamlib"` resolves from Gitea's npm registry
  (`.npmrc`) or a container-level import map — no relative `../../../libs/...`.

### vulkanalia fork (finalized in #1105)

The `tatolab/vulkanalia` fork (`vulkanalia` / `-sys` / `-vma`) is published to
Gitea and resolved by `{ version, registry = "gitea" }`. The workspace's
`[patch.crates-io] vulkanalia = { git = … }` override is **removed**. The fork
shares crates.io version numbers, so the `registry` annotation (not a distinct
version) is what selects the fork over upstream.

## Packages: source-only `.slpkg`s (finalized in #1119)

`streamlib pack` produces **source-only** `.slpkg`s — the prebuilt per-triple
cdylib it bundles today is dropped, because a package is polyglot and source
is the uniform shape. Every `packages/*` is published as a source `.slpkg` to
Gitea's generic registry under `tatolab`. A host `add_module`s one, and its
rust/python/deno code builds on the host resolving the SDK libraries from
Gitea by version.

> Trade-off to confirm in #1119: a source-only `.slpkg` requires a toolchain
> on the consuming host to build the rust cdylib — weigh against the
> compiler-free-deployment goal.

## Schema-package resolution (finalized in #1116)

`streamlib.yaml` schema dependencies (e.g. `@tatolab/escalate`) are themselves
packages — they resolve from Gitea's **generic** registry: fetch the schema
package's `.slpkg` by name+version, extract, resolve (the `extract_slpkg`
plumbing already exists; today `streamlib-idents`' resolver returns
`RegistryNotImplemented` for the `Registry` arm). Two halves, mirroring cargo:

1. **Publish side (already exists):** `streamlib pack`'s `RejectPathPatches`
   strips the dev `path:` patch from a distributed `streamlib.yaml`. The gap is
   that **`cargo publish` bundles `streamlib.yaml` verbatim** (patch included),
   so the same strip must run when a crate's manifest is bundled by cargo.
2. **Consume side (new code):** implement the resolver's `Registry` arm to
   fetch+extract the schema package's `.slpkg` from Gitea.

This is the schema-tier twin of the cargo-crate resolution. The resolver is
shared by all three runtimes' codegen, so the one fix covers rust/python/deno.

## The dev loop — one knob, publish-by-version

A change to an internal crate becomes a **published `0.4.x-dev.N` version** the
consumer bumps to — never a new path dep or `[patch]`. In the monorepo, the
dev-only `path` makes local edits instant; publishing is a release step. For
co-developing the engine against a separate-repo package, the same applies:
publish a dev version and bump — there is no relative-path escape hatch by
design (that "purity" is what makes splitting crates into their own repos a
no-op later).

## Operational notes (carry into #1115 runbook)

- One Gitea container hosts all four registries (cargo/pypi/npm/generic) — a
  single lightweight Go binary, no JVM. Local dev registry today; lifts to a
  cloud Gitea for the hosted/centralized backend unchanged.
- cargo gotchas: token stored `Bearer <token>` in `credentials.toml`
  (`cargo login` stores it bare → 401); cargo index needs a one-time
  web-session init (`/user/settings/packages/cargo/initialize`).
- `cargo publish --no-verify` publishes source without compiling — the lever
  that makes the heavy engine chain tractable; consumers verify by building.
- Submodule-vendored crates (`vulkanalia-vma`'s VMA/Vulkan-Headers) must be
  published from a **non-git scratch copy** so cargo bundles the vendored
  sources (cargo excludes submodule contents when packaging inside a git repo).
- The `{ path, version, registry }` migration is a `tomlkit` sweep — rebuild
  inline tables fresh (in-place key-append corrupts comma separators) and
  exclude `dev-dependencies` (cargo strips bare-path dev-deps on publish;
  annotating them creates publish-order cycles, e.g. engine dev-deps streamlib).

## Validated vs in-flight

| Piece | Status | Issue |
|---|---|---|
| cargo publish → resolve-by-version (real SDK chain + vulkanalia/VMA) | ✅ validated (POC) | #1105 |
| `.slpkg` round-trip through the generic registry | ✅ validated (POC) | #1119 |
| `tatolab` org namespace + POC cleanup | ⏳ | #1115 |
| schema-package registry resolution + cargo-publish path-strip | ⏳ | #1116 |
| Python SDK publish | ⏳ | #1117 |
| Deno SDK publish | ⏳ | #1118 |
| packages as source-only `.slpkg` + `streamlib pack` source-only | ⏳ | #1119 |
| repo migration committed (`{ path, version, registry }`, `.cargo/config`, dev-publish script) | ⏳ | #1105 |

## Reference

- Milestone: **Gitea Package Registry**.
- Downstream: #1114 (package/example repo split — packages become version
  consumers of the published `streamlib`); the `.deb`/Docker work syncs
  against Gitea.
- Related: `docs/architecture/runtime-module-materialization.md` (how
  `add_module` builds a source module — its SDK-resolution role becomes
  "resolve from the Gitea registry" under this model).
