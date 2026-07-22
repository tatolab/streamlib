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
published artifact, and a locked run does zero live re-resolution. Both
loops feed the same install seam — installing over a linked / path-declared
tree records `path:` sources in the lockfile, while installing from a
registry records content-pinned slots. The version model in the middle makes
both consistent — one version axis per package, one version per package in a
running process.

```
   dev loop                         install seam                 distribution loop
   ────────                         ────────────                 ─────────────────
   streamlib link --engine <co>                                  streamlib pkg publish
     whole-tree toolchain            streamlib install            assemble .slpkg + catalog
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

`streamlib link --engine <CHECKOUT>` points a consumer's *entire* streamlib SDK
surface at a local streamlib checkout; `streamlib unlink --engine` restores every
touched file byte-identically; bare `streamlib link --engine` prints status.
(The unqualified `streamlib link <path>` is a different verb — the per-app
package symlink into `streamlib_modules/`, covered under the install seam
below.) Implementation:
[`tools/streamlib-cli/src/commands/link.rs`](../../tools/streamlib-cli/src/commands/link.rs)
plus the shared marker in
[`sdk/streamlib-idents/src/link_marker.rs`](../../sdk/streamlib-idents/src/link_marker.rs)
(the marker schema lives in `streamlib-idents`, alongside the manifest /
lockfile types, so the engine module loader can reach it without depending
on `streamlib-pack`).

**Two effects: consumer-manifest overrides + link-aware resolution.**

*Consumer-manifest overrides.* `plan_edits` computes three language-native
overrides in one plan, so a checkout's SDK libraries resolve to that one tree
for the consumer's **own** build:

| Toolchain | Override | Written to |
|---|---|---|
| cargo | `[patch.crates-io]` with one `path = "<member>"` per crate | `.cargo/config.toml` |
| Python (uv) | `[tool.uv.sources] streamlib = { path = "sdk/streamlib-python", editable = true }` | `pyproject.toml` (only if present) |
| Deno | import-map `imports.streamlib` → `sdk/streamlib-deno/mod.ts` | `deno.json(c)` (only if present) |

The SDK crates resolve from crates.io by bare `version` (there is no custom
registry), so the override patches `crates-io`; `[patch.crates-io]` is a source
replacement, so cargo uses the patched `path` and never queries crates.io —
it works offline even though the SDK isn't published to crates.io yet. Each
emitted table carries a greppable
`# streamlib-link — managed by streamlib link` marker. The crate set is
derived from the checkout via `compute_release_closure` — the **same**
definition a release uses — so a whole-tree link and a release always agree
on which crates exist. Whole-tree consistency is by construction: a single
checkout never mixes published versions.

*Link-aware module resolution (npm-link semantics).* With an active engine
link, the module loader resolves any `@org/name` present in the linked
checkout's `packages/` tree **from the checkout, regardless of the caller's
[`Strategy`]** — including an explicit `add_module(ident, registry())`. A linked
name takes precedence: this matters for the power-caller path that still passes
an explicit `Strategy` to `add_module`, because overriding only the *default*
strategy would miss it, so editing a linked package and re-running would not
reflect the edit. (App code today rarely calls `add_module` at all — it
references processors version-free with `processor_type_ref!` and the runtime
lazily discovers the provider from `streamlib_modules/`; see the install seam
below. The explicit-`Strategy` path is a power-caller escape hatch.) A package
**not** in the checkout is untouched — it resolves from its declared strategy —
so registry strategies stay available for everything the checkout doesn't
provide. Discovery is from the process working directory (the run dir, where
the marker sits); a corrupt marker is a loud `AddModuleError::LinkStateCorrupt`,
never a silent skip. A **locked run** (`add_modules_from_lockfile`) ignores
links by contract (reproducible / offline). Implementation:
`ActiveLinkedCheckout` +
`resolve_strategy_to_source` in
[`module_loader/source.rs`](../../runtime/streamlib-engine/src/core/runtime/module_loader/source.rs).

