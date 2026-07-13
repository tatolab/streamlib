# Static-file registry

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).

A registry's read side is just static files. StreamLib emits a plain on-disk
tree — a cargo sparse index + `.crate` tarballs, a PEP-503 pypi-simple tree, an
npm packument + `.tgz`, and the `.slpkg` generic store — that is **tokenless to
read** and **browsable as a plain HTTP directory index**. The same tree serves
identically whether it is a CI fixture, a local publish-and-read folder, or a
cloud object store. No registry daemon, no database, no token is required to
*serve* it.

## Per-ecosystem read transport

Each ecosystem is served over its native anonymous read transport. There is no
single transport because the ecosystems' clients differ:

| Ecosystem | Transport | Why |
|---|---|---|
| `.slpkg` generic | `file://` | The `streamlib-idents` registry client already reads the generic store over `file://` (`RegistryClient`); reused, not rebuilt. |
| pypi-simple | `file://` | uv/pip consume a PEP-503 `simple/` tree over `file://` natively (`UV_INDEX=file://…/pypi/simple`). |
| cargo sparse | dumb static HTTP mount (`sparse+http://…`) | The cargo sparse protocol is **HTTP-only by spec** — there is no `sparse+file://`. A static file server is not a registry daemon. |
| npm | same static HTTP mount | npm reads a packument (`GET /<name>`) + `.tgz`; `dist.tarball` points at the mount. |

The static HTTP mount is any dumb directory server — `python3 -m http.server`,
`nginx autoindex`, `caddy file_server`, an object-store/CDN origin. StreamLib
does **not** ship a server binary.

## Tree layout

```
<root>/
  cargo/
    config.json                         # {"dl":"<base>/cargo/crates/{crate}/{crate}-{version}.crate","api":"<base>/cargo"}
    <aa>/<bb>/<crate>                    # sparse index NDJSON (one line per version)
    crates/<crate>/<crate>-<version>.crate
  pypi/
    simple/index.html                   # PEP-503 root
    simple/streamlib/index.html         # per-project links (#sha256=…)
    packages/streamlib-<version>.tar.gz # sdist
  npm/
    @tatolab/streamlib-deno             # packument — a FILE at the package
                                        # URL path (npm GETs /npm/@scope%2fname;
                                        # a static server decodes to this path)
    tarballs/streamlib-deno-<version>.tgz  # dist.tarball points here
  slpkg/
    <pkg>/<version>/<pkg>.slpkg          # generic store (RegistryClient file:// layout)
    <pkg>/<version>/<pkg>.catalog.json   # per-package catalog — keyed by FULL version
    <pkg>/<core>/schemas/<Type>.jtd.json # per-schema JTD — keyed by RELEASE-CORE version
    streamlib-release/<V>/manifest.json  # the release manifest — completion marker
  catalog/
    index.ndjson                         # processor palette — one NDJSON line per processor
```

The cargo `config.json` `dl`/`api` and the npm `dist.tarball` are absolute URLs
(sparse + npm are HTTP), so they carry the **base URL** the tree is served at;
the `.crate`/`.tgz`/sdist/`.slpkg` bytes and the index files themselves are
relocatable.

## The workspace release closure

