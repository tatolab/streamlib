# Package development & distribution — the two loops

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).

## What this is

streamlib is one substrate — an engine that resolves *where* a package's
source lives, materializes it, and loads the staged result (see
[`runtime-module-materialization.md`](runtime-module-materialization.md)).
Two loops run over that substrate, and the seam between them is a single
operation: **install**.

- **The dev loop** — `streamlib link` points a consumer's *entire*
  streamlib surface at one local checkout, so edits resolve to working-tree
  source with no publish. Instant edit→run; deliberately all-local.
- **The distribution loop** — `streamlib pkg publish` releases a package
  by version into a registry; a consumer resolves it back by version.
  Releases are atomic and a consumer can detect a partial one.

The two loops never blur: a link override is refused entry into any
published artifact, and a locked run does zero live re-resolution. The
version model in the middle makes both consistent — one version axis per
package, one version per package in a running process.

```
   dev loop                         install seam                 distribution loop
   ────────                         ────────────                 ─────────────────
   streamlib link <checkout>                                     streamlib pkg publish
     whole-tree toolchain            streamlib install            compute_release_closure
     overrides (cargo/uv/deno)   ─▶    resolve range→concrete  ◀─   → atomic staged release
     → working-tree source            materialize + lock            (manifest written last)
                                       → streamlib-app.lock
                                            │
                                            ▼
                                     Runner::add_modules_from_lockfile
                                       locked run — offline, pinned,
                                       content-hash verified
```

## The dev loop — whole-tree link

`streamlib link <CHECKOUT>` points a consumer at a local streamlib checkout;
`streamlib unlink` restores every touched file byte-identically; bare
`streamlib link` prints status. Implementation:
[`libs/streamlib-cli/src/commands/link.rs`](../../libs/streamlib-cli/src/commands/link.rs)
plus the shared marker in
[`libs/streamlib-pack/src/link_marker.rs`](../../libs/streamlib-pack/src/link_marker.rs).

**Whole-tree, not per-dep.** Link mode is a *toolchain-emission* concern —
it does not add a resolution path (the engine already resolves a local
package via `Strategy::Path`). `plan_edits` computes three language-native
overrides in one plan, so a checkout's SDK libraries *and* sibling packages
all resolve to that one tree:

| Toolchain | Override | Written to |
|---|---|---|
| cargo | `[patch."<index>"]` with one `path = "<member>"` per crate | `.cargo/config.toml` |
| Python (uv) | `[tool.uv.sources] streamlib = { path = "libs/streamlib-python", editable = true }` | `pyproject.toml` (only if present) |
| Deno | import-map `imports.streamlib` → `libs/streamlib-deno/mod.ts` | `deno.json(c)` (only if present) |