*Link-aware staged builds.* When the orchestrator materializes a package under
an active link, it builds against the checkout so host + plugin come from one
source tree (removing the mixed-build plugin-ABI hazard from the dev loop by
construction): the Rust cdylib build is passed the consumer's `[patch]` cargo
config via `cargo build --config <file>`
(`assemble_artifact_with_cargo_config`), and the Python venv installs the
checkout's SDK via `[tool.uv.sources]` (`apply_link_override`). Discovery is
resolved once per build in `discover_active_build_link`
([`streamlib-build-orchestrator`](../../tools/streamlib-build-orchestrator/src/lib.rs)).

[`Strategy`]: ../../runtime/streamlib-engine/src/core/runtime/module_loader/source.rs

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

**Post-link verification + stale-lock remedy.** Unless `--skip-verify`,
`link` runs `cargo metadata --offline` and asserts every `streamlib*` /
`vulkan-jpeg` package resolves to a path source under the checkout. Two
things can make cargo ignore the freshly-emitted `[patch]`, and they need
opposite remedies:

- *A pre-existing consumer `Cargo.lock`.* Cargo honors an existing lock over
  a newly-added `[patch]`, so the streamlib crates keep resolving to their
  registry pins — a re-lock, not a version-requirement change, is the fix.
  `link` owns that step: it transparently re-locks exactly the crates that
  failed to redirect (`cargo update -p <each>`), records the mutated
  `Cargo.lock` as a link-managed file (`record_relocked_lockfile` — snapshot
  + backup + manifest entry, so `unlink` restores it byte-identically), and
  re-verifies. If the automatic re-lock itself can't run, `link` leaves the
  patch applied (unchanged lock) and returns `StaleConsumerLockRelockFailed`
  naming the lockfile and the exact `cargo update` command to finish it.
- *A semver-incompatible consumer requirement.* When the crates STILL resolve
  from the registry after re-locking (or there is no lock to blame), the
  checkout's versions genuinely don't satisfy the requirements — the whole
  link rolls back and `LinkVerificationFailed` names the offending crates and
  points at the version requirements (or `--skip-verify`).

**Tri-state.** Link state is `LinkTransactionState::{Applying, Active}` plus
marker-absence (three states, surfaced by `status`). Per-file, `unlink`
classifies each touched file into `RestoreAction::{Skip, RestoreOriginal,
RemoveCreated}`; a file the user modified while linked refuses with
`UnlinkRefusedModifiedFile` unless `--force`.

**Overrides never leak into an artifact.** The three toolchain-config edits
land in *config* files — never a lockfile. The only lockfile `link` may write
is the consumer's own `Cargo.lock`, transparently re-locked to make the
`[patch]` take effect (above) and restored byte-identically by `unlink`.
Neither escapes the dev tree: `streamlib pkg build` / `publish` refuse while
any link marker exists up-tree (`ensure_no_active_link_for_pack` →
`PackRefusedWhileLinked`), and the build orchestrator re-injects the checkout
overrides when it materializes a linked package — the consumer's `[patch]`
cargo config into the cdylib build and the uv-source override
(`apply_link_override`) into the venv — so a cargo-patched-but-venv-from-registry
mixed state can't occur.

**What link deliberately does not solve.** Developing against one *specific
published* version — link is all-or-nothing against a single checkout. That
case is the version model + a `patch:` entry, not link.

## The version model

One version axis per package, one version per package in a process.

