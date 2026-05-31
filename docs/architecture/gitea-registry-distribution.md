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

## Namespace — the `tatolab` org

Everything lives under a Gitea **org** named `tatolab`, matching the
`@tatolab/...` package naming:

- cargo: `/api/packages/tatolab/cargo` (sparse index)
- pypi: `/api/packages/tatolab/pypi`
- npm: scope `@tatolab` → `/api/packages/tatolab/npm`
- generic (`.slpkg`): `/api/packages/tatolab/generic/<name>/<version>/<file>`

The org is owned by a dedicated admin user; all four registries are reachable
as soon as the org exists and Gitea's package feature is on. The cargo
registry uses the **sparse** protocol
(`sparse+.../api/packages/tatolab/cargo/`) — Gitea serves the index from its
package database, so there is **no `_cargo-index` repo to create and no
web-session "initialize" step**: the registry is reachable immediately and
the first `cargo publish` populates the DB-backed index. Standing the
namespace up is scripted and idempotent — see
[Operational notes](#operational-notes).

## Resolution: by version, never paths

No consumer ever sees a relative `path` dep or a git `[patch]`. Each language
uses its native resolution, pointed at Gitea via container-level config:

### Rust

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

### Python / Deno

- Python: a package **declares** `streamlib` like any dependency; the build
  orchestrator provisions a **per-package venv** as the tail of `materialize`
  (once per package, alongside the cdylib) and installs `streamlib` into it
  from Gitea's pypi index by version (container-level `UV_INDEX` / `pip.conf`) —
  no editable/path install, and no PYTHONPATH injection of a workspace copy. The
  published SDK is **source-only** (its `_generated_/` is a build artifact
  excluded from the distribution, like a crate's `target/`); the orchestrator
  regenerates the SDK's wire vocabulary (`streamlib/_generated_`) into the venv
  after install via in-process JTD codegen, with schema deps (`@tatolab/core`,
  `@tatolab/escalate`) resolved from the generic registry. Running an example or
  `streamlib pkg install` therefore needs **both** registry env channels set
  (`UV_INDEX` for the pypi install; `STREAMLIB_REGISTRY_URL` +
  `STREAMLIB_REGISTRY_TOKEN` for the codegen's generic-registry resolution) —
  see [`../learnings/polyglot-venv-gitea-registry-env.md`](../learnings/polyglot-venv-gitea-registry-env.md)
  for the failure symptoms when they're missing. Engine and SDK agree on a monotonic, language-agnostic
  **subprocess protocol version** (`STREAMLIB_SUBPROCESS_PROTOCOL_VERSION` ↔
  `streamlib.PROTOCOL_VERSION`), handshaken at subprocess startup and fail-loud
  on mismatch — the replacement for the old compatibility-by-injection
  guarantee. The engine satisfies a range (`MIN..=CURRENT`), so a newer engine
  keeps accepting SDKs that speak an older-but-supported protocol.
- Deno: a package's `deno.json` **declares** `streamlib`
  (`npm:@tatolab/streamlib-deno@^0.4`) and a sibling `.npmrc` points the
  `@tatolab` scope at Gitea's npm registry (read is anonymous; the localhost
  URL is the dev default, overridden for a hosted backend). A processor then
  imports a stable bare `import "streamlib"` / `import "streamlib/adapters/…"`
  — no relative `../../../libs/...`. The engine launches the SDK's runner the
  same way (`deno run --config <package>/deno.json
  streamlib/subprocess_runner.ts`), so the runner and the processor resolve
  the same registry-pinned SDK; there is no workspace path and no env
  path-injection escape hatch (parity with the Python venv loop — dev
  iteration is publish-a-dev-version + bump the declared `streamlib`). Unlike
  the source-shipping cargo/pypi SDKs, the **npm artifact is built JS + `.d.ts`**
  (produced by `deno pack` — `scripts/gitea/publish-deno-sdk.sh`): Deno cannot
  consume a `.ts` package through the `npm:` protocol (node_modules forbids
  type-stripping and `jsr:` schemes), so the npm idiom of shipping transpiled
  JS applies. The SDK source is therefore kept free of `jsr:` deps (`node:`
  builtins + plain npm deps), and the protocol-locked escalate wire-vocabulary
  is regenerated and baked into the published JS (there is no post-install
  codegen hook for an npm consumer the way Python regenerates into its venv).
  The same protocol-version handshake coordinate
  (`STREAMLIB_SUBPROCESS_PROTOCOL_VERSION` ↔ the SDK's `PROTOCOL_VERSION`)
  applies to the Deno SDK.

### vulkanalia fork

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

## Schema-package resolution (resolver + strip capability + cargo-publish wiring shipped)

`streamlib.yaml` schema dependencies (e.g. `@tatolab/escalate`) are themselves
packages — they resolve from Gitea's **generic** registry: list the schema
package's versions, select the highest satisfying the declared semver range,
fetch that version's `.slpkg`, extract, resolve. The flat generic registry
has no semver-range query, so range → concrete version happens client-side
(cargo/npm/pip shape) via Gitea's package-management API
(`GET /api/v1/packages/{org}?type=generic`), and the resolved concrete
version is pinned in `streamlib.lock` via `ResolvedSource::Registry`. Two
halves, mirroring cargo:

1. **Consume side:** `streamlib-idents`' resolver implements the `Registry`
   arm — list → select-highest-in-range → fetch + `extract_slpkg` → load. The
   registry base URL is carried on `ResolverOptions::registry`; `resolve_with`
   is pure (it never reads the process environment). The codegen boundary —
   build scripts and `streamlib generate` — populates it via
   `ResolverOptions::from_env`, which reads `STREAMLIB_REGISTRY_URL` /
   `GITEA_URL` (plus optional `STREAMLIB_REGISTRY_TOKEN`); `file://` is the
   hermetic local-mirror / test transport. A `Registry` dep with no registry
   configured fails loud with `RegistryNotConfigured`.
2. **Publish side:** a crate's bundled `streamlib.yaml` must be path-free so a
   registry-cached consumer hits the `Registry` arm (not a dangling
   `../../packages/...` path patch). `streamlib_pack::strip_path_patches`
   drops dev path-flavor `patch:` entries, exposed as `xtask
   strip-publish-manifest --dir <crate-dir>`. Because **`cargo publish`
   bundles `streamlib.yaml` verbatim** with no file-rewrite hook,
   `scripts/gitea/publish-crates.sh` runs the strip immediately before each
   affected crate's publish and restores the tree afterward (today only
   `streamlib-engine` →`@tatolab/escalate` carries a `patch:`); the companion
   `scripts/gitea/publish-vulkanalia.sh` does the equivalent on a non-git
   scratch copy of the fork. Verified end to end: an out-of-repo consumer
   deps `streamlib` by version, and `streamlib-engine`'s `build.rs` codegen
   resolves `@tatolab/escalate` from the generic registry (the schema package
   published as a source `.slpkg`) and the engine compiles.

   > ~~`streamlib pack`'s `RejectPathPatches` strips the dev `path:` patch.~~
   > — Corrected 2026-05-30 (#1116): `RejectPathPatches` *rejects* a path
   > patch with a hard error (a distributed source `.slpkg` must not carry a
   > dev override); it never stripped. The cargo-publish case genuinely needs
   > a *strip* — the path patch is a legitimate dev affordance locally but
   > must be removed from the published manifest — so `strip_path_patches` is
   > new code, the publish-side counterpart to `RejectPathPatches`.

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

## Operational notes

Standing up / verifying a registry namespace is scripted and idempotent. The
committed scripts are generic — configure the org / admin / URL via the
environment, so the same tooling provisions the central registry and any
self-hosted instance:

- `scripts/gitea/provision-registry.sh` — ensures the admin owner + `GITEA_ORG`
  exist and that cargo/pypi/npm/generic are reachable. API mode
  (`GITEA_ADMIN_TOKEN=…`) targets any Gitea — local container or a hosted
  backend — unchanged; bootstrap mode creates the admin via the local
  container CLI when no token exists yet.
- `scripts/gitea/smoke-test-registry.sh` — publishes a throwaway crate,
  resolves it by version, removes it, and round-trips the generic registry;
  self-cleaning, safe to re-run against a live registry.

Notes the scripts encode:

- One Gitea container hosts all four registries (cargo/pypi/npm/generic) — a
  single lightweight Go binary, no JVM. Local dev registry today; lifts to a
  cloud Gitea for the hosted/centralized backend unchanged.
- cargo token must be stored as `Bearer <token>` in `credentials.toml`
  (`cargo login` stores it bare → 401).
- The **sparse** cargo index needs no setup: the registry is reachable once
  the org exists and the first publish populates the DB-backed index.

  > ~~cargo index needs a one-time web-session init
  > (`/user/settings/packages/cargo/initialize`).~~ — Superseded 2026-05-29.
  > That step belongs to Gitea's older **git-based** cargo index. The
  > committed shape uses the sparse protocol (`sparse+…`), which Gitea serves
  > from the package DB with no `_cargo-index` repo and no initialize call.
- The generic registry (the `.slpkg` home) requires a **raw** request body on
  upload (`curl --upload-file`); a urlencoded body (`curl --data`) is rejected
  with HTTP 500.
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
| cargo publish → resolve-by-version (real SDK chain + vulkanalia/VMA) | ✅ shipped | #1105 |
| `.slpkg` round-trip through the generic registry | ✅ validated (POC) | #1119 |
| `tatolab` org namespace + POC cleanup | ✅ shipped | #1115 |
| schema-package registry resolution (resolver `Registry` arm) + `strip_path_patches` capability | ✅ shipped | #1116 |
| cargo-publish manifest path-strip *wiring* (dev-publish script + manifest migration) | ✅ shipped | #1105 |
| full-engine codegen consumer build (engine `build.rs` resolves `@tatolab/escalate` from the generic registry) | ✅ shipped | #1105 |
| Python SDK publish (source-only) + declare/install-from-registry, protocol-version handshake, codegen-into-venv | ✅ shipped | #1117 |
| Deno SDK publish (built JS via `deno pack`) + declare/resolve-from-npm, protocol-version handshake | ✅ shipped | #1118 |
| packages as source-only `.slpkg` + `streamlib pack` source-only | ⏳ | #1119 |
| repo migration committed (`{ path, version, registry }`, `.cargo/config`, dev-publish script) | ✅ shipped | #1105 |

## Reference

- Milestone: **Gitea Package Registry**.
- Downstream: #1114 (package/example repo split — packages become version
  consumers of the published `streamlib`); the `.deb`/Docker work syncs
  against Gitea.
- Related: `docs/architecture/runtime-module-materialization.md` (how
  `add_module` builds a source module — its SDK-resolution role becomes
  "resolve from the Gitea registry" under this model).