The cargo registry index is discovered live (`discover_registry_index`
reads `registries.gitea.index` from the consumer's cargo config), never
hardcoded; each emitted table carries a greppable
`# streamlib-link — managed by streamlib link` marker. The crate set is
derived from the checkout via `compute_release_closure` — the **same**
definition a release uses — so a whole-tree link and a release always agree
on which crates exist. Whole-tree consistency is by construction: a single
checkout never mixes published versions.

**Manifest-first transaction.** `establish_link` plans every edit in memory,
persists the full plan to `.streamlib/link.json` with state `Applying` via
`O_EXCL` (`create_new`) *before* touching a file — a concurrent link fails
`LinkMarkerAlreadyExists` — then backs each pre-existing file up under
`.streamlib/link-backup/` and writes. Any apply error triggers a
*verified* rollback (each restore is re-read and hash-checked); only a
provably-restored tree removes the state dir, otherwise `RollbackIncomplete`
leaves the backups for `unlink` recovery. The marker records per-file
`pre_edit_sha256` / `post_edit_sha256` / `existed_before` for byte-exact
teardown. A crash mid-apply leaves state `Applying`; a later `link` refuses
with `TornLinkState` pointing at `unlink`.

**Post-link verification.** Unless `--skip-verify`, `link` runs
`cargo metadata --offline` and asserts every `streamlib*` / `vulkan-jpeg`
package resolves to a path source under the checkout; if a
semver-incompatible consumer requirement made cargo silently ignore the
`[patch]`, verification names the offending crates, the whole link rolls
back, and `LinkVerificationFailed` tells the user to fix the requirement or
re-run with `--skip-verify`.

**Tri-state.** Link state is `LinkTransactionState::{Applying, Active}` plus
marker-absence (three states, surfaced by `status`). Per-file, `unlink`
classifies each touched file into `RestoreAction::{Skip, RestoreOriginal,
RemoveCreated}`; a file the user modified while linked refuses with
`UnlinkRefusedModifiedFile` unless `--force`.

**Overrides never leak into an artifact.** All three edits land in
*toolchain config* files — never a lockfile. `streamlib pkg build` /
`publish` refuse while any link marker exists up-tree
(`ensure_no_active_link_for_pack` → `PackRefusedWhileLinked`), and the build
orchestrator re-injects the same uv-source override from the marker when it
provisions a linked package's venv (`apply_link_override_if_active`), so a
cargo-patched-but-venv-from-registry mixed state can't occur.

**What link deliberately does not solve.** Developing against one *specific
published* version — link is all-or-nothing against a single checkout. That
case is the version model + a `patch:` entry, not link.

## The version model

One version axis per package, one version per package in a process.

**SemVer with a closed prerelease grammar.** The `SemVer` type
([`libs/streamlib-idents/src/semver.rs`](../../libs/streamlib-idents/src/semver.rs))
carries `major.minor.patch` plus an optional `Prerelease { kind, n }` where
`kind` is `Dev` or `Rc` only — no `+build` metadata. Ordering within one
release core is `dev.k < rc.j < release`: a release (no prerelease) outranks
any prerelease of the same core; two prereleases compare by `(kind, n)`.
Range matching (`SemVerRange::{Any, Exact, AtLeast, Caret, Tilde}`) is
npm-style: a release requirement admits only releases, `*` excludes
prereleases, and a prerelease requirement admits a prerelease candidate only
when it shares the release core and is at-or-above the requirement.

**One version, stamped — no redundant crate-version axis.** The canonical
version is `streamlib.yaml`'s `package.version`. At pack time
`assemble_artifact` stamps the artifact's `Cargo.toml` `[package].version`
from it (`stamped_cargo_toml_bytes` → `rewrite_cargo_package_version`,
format-preserving, handles `version.workspace = true`), so a package's crate
version can't drift from its manifest version in a distributed artifact. The
in-tree copy is kept honest by the `cargo xtask check-package-version-drift`
lint; `--fix` rewrites the in-tree `Cargo.toml` through the same routine.
The intended bump workflow is "edit `streamlib.yaml`, run `--fix`" — never
hand-edit `Cargo.toml`.

**Schema idents are release-core.** A `SchemaIdent`
([`libs/streamlib-idents/src/ident.rs`](../../libs/streamlib-idents/src/ident.rs))
carries `@org/package/Type@version` where `version` is *release-core* —
the prerelease channel stripped, `major.minor.patch` preserved
(`SemVer::release_core()`). So `1.2.3-dev.4` and `1.2.3` share one schema
identity. Three enforcement prongs keep the invariant total:

1. **Constructor projects** — `SchemaIdent::new` stores
   `version.release_core()`.
2. **Parser rejects** — the `version` field deserializes through
   `deserialize_release_only_semver`, a hard error on any prerelease on the
   wire.
3. **Two grep-auditable format sites** — `Display for SchemaIdent` and
   `CatalogClient::fetch_schema_type_definition` both emit the `version`
   field verbatim, trusting (1)+(2). They are the only places a schema-ident
   version is formatted, so the invariant is auditable by inspecting two
   call sites.

**Single version per package, enforced at resolution.** `ResolutionMemo`
([`libs/streamlib-engine/src/core/runtime/module_loader/recursive_walker.rs`](../../libs/streamlib-engine/src/core/runtime/module_loader/recursive_walker.rs))
is a runtime-lifetime `Mutex<HashMap<PackageRef, PackageResolutionState>>`
held as `Arc<ResolutionMemo>` on `Runner`. Its `gate` classifies-and-inserts
under one lock: the first resolver to reach a package inserts an
`InFlightPlaceholder`; a second load that observes the placeholder skips and
waits on a `PackageResolutionCompletionSignal` at the *end* of the walk
(600s timeout) rather than blocking inside the gate — so the design is
deadlock-free (no thread blocks holding the packages lock). A concrete
version mismatch raises `SingleVersionConflict { package, existing_version,
existing_required_by, conflicting_version, conflicting_required_by }`.
`InFlightPlaceholderGuard` is RAII: commit on success, remove-and-publish-
`Failed` on drop.

**Flat-global by IPC necessity.** The shared msgpack wire vocabulary means
two packages on different `@tatolab/core` schema versions would emit
incompatible bytes on the same IPC fabric, so npm-style nested duplicates
are physically impossible in one process. The schema registry
([`libs/streamlib-engine/src/core/embedded_schemas/mod.rs`](../../libs/streamlib-engine/src/core/embedded_schemas/mod.rs))
is one flat `LazyLock<RwLock<HashMap<String, Arc<str>>>>` keyed by the
*unversioned* canonical id: `strip_semver_suffix` drops any trailing
`@version` before lookup and registration composes `@org/name/Type` using
`owner.version.release_core()`, so versioned and unversioned references
collapse to one slot (last-write-wins). The static is per-loaded-artifact,
but plugin cdylibs forward through the host's single map via
`install_host_services` function pointers, so at runtime every consumer
reads and writes the host's one vocabulary. Single-version-per-package is
the invariant that keeps that flat map coherent.

**`patch:` is per-manifest-local.** A `Manifest.patch` table
([`libs/streamlib-idents/src/manifest.rs`](../../libs/streamlib-idents/src/manifest.rs))
lives in the consumer's own `streamlib.yaml` — there is *no* workspace
walk-up. Resolution consults the same consumer's patch
(`effective_spec = patch.get(dep_ref).unwrap_or(spec)`) before the installed
cache. A root-level override reaches a *locked run* only because the install
resolve applied it and materialized the result into the lockfile pins — a
locked run bypasses `patch:` entirely and resolves from the lockfile.

## The distribution loop — atomic release

**Closure by definition.** `compute_release_closure(workspace_root)`
([`libs/streamlib-pack/src/lib.rs`](../../libs/streamlib-pack/src/lib.rs))
is the *single* definition of "the set of crates a release publishes":
workspace member ∧ linkable name (`streamlib*` or `vulkan-jpeg`) ∧ has a
library target ∧ publishable, returned in topological publish order. There
is no "SDK-subset vs. all-libs" switch — `streamlib-plugin-sdk` /
`vulkan-jpeg` are members by definition, so a release can't silently omit a
crate a human forgot to flag.

**Manifest-last atomicity.** A `ReleaseManifest`
([`libs/streamlib-idents/src/release.rs`](../../libs/streamlib-idents/src/release.rs))
lands at `streamlib-release/<V>/manifest.json` and is written **last**, so
its presence is the release's completion marker (`upload_release_manifest`
is the atomicity flip the publisher runs after every crate / SDK / package
has landed).