> Section on the fork-mirror bootstrap ("The vulkanalia fork is
> mandatory") removed 2026-07-13 — the vulkanalia fork is vendored at
> `libs/tatolab-vulkanalia*` (see
> [`vendored-vulkanalia.md`](vendored-vulkanalia.md)) and resolves by
> path, so the workspace (xtask included) builds with zero registry
> configuration. The standalone fork emitter
> (`emit-static-fork.sh`), its byte-stable crate normalizer
> (`normalize_fork_crate.py`), and the `cargo-fork-mirror` CI
> composite action were deleted with it; the committed `Cargo.lock`
> carries no tatolab-registry source lines. The vendored crates
> publish through the closure emit below like any other member. The
> normalizer's hand-framed-gzip byte-stability technique survives in
> `crate_tarball::finalize_crate_tarball` (see [Byte-stable crate
> emission](#byte-stable-crate-emission)).

The **workspace release closure** rides the tree via
`cargo xtask static-registry emit --cargo-closure`: each closure crate is
`cargo package`d in topo order against an ephemeral static server on the
staging tree itself (each crate resolves its already-emitted siblings —
the vendored `tatolab-vulkanalia*` crates are ordinary closure members —
from the growing staging index; `cargo package` validates registry deps
at package time). Versions always follow the crates' actual manifests — a
`--dev` emit expects the workspace manifests already bumped (the publish
scripts' bump/restore convention). Same-registry index deps omit the
`registry` key — detected data-driven from the packaged manifest's
`registry-index` key; crates.io deps name
`https://github.com/rust-lang/crates.io-index`.

Per-crate `.crate` tarballs are **always repackaged, never reused from a
content cache** (`streamlib_pack::crate_tarball::obtain_crate_tarball`).
`cargo package` is the single source of truth for crate bytes:
`target/package/<crate>-<version>.crate` is cargo scratch, not a
streamlib-managed cache, so any pre-existing artifact is dropped up front and
`cargo package` re-runs for every closure crate. This guarantees the emitted
`.crate` reflects current source at that version — a structurally-valid but
content-stale leftover (e.g. an old-ABI `streamlib-plugin-abi` tarball cached
under a version whose source has since moved to a new ABI) can never be handed
back verbatim. The freshly packaged tarball is then structurally verified
(`verify_crate_tarball`: gzip stream fully decodes, every tar entry enumerates
to EOF, the `<crate>-<version>/Cargo.toml` entry is present); a still-invalid
result is a hard error. Each fresh `cargo package` validates its own live
`registry = "tatolab"` dep set, and the emitted sparse-index line is rendered
from that tarball's bundled manifest, so the tree stays internally consistent.
(Source-tree-hash-keyed reuse was rejected: 35 member manifests inherit
`workspace = true` and the committed workspace `Cargo.lock` is packaged into
each crate, so inputs live outside a crate's own directory and a source-dir
hash would silently miss a central dep / lock bump — reintroducing the exact
stale-emit bug.)

## Atomic release — the staged swap

A `file://` consumer must never observe a half-written tree. `emit_static_registry`
builds the whole tree into a **staging sibling** of the served path and writes
the [`ReleaseManifest`](../../libs/streamlib-idents/src/release.rs) LAST, then
flips staging into the served path in a single operation
(`static_registry::publish_staged_tree`): a plain atomic `rename` when the
served path is absent, and a gapless `renameat2(RENAME_EXCHANGE)` swap when
replacing an existing tree (Linux). During the (long) staging build the served
tree is a separate directory and is never touched, so a concurrent reader always
sees the previous *complete* release; the flip is the only mutation of the
served path. This closes the mid-publish window where a consumer could cargo-
resolve a higher partial version before its release manifest landed.

## Byte-stable crate emission

Each emitted `.crate` is a pure function of source content per
`(crate, version)`: re-emitting an unchanged release yields byte-identical
tarballs and checksums (no sparse-index or consumer-lockfile churn), and a
crate whose source changed under an already-published version is **refused**
rather than silently swapped.

`cargo package` is already byte-deterministic on a fixed toolchain (gzip
MTIME zeroed, fixed tar mtimes / modes, stable entry order, deterministic
DEFLATE) *except* for the `{name}-{version}/.cargo_vcs_info.json` entry,
whose `{"git":{"sha1":...}}` payload tracks git HEAD, not source — so the
raw `.crate` checksum was a function of the commit. No stable cargo flag
suppresses that entry, so `emit_cargo_closure` normalizes each tarball after
packaging (`crate_tarball::finalize_crate_tarball`): strip
`.cargo_vcs_info.json`, re-tar the survivors (cargo's headers cloned
verbatim), and re-gzip with a fixed header. Normalization is idempotent, so
re-emitting a crate at an unchanged version yields byte-identical output even
though each emit repackages from source.

The immutability guard compares a **content fingerprint** — the sha256 of
the canonical, vcs-stripped, *uncompressed* tar, so it's independent of gzip
level too — of the freshly packaged crate against the prior served crate,
which is still present at `opts.out` during the staged build (the flip
happens after the emit closure returns). The served side is fingerprinted
through the same normalization, so a legacy un-normalized served tree does
not false-positive during the transition. A benign commit bump (identical
source, new git HEAD) passes and yields the same checksum; a real source
change under the same version fails the emit with an explicit "bump the
version" error. See
[`../learnings/cargo-crate-vcs-info-nondeterminism.md`](../learnings/cargo-crate-vcs-info-nondeterminism.md).

## Byte-stable pypi + npm emission

The same source-content-only contract holds for the pypi sdist
(`uv build --sdist`) and the npm tarball (`deno pack`) — both `.tar.gz`
archives, both normalized through `tarball::finalize_tar_gz` in
`emit_pypi` / `emit_npm`. The normalization zeroes every tar entry's
mtime, canonicalizes ownership (uid/gid 0, empty owner names), preserves
mode / entry type / path / data / order, and re-gzips with a fixed
header — making the archive a pure function of file content. The same
immutability guard runs: the fresh archive's content fingerprint (sha256
of the canonical, mtime-zeroed, *uncompressed* tar, so gzip-level
independent) is compared against the same-named artifact still present
at `opts.out` during the staged build, and a source change under a
published version is refused with a "bump the version" error. The pypi
simple-index `#sha256=` hash and the npm packument `shasum` / `integrity`
are computed over the normalized bytes so the served artifact matches the
index.

The per-tool non-determinism sources differ and are the vectors the
normalization neutralizes:

- `uv build --sdist` stamps the build wall-clock into every
  build-generated entry's tar mtime (`PKG-INFO`, the `.egg-info/*`
  metadata, directory entries, `setup.cfg`) and into the gzip MTIME
  header; source-file contents, entry order, mode, and ownership are
  already stable across re-emits.
- `deno pack` is already byte-deterministic (fixed epoch-0 tar mtime,
  uid/gid 0, zeroed gzip MTIME), so normalization is a lossless
  idempotent pass over it — the value it adds is running the same
  immutability guard the other three ecosystems get.

## Release / ABI republish

`STREAMLIB_ABI_VERSION` (in `streamlib-plugin-abi`) is the C-ABI contract a
`dlopen`-loaded package cdylib and the source-built host must agree on. A
package resolves the **published** `streamlib` SDK **by version** from this
registry; the host builds the SDK **from source**. The two carry the same
ABI only when the registry serves an SDK compiled at the host's
`STREAMLIB_ABI_VERSION`. When they diverge, the load handshake refuses the
cdylib with `PluginAbiVersionMismatch` — working as designed on a genuine
version skew, not a bug to route around.

So **an ABI-version bump is a coordinated SDK republish**, atomic across three
edits:

1. **Bump `[workspace.package] version`** in the root `Cargo.toml`. The whole
   SDK crate set (`libs/*` + the engine-free `plugin/*` crates) inherits it
   via `version.workspace = true`, so one bump moves every published SDK
   crate. Keep `.release-please-manifest.json` in step with the manual bump so
   release automation doesn't fight it.
2. **Bump every package's `streamlib*` pin** to the new version. Each
   `packages/*` and each internal cross-crate `{ path, version, registry =
   "tatolab" }` dep pins the SDK by version; a caret pin (`"0.5.0"`) does not
   span a minor bump, so a stale pin re-introduces the skew for that package.
   Exact pins (`"=x.y.z"`) are the easy ones to miss.
3. **Re-emit the closure** at the new version (`static-registry emit
   --cargo-closure`), which republishes every SDK crate so a package resolves
   the new-ABI SDK.

The version bump is what lets a consumer pin the new-ABI SDK (the load
handshake refuses an ABI-mismatched cdylib) and what the immutability guard
requires to change published source — a silent same-version source change is
refused. The emit itself carries no stale-cache risk at any version:
`emit_cargo_closure` always repackages each closure crate from current source
(`target/package` is cargo scratch, not a trusted content cache — see [the
closure section](#the-workspace-release-closure)), so even a same-version
re-emit reflects current source. Clearing `target/package` before a CI emit is
therefore redundant belt-and-suspenders, not a correctness requirement.

The `cargo xtask check-abi-republish` CI gate enforces the first, mechanical
half at PR time: a change to `STREAMLIB_ABI_VERSION` without a matching
`[workspace.package]` version change fails the check (a registry-free `git`
diff of merge-base vs. working tree). The pin sweep and closure re-emit are the
release-time actions the gate points a bumper toward.

## Catalog — queryable processor / port / schema metadata

Alongside the resolvable artifacts, an emit writes a **catalog**: the
processor / port / schema metadata a visual graph editor (or any tool
building a node palette) browses without downloading every `.slpkg`. The
protocol surface — types plus the read client — lives in `streamlib-idents`
([`catalog.rs`](../../libs/streamlib-idents/src/catalog.rs)), the crate that
owns the registry protocol; assembly lives in `streamlib-pack`
([`catalog.rs`](../../libs/streamlib-pack/src/catalog.rs),
`build_package_catalog`).

Three on-disk shapes:

| Path | Contents |
|---|---|
| `catalog/index.ndjson` | Aggregate processor palette — one `CatalogIndexLine` per processor across all packages (`CATALOG_INDEX_PATH`). |
| `slpkg/<name>/<version>/<name>.catalog.json` | One package's `PackageCatalog` (its processors, ports, config), keyed by **full** published version. |
| `slpkg/<name>/<core>/schemas/<Type>.jtd.json` | One schema's JTD document, keyed by **release-core** version. |

### Two write paths, one shape

Both the whole-tree emit and a single-package publish write the same three
shapes through the same assembler (`build_package_catalog`) and per-package
writer (`write_package_catalog`), so the per-package `<name>.catalog.json` +
owned JTDs a client fetches are byte-identical regardless of which path wrote
them. The aggregate differs only in breadth: an emit rewrites it as a
single-version snapshot of the source tree, while incremental publishing
accumulates a line per processor per **published** version — consistent with
the versioned `slpkg/` store, which keeps every published version fetchable.

- **Whole-tree `static-registry emit`** builds the catalog for every
  `packages/*` dir and writes the aggregate **whole** (accumulate all lines,
  write `catalog/index.ndjson` once) during the atomic staged flip.
- **`streamlib pkg build` / `pkg publish`** (a single package) writes that
  package's `<name>.catalog.json` + owned JTDs beside the `.slpkg` it just
  uploaded, then **read-merge-writes** the aggregate
  (`merge_catalog_index_lines`): read the existing `catalog/index.ndjson`
  (absent ⇒ empty, self-healing like the per-package version index), drop
  every line owned by the publishing `(package, version)`, append the fresh
  lines, rewrite. Dropping-then-appending makes a republish of the same
  version replace rather than duplicate, and drops the stale line of a
  processor removed on a republish. A publish is not atomic the way an emit
  is — it writes the store, index, and catalog in sequence.

  External schema references resolve against the sibling packages next to the
  one being published (the emit's `packages/` enumeration, applied to the
  package's parent directory); an external ref whose owning dependency isn't
  locally resolvable surfaces a typed `CatalogError` (e.g. `ExternalDepMissing`)
  **before** any bytes land, so a publish either writes a fully-resolved
  catalog or fails loud.

### The version asymmetry

`catalog.json` is keyed by the **full** package version (`2.1.0-dev.3`);
the `schemas/` JTD directory is keyed by the **release-core** version
(`2.1.0` — prerelease stripped, patch preserved). The asymmetry is
deliberate: a schema `SchemaIdent` is release-core by invariant (see
[`package-development-model.md`](package-development-model.md#the-version-model)),
so the reader derives a JTD path from the ident's already-projected version.
A `-dev.N` publisher whose JTDs sat under the full prerelease dir would be
silently unfetchable, because no consumer ever holds a prerelease-versioned
schema ident to look them up by.

### Query surface

`CatalogClient::new(base_url, token)` exposes exactly three fetches (each
tolerates an absent tree as "empty / none", so a pre-catalog registry
degrades cleanly):

- `fetch_processor_index() -> Vec<CatalogIndexLine>` — the whole palette.
- `fetch_package_catalog(package, version) -> Option<PackageCatalog>` — one
  package's processors / ports / config at an exact version.
- `fetch_schema_type_definition(ident) -> Option<Value>` — the JTD for one
  schema, from the owning package's dir.

There is no "list all packages" or "query by processor type" call by design —
palette-by-type is served client-side by scanning `fetch_processor_index()`,
each line of which carries a full `CatalogProcessor`.

### External refs carry the owner's version

When a package's port / config references a schema owned by *another*
package, the recorded `SchemaIdent` carries **that owner's** version, not the
referencing package's. Catalog assembly resolves a manifest's `schemas:`
external entry by recursing into the dependency with the dependency's own
version (a missing dep is `ExternalDepMissing`, a cycle is
`SchemaResolutionCycle`). So a camera package at `2.1.0` referencing
`@tatolab/core`'s `Frame` at `1.4.0` records the ref as core's `1.4.0`.

### Deliberate omissions

`CatalogPort` carries `name`, `description`, `schema`, `read_mode` only. Two
classes of manifest field are intentionally dropped:

- `overflow` / `buffer_size` — present on the authored manifest port, but
  they are per-edge *buffering knobs*, not wiring topology, so they don't
  belong in a palette the editor wires graphs from.
- `required` — has no authored-manifest source at all; it exists only on the
  runtime port descriptors, not on the `streamlib.yaml` port.

The omission is structural (the fields simply aren't on `CatalogPort` and
aren't copied by the assembler), not gated by a runtime check.

## Emitting a tree

```
cargo xtask static-registry emit --out <dir> [--dev N] \
    [--base-url http://127.0.0.1:PORT] \
    [--cargo-closure] [--no-pypi] [--no-npm] [--no-slpkg]
```

- `--cargo-closure` packages every workspace release-closure crate
  into the cargo tree (heavy). The vendored `tatolab-vulkanalia*` crates are
  ordinary closure members; there is no separate fork emit.
- pypi (uv sdist) and npm (deno pack) reuse the SDK build toolchains; `.slpkg`
  packages are assembled via `streamlib pkg build` semantics and the release
  manifest is written last.
- The whole-tree `.slpkg` emit **skips** any `packages/*` that is
  non-distributable — one carrying a `streamlib.yaml` path-`patch:` block OR a
  Cargo.toml dependency-table `path` dep (the test-only fixtures) — with a
  `warn!` naming every offender, so it is excluded from the release manifest
  and the catalog rather than failing the whole emit. The skip predicate
  (`decide_package_emit`) keys on exactly the set `ensure_no_path_artifacts`
  rejects for the `Slpkg` target, so the skip set equals the rejection set: a
  package the emit would hard-fail on is always skipped instead. TARGET paths
  (`[[bin]].path` / `[lib].path`) are not dependency paths and never count. The
  single-package `streamlib pkg build` / `pkg publish` still hard-fails on the
  same condition so an author sees the error.

## Consuming a tree

A consumer configures **one registry location** — the tree ROOT — and the
toolchain derives every ecosystem channel from it. Serve + configure it with:

```
scripts/registry/serve-static-registry.sh <dir> [--port 8799]
```

which starts `python3 -m http.server` on the tree root and prints the channels
a consumer sets. `.slpkg` + in-process schema codegen and pypi read straight
off `file://`; npm needs a static HTTP mount (HTTP-only by spec). cargo over
this served mount uses a `sparse+http` `[source]` replacement — or, for a
serverless resolve, reshape the sparse tree into a `local-registry` and use the
`file://` replacement instead (the cargo `[source]` blocks below show both
shapes):

```bash
# .slpkg generic store + in-process schema codegen (file://, tree root)
export STREAMLIB_REGISTRY_URL="file://<dir>"
# pypi (file://, PEP-503 simple)
export UV_INDEX="file://<dir>/pypi/simple"
# npm (static HTTP mount) — .npmrc:
#   @tatolab:registry=http://127.0.0.1:8799/npm/
```

cargo resolves the `tatolab` registry via a `[source]` replacement, which keeps
the canonical `sparse+https://registry.tatolab.com/cargo/` source id in
`Cargo.lock`. There are two shapes, depending on whether a server is running.

**Serverless (`local-registry`, `file://` — no server).** The cargo *sparse*
protocol is HTTP-only, but a cargo `local-registry` is a distinct source kind
that reads straight off disk. Reshape the sparse `cargo/` subtree into one with
[`scripts/registry/emit-cargo-local-registry.sh`](../../scripts/registry/emit-cargo-local-registry.sh)
— it flattens the `.crate` tarballs to the registry root and copies each sparse
index shard verbatim (a sparse index NDJSON line is byte-identical to a
local-registry index line, both carrying `cksum`), dropping `config.json` — then
point `tatolab` at it:

```toml
[source.tatolab]
registry = "sparse+https://registry.tatolab.com/cargo/"
replace-with = "tatolab-local-registry"
[source.tatolab-local-registry]
local-registry = "<dir>/cargo-local-registry"
```

An out-of-tree consumer's `cargo … --locked --offline` then resolves any
emitted closure crate from this mirror with **no HTTP server** and **no
`CARGO_REGISTRIES_TATOLAB_INDEX` override** — the override would shadow the
replacement and rewrite the source id in the consumer's lockfile, so it must
stay unset. The emitted `.crate`s are byte-stable at the canonical URL (see
[Byte-stable crate emission](#byte-stable-crate-emission)), so a consumer's
locked checksums stay valid across re-emits. CI uses exactly this: the
check-pack-load workflow closure-emits, reshapes, and writes this replacement
to the global cargo config so its out-of-tree staged-package build resolves
`registry = "tatolab"` deps serverlessly. It is also the offline path an
external consumer rides. (The workspace itself never needs the mirror — the
vendored `libs/tatolab-vulkanalia*` crates and every internal cross-crate dep
resolve by path.)

**Served mount (`sparse+http`).** When the tree is already served over a dumb
HTTP mount (`serve-static-registry.sh` runs `python3 -m http.server`), point
`tatolab` at the sparse index over that mount instead — no reshape needed:

```toml
[source.tatolab]
registry = "sparse+https://registry.tatolab.com/cargo/"
replace-with = "tatolab-local"
[source.tatolab-local]
registry = "sparse+http://127.0.0.1:8799/cargo/"
```

A `streamlib registry use <dir>` verb that emits the cargo `[source]`
replacement + the npm `.npmrc` scope and auto-serves npm on localhost — so a
consumer never hand-writes any of the above — is **planned**.

## Reference

- **Renderers + atomic swap**: `libs/streamlib-pack/src/static_registry.rs`.
- **Always-repackage crate emission + byte-stable normalization**:
  `libs/streamlib-pack/src/crate_tarball.rs` (`verify_crate_tarball`,
  `obtain_crate_tarball`, `normalize_crate_tarball`,
  `crate_content_fingerprint`, `finalize_crate_tarball`).
- **Catalog**: `libs/streamlib-idents/src/catalog.rs` (protocol surface +
  `CatalogClient`), `libs/streamlib-pack/src/catalog.rs` (assembly).
- **Index-line renderer + reshape scripts**:
  `scripts/registry/render_cargo_index_line.py`,
  `scripts/registry/emit-cargo-local-registry.sh` (sparse → serverless
  `local-registry` reshape).
- **Generator CLI**: `cargo xtask static-registry emit`.
- **`.slpkg` `file://` transport**: `libs/streamlib-idents/src/registry.rs`.
- **CI**: `.github/workflows/static-registry.yml`,
  `.github/workflows/check-pack-load.yml` (closure emit + serverless
  `[source]`-replacement consumer).
- **The two loops** this registry serves:
  [`package-development-model.md`](package-development-model.md).
