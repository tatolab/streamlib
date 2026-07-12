# Gitea registry — the hosted distribution backend

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).

> **Scope.** This doc describes the **hosted-registry backend** and the
> by-version resolution model both backends share. Distribution has two
> backends behind one tokenless read shape: this hosted Gitea one and a
> plain static file tree ([`static-registry.md`](static-registry.md)) — the
> static tree is what CI and local `file://` resolution use. The overall
> two-loop model (dev loop = `streamlib link`; distribution loop =
> publish/install/run) is
> [`package-development-model.md`](package-development-model.md). The
> `{ path, version, registry }` cargo shape, schema-package resolution, and
> the anonymous version index below apply to both backends.

> ~~Every StreamLib-authored **or customized** artifact is distributed and
> resolved through a *single* self-hosted Gitea instance.~~ — Superseded
> 2026-07-12: distribution now has two backends behind one read shape (this
> hosted Gitea one and the static file tree in
> [`static-registry.md`](static-registry.md)). The by-version resolution
> model below is unchanged and shared by both; only the "single Gitea
> instance is the sole backend" framing is superseded.

Every StreamLib-authored **or customized** artifact is distributed and resolved
**by version** — never by relative `path` or git `[patch]` in anything a
consumer sees. SDK libraries resolve from the backend's cargo / pypi / npm
registries; packages are source-only `.slpkg`s in its generic store.

## The model in one picture

> ~~The self-hosted Gitea lifts to a *cloud Gitea* for the public / hosted
> backend.~~ — Superseded 2026-07-12: the public / fresh-clone / CDN path is
> the static file tree ([`static-registry.md`](static-registry.md)), not a
> hosted Gitea. A self-hosted Gitea remains a valid backend for the
> by-version read/publish model below; a *hosted* one is no longer the plan.