**Consumers detect a partial release.** `assert_release_complete`
([`libs/streamlib-build-orchestrator/src/release_check.rs`](../../libs/streamlib-build-orchestrator/src/release_check.rs))
picks the newest release manifest satisfying each pin's range and runs
`crates_missing_from_release`; any gap raises `IncompleteRelease { package,
release_version, missing, hint }` — an actionable, up-front error instead of
a cryptic "failed to select a version for `streamlib-plugin-sdk`" deep in
cargo / `streamlib-macros` version unification (the symptom this replaces).
The check rides *inside* materialize, so install fails before cargo runs.

**Two backends, one read shape.** Distribution has a hosted-registry backend
and a plain static-file-tree backend behind one tokenless read shape (sparse
index + tarballs + `.slpkg` generic store + catalog). The static tree —
what CI and local `file://` resolution use — is documented in
[`static-registry.md`](static-registry.md); its catalog surface (a queryable
processor / port / schema index a visual editor browses without downloading
packages) is documented there too. The hosted-Gitea backend is documented in
[`gitea-registry-distribution.md`](gitea-registry-distribution.md); the
by-version resolution model (`{ path, version, registry }`, schema-package
resolution, the anonymous version index) is shared by both.

One gap the static path closes: on a hosted-registry read path, a partial
crate set uploaded at a version *above* the newest manifest'd version — with
no manifest of its own — is invisible to `assert_release_complete` (which
keys on manifest'd versions and proceeds when no manifest covers a pin), yet
cargo's sparse index could still resolve a caret pin up to that higher
partial version. The static tree removes the window entirely: crates and the
release manifest land in one atomic whole-tree `renameat2(RENAME_EXCHANGE)`
staged swap (`publish_staged_tree`), so no state ever exists where partial
crate versions sit above the newest manifest.

## The install seam — install / run split

Install and run are distinct operations with the application lockfile as the
only handoff.

**`install` — resolve + materialize + lock.**
`install(root_dir, orchestrator, sink, options)`
([`libs/streamlib-engine/src/core/runtime/install.rs`](../../libs/streamlib-engine/src/core/runtime/install.rs),
CLI `streamlib install`) runs the shared `resolve_with` (range→concrete),
materializes each resolved package through the orchestrator, and writes the
lockfile. The release-completeness check *rides* materialize — a partial
registry release fails install before any lockfile is written. Network at
install time is expected.

**Locked run — offline, pinned, verified.**
`Runner::add_modules_from_lockfile` builds a `LockedResolution` from the
lockfile and forces every dependency edge through it: each edge becomes a
`Strategy::Path { build: NeverBuild }` at `SemVerRange::Exact(pin.version)`.
Five network / build touchpoints are unreachable in locked mode — **registry
list, registry download, git fetch, `.slpkg` re-fetch, and build** — the run
loads strictly from the pre-materialized cache and is offline by
construction; a dep absent from the lockfile is a hard `LockfileMiss`, never
a live fetch. At load, a **content-hash integrity gate** re-hashes each
slot's manifest + schema set (`content_hash_for_package_dir`, SHA-256) and
refuses on mismatch (`LockedSlotContentMismatch`), closing the
tampered / republished-in-place hole. Slot paths are re-derived by the shared
`get_cached_package_dir_for_name_version` helper
([`libs/streamlib-engine/src/core/streamlib_home.rs`](../../libs/streamlib-engine/src/core/streamlib_home.rs)) —
the single `cache/packages/{name}-{version}` convention also used by `.slpkg`
extraction, registry resolution, and orchestrator staging.

**Two lockfiles, two lifecycles.** Both serialize the same `Lockfile` wire
shape ([`libs/streamlib-idents/src/lockfile.rs`](../../libs/streamlib-idents/src/lockfile.rs))
but are distinct files with distinct headers:

| File | Written by | Pins |
|---|---|---|
| `streamlib.lock` (`LOCKFILE_NAME`) | `streamlib generate` / jtd-codegen | the *schema* set that reconstructs generated bindings byte-for-byte |
| `streamlib-app.lock` (`APP_LOCKFILE_NAME`) | `streamlib install` | the *runtime package* set an installed app loads offline |

**Resolver handoff — deliberately two resolvers.** Range logic lives only at
install (`resolve_with`); concrete enforcement lives only at run
(`LockedResolution`, which does zero range resolution). The two resolvers
stay physically separate; the lockfile is the only artifact between them.
They are not merged on purpose — the picker (install) and the enforcer (run)
have different jobs.

## Known limitations

Stated honestly; verify against current code before relying on any.

- **Polyglot spawn-time offline is unproven.** The locked run's five-
  touchpoint offline guarantee is proven for the Rust module-load path. The
  Python venv provisioning / Deno subprocess spawn behavior under a strictly
  offline locked run is not separately gated, so "an installed polyglot app
  runs fully offline at process spawn" is, to the best of our current
  knowledge, unverified.
- **pypi / npm emit has narrower CI coverage than cargo / slpkg.** The
  daemon-free registry gate resolves `.slpkg` (`file://`) and the cargo fork
  tree end-to-end and exercises the completeness negative gate, but the xtask
  emit smoke runs `--no-pypi --no-npm --no-slpkg`; the pypi-simple and npm
  renderers are exercised by unit tests, not a full `file://` resolve in CI.
- **Deploy lockfiles don't pin checkouts.** Link-mode registry-dep
  redirection lives at the toolchain layer. A lockfile produced from a linked
  or path-declared tree records `path:` sources (a working-tree path), not an
  immutable content-pinned slot — so an app lockfile captured over a link is
  not a reproducible pin of the checkout's contents.

## Reference

- **Link mode**: `libs/streamlib-cli/src/commands/link.rs`,
  `libs/streamlib-pack/src/link_marker.rs`.
- **Version model**: `libs/streamlib-idents/src/semver.rs` (SemVer +
  ranges), `ident.rs` (`SchemaIdent` release-core invariant),
  `manifest.rs` (`patch:` locality),
  `libs/streamlib-engine/src/core/runtime/module_loader/recursive_walker.rs`
  (`ResolutionMemo`),
  `libs/streamlib-engine/src/core/embedded_schemas/mod.rs` (flat-global
  registry).
- **Version stamping**: `libs/streamlib-pack/src/lib.rs`
  (`rewrite_cargo_package_version`), `xtask/src/check_package_version_drift.rs`.
- **Atomic release**: `libs/streamlib-pack/src/lib.rs`
  (`compute_release_closure`), `libs/streamlib-idents/src/release.rs`,
  `libs/streamlib-build-orchestrator/src/release_check.rs`.
- **Install / run**: `libs/streamlib-engine/src/core/runtime/install.rs`,
  `libs/streamlib-engine/src/core/runtime/module_loader/locked.rs`,
  `libs/streamlib-idents/src/lockfile.rs`,
  `libs/streamlib-engine/src/core/streamlib_home.rs`.
- **Related docs**: [`runtime-module-materialization.md`](runtime-module-materialization.md)
  (the one materialize path), [`static-registry.md`](static-registry.md)
  (static backend + catalog),
  [`gitea-registry-distribution.md`](gitea-registry-distribution.md)
  (hosted backend + by-version resolution),
  [`schema-identity-and-packaging.md`](schema-identity-and-packaging.md)
  (schema-ident grammar + packaging),
  [`package-staging-layout.md`](package-staging-layout.md) (what a staged /
  published package contains).