**SemVer with a closed prerelease grammar.** The `SemVer` type
([`sdk/streamlib-idents/src/semver.rs`](../../sdk/streamlib-idents/src/semver.rs))
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
([`sdk/streamlib-idents/src/ident.rs`](../../sdk/streamlib-idents/src/ident.rs))
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
([`runtime/streamlib-engine/src/core/runtime/module_loader/recursive_walker.rs`](../../runtime/streamlib-engine/src/core/runtime/module_loader/recursive_walker.rs))
is a runtime-lifetime `Mutex<HashMap<PackageRef, PackageResolutionState>>`
held as `Arc<ResolutionMemo>` on `Runner`. Its `gate` classifies-and-inserts
under one lock: the first resolver to reach a package inserts an
`InFlightPlaceholder`; a second load that observes the placeholder skips and
waits on a `PackageResolutionCompletionSignal` at the *end* of the walk
(600s timeout) rather than blocking inside the gate — so the design is
deadlock-free (no thread blocks holding the packages lock). A live
re-encounter that resolves the same package to a *different* concrete
version warns and dedupes to the first-resolved winner (single-version
model — a version mismatch never blocks a load; an incompatibility
surfaces at runtime), never a hard conflict. Range enforcement is hard
only at a *locked* run: an installed slot whose on-disk version drifts
from its lockfile `Exact` pin raises `VersionRangeUnsatisfied` (a
reproducibility/integrity failure), keeping the range→concrete resolver
(install) and the concrete-enforcement resolver (locked run) separate.
`InFlightPlaceholderGuard` is RAII: flipped-to-committed by the load's
whole-load commit on success (registration is transactional — see
[`runtime-module-materialization.md`](runtime-module-materialization.md)),
remove-and-publish-`Failed` on drop. A same-load re-encounter of its own
in-flight placeholder (a diamond) skips with no wait entry;
`Runner::remove_module` clears a removed package's committed entry so a
later `add_module` re-resolves it.

**Flat-global by IPC necessity.** The shared msgpack wire vocabulary means
two packages on different `@tatolab/core` schema versions would emit
incompatible bytes on the same IPC fabric, so npm-style nested duplicates
are physically impossible in one process. The schema registry
([`runtime/streamlib-engine/src/core/embedded_schemas/mod.rs`](../../runtime/streamlib-engine/src/core/embedded_schemas/mod.rs))
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
([`sdk/streamlib-idents/src/manifest.rs`](../../sdk/streamlib-idents/src/manifest.rs))
lives in the consumer's own `streamlib.yaml` — there is *no* workspace
walk-up. Resolution consults the same consumer's patch
(`effective_spec = patch.get(dep_ref).unwrap_or(spec)`) before the installed
cache. A root-level override reaches a *locked run* only because the install
resolve applied it and materialized the result into the lockfile pins — a
locked run bypasses `patch:` entirely and resolves from the lockfile.

## The distribution loop — atomic release

A package release is cut with `streamlib pkg publish` (one package to the
`.slpkg` generic store) or, for the whole `packages/*` surface, `cargo xtask
static-registry emit`; everything below is what those commands do under the
hood. SDK / library-crate publishing to the real public registries is a
separate, gated release step — the custom cargo registry that used to serve
the SDK by version was removed; internal cross-crate deps resolve by `path`.

**Closure by definition.** `compute_release_closure(workspace_root)`
([`tools/streamlib-pack/src/lib.rs`](../../tools/streamlib-pack/src/lib.rs))
is the *single* definition of "the linkable crate set":
workspace member ∧ linkable name (`streamlib*` / `vulkan-jpeg` /
`tatolab-vulkanalia*`) ∧ has a library target ∧ publishable, returned in
topological order. There is no "SDK-subset vs. all-libs" switch —
`streamlib-plugin-sdk` / `vulkan-jpeg` are members by definition. It is the
crate set `streamlib link` overrides (and the set a future SDK release would
publish); the `.slpkg` emit no longer consults it (a `.slpkg` release
publishes packages, not crates).

**Manifest-last atomicity.** A `ReleaseManifest`
([`sdk/streamlib-idents/src/release.rs`](../../sdk/streamlib-idents/src/release.rs))
lands at `streamlib-release/<V>/manifest.json` and is written **last**, so
its presence is the release's completion marker (`upload_release_manifest`
is the atomicity flip the publisher runs after every crate / SDK / package
has landed).

**Consumers detect a partial release.** `assert_release_complete`
([`tools/streamlib-build-orchestrator/src/release_check.rs`](../../tools/streamlib-build-orchestrator/src/release_check.rs))
picks the newest release manifest satisfying each pin's range and runs
`crates_missing_from_release`; any gap raises `IncompleteRelease { package,
release_version, missing, hint }` — an actionable, up-front error instead of
a cryptic "failed to select a version for `streamlib-plugin-sdk`" deep in
cargo / `streamlib-macros` version unification (the symptom this replaces).
The check rides *inside* materialize, so install fails before cargo runs.