```
                self-hosted Gitea  (org: tatolab)
   ┌───────────────────────────────────────────────────────────┐
   │  cargo registry     pypi registry     npm registry         │  ← SDK libraries
   │  (rust SDK crates)  (python SDK)      (deno SDK)            │    resolved by version
   │                                                            │
   │  generic registry  ── source-only .slpkg (streamlib pkg publish) ─┐ ← packages
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
| **Package** | a streamlib package (polyglot: rust + python + deno), loaded by `runtime.add_module` | **generic** (as `.slpkg`) | **`streamlib pkg publish`, source-only** |

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
  (`UV_INDEX` for the pypi install; `STREAMLIB_REGISTRY_URL` for the codegen's
  generic-registry resolution) — see
  [`../learnings/polyglot-venv-gitea-registry-env.md`](../learnings/polyglot-venv-gitea-registry-env.md)
  for the failure symptoms when they're missing. The generic-registry read path
  (version index + `.slpkg` download) is anonymous, so no
  `STREAMLIB_REGISTRY_TOKEN` is needed to resolve — the token is publish-only.
  Engine and SDK agree on a monotonic, language-agnostic
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

## Packages: source-only `.slpkg`s

`streamlib pkg build` / `streamlib pkg publish` produce **source-only**
`.slpkg`s — no prebuilt per-triple cdylib, because a package is polyglot and
source is the uniform shape. The `Slpkg` assemble target ships source only
(`streamlib-pack`); only the runtime orchestrator's `StagedDir` target
compiles a cdylib, because that materialization *is* the host build. Every
`packages/*` is published as a source `.slpkg` to Gitea's generic registry
under `tatolab` (`scripts/gitea/publish-packages.sh` loops the set, shelling
out to `streamlib pkg publish`). A host `add_module`s one via
`Strategy::Registry`, and its rust/python/deno code builds on the host
resolving the SDK libraries from Gitea by version.

A source-only `.slpkg` requires a Rust toolchain on the consuming host to
build the cdylib — the accepted trade-off for the polyglot/uniform-source
shape; compiler-free deployment is a separate prebuilt-distribution concern,
not the package-registry path.

### Anonymous version index

Reads are tokenless, matching cargo's sparse index. The generic registry has
no native version-listing query — Gitea's `/api/v1/packages` management API
`401`s anonymously — so each publish writes a **cargo-sparse-shaped version
index** as a plain generic file at
`/api/packages/{org}/generic/{name}/index/index.json` (NDJSON, one
`{"name","vers"}` line per version), anonymously downloadable like any
generic file. `streamlib pkg publish` recomputes that index from the authed
management listing unioned with the just-published version — every publish
rewrites the full correct index, so a stale or missing index self-heals on
the next publish. `streamlib_idents`' resolver lists versions by reading the
index (`list_versions_http`); the management API is used only on the publish
path. Generic *download* was already anonymous, so the whole read path (list
+ download) is tokenless and the registry token is publish-only.

## Schema-package resolution

`streamlib.yaml` schema dependencies (e.g. `@tatolab/escalate`) are themselves
packages — they resolve from Gitea's **generic** registry: list the schema
package's versions, select the highest satisfying the declared semver range,
fetch that version's `.slpkg`, extract, resolve. The flat generic registry
has no semver-range query, so range → concrete version happens client-side
(cargo/npm/pip shape) by reading the package's anonymous version index
(`/api/packages/{org}/generic/{name}/index/index.json`, see [Anonymous
version index](#anonymous-version-index)), and the resolved concrete version
is pinned in `streamlib.lock` via `ResolvedSource::Registry`. Two halves,
mirroring cargo:

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

   `strip_path_patches` is distinct from `streamlib-pack`'s `RejectPathPatches`:
   the latter *rejects* a path patch with a hard error (a distributed source
   `.slpkg` must not carry a dev override), while the cargo-publish case needs a
   *strip* — the path patch is a legitimate dev affordance locally but must be
   removed from the published manifest.

This is the schema-tier twin of the cargo-crate resolution. The resolver is
shared by all three runtimes' codegen, so the one fix covers rust/python/deno.

## The dev loop — publish-by-version, or whole-tree link

There are two local dev loops, for two different intents (full model:
[`package-development-model.md`](package-development-model.md)):

- **Develop against a specific *published* version.** A change to an
  internal crate becomes a **published `-dev.N` version** the consumer bumps
  to — never a persistent path dep or `[patch]` in a manifest. In the
  monorepo the dev-only `path` makes local edits instant; publishing is a
  release step. This is the loop for co-developing the engine against a
  separate-repo package: publish a dev version and bump.

- **Develop all-local against one checkout.** `streamlib link <checkout>`
  points a consumer's *entire* streamlib surface at a working tree
  (whole-tree cargo `[patch]` / uv-source / deno-import-map overrides), for
  the instant edit→run WIP loop.

> ~~There is no relative-path escape hatch by design.~~ — Reconciled
> 2026-07-12: `streamlib link` *does* emit a whole-tree path override, but it
> is transactional, greppable, restored byte-identically by `streamlib
> unlink`, and **refused entry into any published `.slpkg`**
> (`PackRefusedWhileLinked`). So the "purity" the original claim protects —
> no persistent relative-path dep leaking into a distributed artifact — still
> holds; link is a dev-only toolchain override, not a manifest path dep.

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
  single lightweight Go binary, no JVM. Used as a local dev registry.
  > ~~Lifts to a cloud Gitea for the hosted/centralized backend
  > unchanged.~~ — Superseded 2026-07-12: the public / centralized path is
  > the static file tree ([`static-registry.md`](static-registry.md)) served
  > from a dumb HTTP mount / object store, not a hosted Gitea.
- cargo token must be stored as `Bearer <token>` in `credentials.toml`
  (`cargo login` stores it bare → 401).
- The **sparse** cargo index needs no setup: the registry is reachable once
  the org exists and the first publish populates the DB-backed index. (The
  one-time web-session init `/user/settings/packages/cargo/initialize` belongs
  to Gitea's older **git-based** cargo index — the sparse protocol Gitea serves
  from the package DB needs no `_cargo-index` repo and no initialize call.)
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

## Reference

- `docs/architecture/package-development-model.md` — the two loops (dev =
  `streamlib link`; distribution = publish/install/run), the version model,
  and the install/run seam this backend feeds.
- `docs/architecture/static-registry.md` — the other backend behind the same
  read shape (static file tree + catalog).
- `docs/architecture/runtime-module-materialization.md` — how `add_module`
  builds a source module, resolving its SDK dependencies from the registry.
- `docs/learnings/polyglot-venv-gitea-registry-env.md` — failure symptoms when
  the registry env channels aren't set for a polyglot build.
