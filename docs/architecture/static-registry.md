# Static-file `.slpkg` registry

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).

A registry's read side is just static files. StreamLib emits a plain on-disk
tree — a `.slpkg` generic store, a per-package + aggregate catalog, and a
release manifest — that is **tokenless to read** over `file://` and
**browsable as a plain HTTP directory index**. The same tree serves
identically whether it is a CI fixture, a local publish-and-read folder, or a
cloud object store. No registry daemon, no database, no token is required to
*serve* it.

> Sections on the **cargo sparse index + `.crate` tarballs**, the
> **pypi-simple sdist tree**, and the **npm packument + `.tgz`** were removed
> 2026-07-13 — those emitters emulated real cargo / PyPI / npm registries for
> by-version SDK distribution against `registry.tatolab.com`, which was never
> deployed. The custom cargo registry is gone: internal cross-crate deps
> resolve by `path`, and SDK / library crates publish to the real public
> registries as a separate, gated release step. This doc now describes only
> the surviving `.slpkg` generic store + catalog. The fork-mirror bootstrap
> section (`emit-static-fork.sh`, `cargo-fork-mirror`) was already removed
> 2026-07-13 when the vulkanalia fork was vendored at `vendor/tatolab-vulkanalia*`
> (see [`vendored-vulkanalia.md`](vendored-vulkanalia.md)).

## Read transport

The `.slpkg` generic store is served over `file://` — the existing
`streamlib-idents` registry client (`RegistryClient`) reads the generic store
over `file://` natively, and a dumb static HTTP mount
(`python3 -m http.server`, `nginx autoindex`, an object-store/CDN origin)
serves the identical tree over `http(s)://` for a remote consumer. StreamLib
does **not** ship a server binary.

## Tree layout

```
<root>/
  slpkg/
    <pkg>/<version>/<pkg>.slpkg          # generic store (RegistryClient file:// layout)
    <pkg>/<version>/<pkg>.catalog.json   # per-package catalog — keyed by FULL version
    <pkg>/<core>/schemas/<Type>.jtd.json # per-schema JTD — keyed by RELEASE-CORE version
    streamlib-release/<V>/manifest.json  # the release manifest — completion marker
  catalog/
    index.ndjson                         # processor palette — one NDJSON line per processor
```

Every path is relocatable — the store carries no absolute base URL, so the
same bytes serve from any mount point or `file://` root.

## Atomic release — the staged swap

A `file://` consumer must never observe a half-written tree. `emit_static_registry`
builds the whole tree into a **staging sibling** of the served path and writes
the [`ReleaseManifest`](../../sdk/streamlib-idents/src/release.rs) LAST, then
flips staging into the served path in a single operation
(`static_registry::publish_staged_tree`): a plain atomic `rename` when the
served path is absent, and a gapless `renameat2(RENAME_EXCHANGE)` swap when
replacing an existing tree (Linux). During the (long) staging build the served
tree is a separate directory and is never touched, so a concurrent reader always
sees the previous *complete* release; the flip is the only mutation of the
served path. This closes the mid-publish window where a consumer could resolve
a higher partial version before its release manifest landed.

The release manifest lists exactly the `.slpkg` packages the emit published;
its `crates` set is empty (the emit no longer publishes an SDK crate chain).
`compute_release_closure` — the single definition of "which library crates a
release publishes" — survives as the crate set `streamlib link` patches, but is
no longer consulted by the `.slpkg` emit.

## Catalog — queryable processor / port / schema metadata

Alongside the resolvable `.slpkg` artifacts, an emit writes a **catalog**: the
processor / port / schema metadata a visual graph editor (or any tool
building a node palette) browses without downloading every `.slpkg`. The
protocol surface — types plus the read client — lives in `streamlib-idents`
([`catalog.rs`](../../sdk/streamlib-idents/src/catalog.rs)), the crate that
owns the registry protocol; assembly lives in `streamlib-pack`
([`catalog.rs`](../../tools/streamlib-pack/src/catalog.rs),
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

## Non-distributable package skip

The whole-tree `.slpkg` emit **skips** any `packages/*` that is
non-distributable — one carrying a `streamlib.yaml` path-`patch:` block OR a
Cargo.toml dependency-table `path` dep (the test-only fixtures) — with a
`warn!` naming every offender, so it is excluded from the release manifest and
the catalog rather than failing the whole emit. The skip predicate
(`decide_package_emit`) keys on exactly the set `ensure_no_path_artifacts`
rejects for the `Slpkg` target, so the skip set equals the rejection set: a
package the emit would hard-fail on is always skipped instead. TARGET paths
(`[[bin]].path` / `[lib].path`) are not dependency paths and never count. The
single-package `streamlib pkg build` / `pkg publish` still hard-fails on the
same condition so an author sees the error.

## ABI-version bump

`STREAMLIB_ABI_VERSION` (in `streamlib-plugin-abi`) is the C-ABI contract a
`dlopen`-loaded package cdylib and the source-built host must agree on. When
they diverge, the load handshake refuses the cdylib with
`PluginAbiVersionMismatch` — working as designed on a genuine version skew.
The `cargo xtask check-abi-republish` CI gate keeps the two in step at PR time:
a change to `STREAMLIB_ABI_VERSION` without a matching `[workspace.package]`
version change fails the check (a registry-free `git` diff of merge-base vs.
working tree). The pin sweep + SDK republish a bump implies are the
release-time actions the gate points a bumper toward, executed against whatever
registry the SDK ships to.

## Emitting a tree

```
cargo xtask static-registry emit --out <dir> [--dev N]
```

Assembles every distributable `packages/*` into the `.slpkg` generic store,
writes the per-package + aggregate catalog, and writes the release manifest
last, all flipped in atomically via the staged swap.

## Consuming a tree

A consumer points the `.slpkg` generic-store client at the tree root over
`file://` (or a dumb HTTP mount):

```bash
# .slpkg generic store + in-process schema codegen (file://, tree root)
export STREAMLIB_REGISTRY_URL="file://<dir>"
```

The runtime module loader's `Strategy::Registry` resolves a package by
`@org/name` + version range from this store (`RegistryClient::list_versions`
→ `select_version` → `download_slpkg`), and `Strategy::Url` fetches a single
`.slpkg` by URL. Both extract the `.slpkg` into the shared package cache and
build it on the host; neither touches a cargo registry.

## Reference

- **Renderers + atomic swap**: `tools/streamlib-pack/src/static_registry.rs`.
- **Catalog**: `sdk/streamlib-idents/src/catalog.rs` (protocol surface +
  `CatalogClient`), `tools/streamlib-pack/src/catalog.rs` (assembly).
- **Generator CLI**: `cargo xtask static-registry emit`.
- **`.slpkg` `file://` transport**: `sdk/streamlib-idents/src/registry.rs`.
- **CI**: `.github/workflows/check-pack-load.yml` (`cargo test -p
  streamlib-pack` renderers/atomic-swap/completeness + the file-based
  pack → load smoke).
- **The two loops** this registry serves:
  [`package-development-model.md`](package-development-model.md).