**One backend, one read shape.** Package distribution is a plain static file
tree — the `.slpkg` generic store + catalog — behind one tokenless read shape,
served over `file://` or a dumb HTTP mount. It is documented in
[`static-registry.md`](static-registry.md), including its catalog surface (a
queryable processor / port / schema index a visual editor browses without
downloading packages). CI resolves against the static file tree; to reproduce
a CI resolve locally, serve an emitted tree per [`static-registry.md` §
Consuming a tree](static-registry.md#consuming-a-tree).

The atomic staged swap closes the mid-publish window: every `.slpkg` and the
release manifest land in one whole-tree `renameat2(RENAME_EXCHANGE)` staged
swap (`publish_staged_tree`), so no state ever exists where a partial package
set sits above the newest manifest.

## The install seam

Three distinct operations reproduce or resolve a package set, keyed by which
lockfile they touch.

**`streamlib install` — reproduce `streamlib_modules/` from `streamlib.lock`.**
`AppModulesDir::install_from_lockfile()`
([`sdk/streamlib-idents/src/app_modules.rs`](../../sdk/streamlib-idents/src/app_modules.rs),
re-exported through `streamlib::sdk::runtime`, CLI `streamlib install`) reads
the committed `streamlib.lock` (`MODULES_LOCKFILE_NAME`) and repopulates the
app's own `streamlib_modules/@org/name/` folder **exactly** — no resolution
decisions. This is the container/CI preinstall seam: `add`/`link` decide what's
in the environment; `install` reproduces that decision elsewhere (a fresh
checkout, an image build). Each byte-source entry (path / archive / url) is
re-materialized through the same stage → validate → promote machinery `add`
uses and re-verified against its recorded `content_hash` — *before* promote, so
a mismatch leaves no partial slot (`InstallContentHashMismatch`); an
`archive`/`url` entry additionally re-checks the recorded `archive_sha256`
(`InstallArchiveHashMismatch`). A `link` entry's symlink is re-created iff its
checkout target still exists; a gone target is a typed
`InstallDanglingLinkTarget` — a dev link is inherently non-reproducible on
another machine. It is per-package-atomic and fail-fast: a failure names the
package (`InstallSourceUnavailable` for a gone path/archive/offline url), leaves
already-reproduced packages in place, and never rewrites the lockfile; a re-run
is idempotent. `path`/`link` sources reproduce only where their recorded local
paths exist, so a portable install relies on `url`/`archive` entries (or a
vendored folder copied into the image directly).

> ~~`install` — resolve + materialize + lock. `install(...)` (CLI `streamlib
> install`) runs the shared `resolve_with` (range→concrete), materializes each
> resolved package through the orchestrator, and writes the lockfile.~~ —
> Superseded 2026-07-13: the CLI `streamlib install` verb now reproduces
> `streamlib_modules/` from `streamlib.lock` (above). The
> resolve + materialize → `streamlib-app.lock` flow survives as the
> **programmatic** `install()` seam (next), no longer surfaced as a CLI verb.

**`install()` — resolve + materialize + lock (programmatic seam).**
`install(root_dir, orchestrator, sink, options)`
([`runtime/streamlib-engine/src/core/runtime/install.rs`](../../runtime/streamlib-engine/src/core/runtime/install.rs),
`streamlib::sdk::runtime::install`) runs the shared `resolve_with`
(range→concrete) over a project's `streamlib.yaml`, materializes each resolved
package through the orchestrator, and writes `streamlib-app.lock`
(`APP_LOCKFILE_NAME`). The release-completeness check *rides* materialize — a
partial registry release fails install before any lockfile is written. Network
at install time is expected. A locked run (`add_modules_from_lockfile`, below)
then loads the pinned set offline. This is the whole-`streamlib.yaml`-tree seam,
distinct from the per-app `streamlib_modules/` reproduction above and consuming
a distinct lockfile.

**`add` / `remove` / `link` — per-app package adoption (the node_modules model).**
`AppModulesDir::add_package(source, options)`
([`sdk/streamlib-idents/src/app_modules.rs`](../../sdk/streamlib-idents/src/app_modules.rs),
re-exported through `streamlib::sdk::runtime`, CLI `streamlib add`) brings
ONE valid streamlib package **byte source** — a folder, an archive (`.slpkg`
/ `.zip` / `.tar.gz`, container detected from magic bytes), or a `file://` /
HTTP(S) URL — into the app's own `streamlib_modules/@org/name/` folder and
records identity, source, and content hash in the app's committed
`streamlib.lock` (`MODULES_LOCKFILE_NAME`). The primitive is "here are the
bytes", never "resolve this against a registry": a registry-coordinate spec
(`@org/name`) is refused with a typed guidance error, and the package's
`@org/name@version` identity always comes from its own manifest. Add never
builds. The flow is stage (`.staging-*` sibling inside the modules folder —
readers ignore that prefix) → validate (manifest identity + content hash) →
promote (atomic same-filesystem swap; previous contents restorable until the
new ones are in place) → lock (atomic temp+rename write), so a failed add
leaves no partial state; a re-add replaces cleanly; an
`expected_archive_sha256` pin mismatch is a typed `HashMismatch`. Anchoring
is the exact app root — the current working directory or an explicit
`at(root)` / CLI `--dir` — with no walk-up and no `STREAMLIB_HOME`
involvement, so each app directory is atomic and self-contained.
`remove_package(pkg_ref)` (CLI `streamlib remove`) reverses it: delete the
package folder, then drop its lockfile entry (folder first, so a crash
leaves the healed direction — an entry pointing at a gone folder).
`link_package(checkout)` (CLI `streamlib link`) is `add` with a symlink
instead of a copy — recorded as a `LockfileSource::Link` — so checkout edits
are live on the next run; `streamlib install` re-creates that symlink when
reproducing the lock.

The CLI `streamlib add` verb is context-sensitive on its anchor directory. The
byte-source adoption above is the **consumer / app** flow (no `package:` block
in the anchor's `streamlib.yaml`). In a **package-authoring** dir (a
`streamlib.yaml` *with* a `package:` block), `streamlib add @org/name@<version>`
instead records a caret dependency (`^<version>`) into that package's own
`dependencies:` table — the schema-tier `cargo add` — preserving every other
manifest field and the leading `# yaml-language-server` comment header. The
`AppModulesDir` primitive itself is unchanged: it is the consumer flow and still
refuses a registry-coordinate byte source.

At load time, `Strategy::InstalledCache` (the bare `Runner::add_module`
default) resolves `<cwd>/streamlib_modules/@org/name` — the co-located slot IS
the installed package, so there is no separate installed-package store behind
it. An active engine `streamlib link --engine` still outranks it (precedence:
link > app modules slot). The same
`streamlib_modules/` probe backs **lazy discovery**: app code that references a
processor version-free (`processor_type_ref!("org","pkg","Type")`) and never
calls `add_module` triggers a discovery of the providing package in
`streamlib_modules/` on the first reference, which loads it on demand — the
no-load-call shape every example uses. Locked runs are unaffected — they
resolve pinned `Strategy::Path` edges and never consult the modules folder. The programmatic `install()` remains the
whole-`streamlib.yaml`-tree front door; `add` / `link` / `streamlib install`
are the per-app `streamlib_modules/` operations.

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
`installed_package_slot_dir` helper
([`runtime/streamlib-engine/src/core/streamlib_home.rs`](../../runtime/streamlib-engine/src/core/streamlib_home.rs)) —
the single co-located, version-free `<app-root>/streamlib_modules/@org/name`
convention also used by `.slpkg` extraction, registry resolution, and
orchestrator staging.

**Three lockfiles, three lifecycles.** All serialize the same `Lockfile` wire
shape ([`sdk/streamlib-idents/src/lockfile.rs`](../../sdk/streamlib-idents/src/lockfile.rs))
but are distinct files with distinct headers:

| File | Written by | Pins |
|---|---|---|
| `streamlib-codegen.lock` (`CODEGEN_LOCKFILE_NAME`) | `streamlib generate` / jtd-codegen | the *schema* set that reconstructs generated bindings byte-for-byte |
| `streamlib-app.lock` (`APP_LOCKFILE_NAME`) | the programmatic `install()` (`sdk::runtime::install`) | the *runtime package* set an installed app loads offline |
| `streamlib.lock` (`MODULES_LOCKFILE_NAME`) | `streamlib add` / `streamlib remove` / `streamlib link` (read back by `streamlib install`) | the packages materialized into the app's `streamlib_modules/` folder (identity, source, content hash) |

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
- **Deploy lockfiles don't pin checkouts.** Link-mode registry-dep
  redirection lives at the toolchain layer. A lockfile produced from a linked
  or path-declared tree records `path:` sources (a working-tree path), not an
  immutable content-pinned slot — so an app lockfile captured over a link is
  not a reproducible pin of the checkout's contents.

## Reference

- **Link mode**: `tools/streamlib-cli/src/commands/link.rs`,
  `sdk/streamlib-idents/src/link_marker.rs` (marker schema + discovery),
  `runtime/streamlib-engine/src/core/runtime/module_loader/source.rs`
  (`ActiveLinkedCheckout`, link-aware resolution),
  `tools/streamlib-build-orchestrator/src/lib.rs`
  (`discover_active_build_link`, staged-build overrides).
- **Version model**: `sdk/streamlib-idents/src/semver.rs` (SemVer +
  ranges), `ident.rs` (`SchemaIdent` release-core invariant),
  `manifest.rs` (`patch:` locality),
  `runtime/streamlib-engine/src/core/runtime/module_loader/recursive_walker.rs`
  (`ResolutionMemo`),
  `runtime/streamlib-engine/src/core/embedded_schemas/mod.rs` (flat-global
  registry).
- **Version stamping**: `tools/streamlib-pack/src/lib.rs`
  (`rewrite_cargo_package_version`), `xtask/src/check_package_version_drift.rs`.
- **Atomic release**: `tools/streamlib-pack/src/lib.rs`
  (`compute_release_closure`), `sdk/streamlib-idents/src/release.rs`,
  `tools/streamlib-build-orchestrator/src/release_check.rs`.
- **Install / run**: `runtime/streamlib-engine/src/core/runtime/install.rs`,
  `runtime/streamlib-engine/src/core/runtime/module_loader/locked.rs`,
  `sdk/streamlib-idents/src/lockfile.rs`,
  `runtime/streamlib-engine/src/core/streamlib_home.rs`.
- **Add / remove (per-app modules folder)**:
  `sdk/streamlib-idents/src/app_modules.rs` (`AppModulesDir` +
  `AddPackageSource` + error taxonomy),
  `sdk/streamlib-idents/src/archive.rs` (the one canonical zip / tar.gz
  extractor pair), `sdk/streamlib-idents/src/lockfile.rs`
  (`MODULES_LOCKFILE_NAME`, `LockfileSource::{Url,Archive}`,
  `write_modules_lockfile`),
  `runtime/streamlib-engine/src/core/runtime/module_loader/source.rs`
  (the `Strategy::InstalledCache` app-modules probe),
  `tools/streamlib-cli/src/commands/add.rs` (CLI wrapper + manifest-derived
  processor summary).
- **Related docs**: [`runtime-module-materialization.md`](runtime-module-materialization.md)
  (the one materialize path), [`static-registry.md`](static-registry.md)
  (the static registry backend, by-version resolution + catalog),
  [`schema-identity-and-packaging.md`](schema-identity-and-packaging.md)
  (schema-ident grammar + packaging),
  [`package-staging-layout.md`](package-staging-layout.md) (what a staged /
  published package contains).
