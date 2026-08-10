# Change: importable-python-library-ripout

**Change B of the pivot pair — rip out the old world.** Blocked on
`importable-python-library.md` (Change A) shipping: the wheel, built-ins, exchange
surface, and dev experience must exist before their predecessors are deleted. Same ADR.
The REMOVED inventory below was verified by a dedicated audit agent against the tree
(reverse-dependency sweep, CI/xtask enumeration) on 2026-08-02.

## MODIFIED — survivor rewires (sequenced before the deletions they unblock, except
where a bullet says otherwise)

- **api-server relocation first** (already DECIDED, `[control-plane-one-surface]`):
  moves from `packages/api-server` into `runtime/`; its live-mutation verbs
  (`submit`/`replace`/`connect`/`remove`) and their MCP tools are removed — the
  vocabulary is observation-shaped (graph, tap, logs, health, nodes). The CLI `mcp`
  verb and `Dockerfile`/`docker/` re-point at the wheel-hosted runtime (or the Docker
  packaging story is deleted with a note — decide in-ticket from what CI uses).

  > ~~The CLI `mcp` verb … re-point[s] at the wheel-hosted runtime.~~ — Superseded
  > 2026-08-08 by `mcp-served-with-the-node.md` (owner ruling). The verb is deleted, not
  > re-pointed: MCP is served by the node's own control plane at `POST /mcp`, with no CLI
  > verb, stdio transport, or attach bridge. The `Dockerfile`/`docker/` half of this
  > clause stands unchanged.
- **`sdk/vulkan-jpeg`** rewires its pervasive `streamlib_plugin_sdk::sdk::*` imports
  to `streamlib-sdk`; the cdylib-safety constraint drops.
- **`sdk/streamlib-macros`**: the `#[processor]` attribute grammar relocates out of
  doomed `streamlib-processor-extract` into a surviving home (per the superseded
  manifest-extraction ADR: exactly one parser, still true).

  > ~~into a surviving home~~ — Named, and re-sequenced, 2026-08-08 (owner ruling, #1713
  > plan gate). The home is `sdk/streamlib-macros` itself, as a private module, and the
  > move rides the PR that deletes `streamlib-processor-extract`. It has **no earlier
  > home**: `streamlib-macros` is `proc-macro = true`, so it cannot export the grammar as
  > a library module while `streamlib-processor-extract` still parses through it
  > (`src/lib.rs:461`, `:597` — reached from `streamlib-pack`, `streamlib-cli`, and
  > `xtask`). Parking it in a third crate would move it twice, since
  > `processor-class-identity.md` then edits `grammar.rs` in place. One parser at every
  > point in time, no intermediate crate. See the stacked-PR bullet.
- **Adapter capability traits** (`VulkanWritable`, `GlWritable`, `CpuReadable`, …)
  relocate out of doomed `adapters/streamlib-adapter-abi` into the surviving adapter
  cores' shared home; the cores' dev-deps on doomed `-helpers` crates drop with their
  subprocess-parity tests. **Shipped in #1743 / PR #1751**: the home is
  `adapters/streamlib-surface-adapter`. The split is drawn at the cross-DSO boundary,
  not around the named traits — they are defined in terms of the guards and the
  surface descriptor, so the whole non-cross-DSO half travels with them, leaving
  `streamlib-adapter-abi` as exactly the cross-DSO half this change deletes.
- **`sdk/streamlib-python-native` retires into the wheel**: helper processes import
  the wheel itself (one native artifact, per §Distribution); the iceoryx2 transport
  and `escalate`/`subprocess_bridge` re-scope to same-interpreter spawns
  (exec-of-`sys.executable`, never fork); venv-provisioning and
  `native_lib_resolver.rs`'s cdylib resolution die. Verify-before-delete: the
  iceoryx2 log-forwarding pair in `core/plugin/` — keep a successor if helper-process
  logging rides it.
- **Engine host-services strip** — **no longer a survivor rewire; moved to the
  contract-deletion ticket below.**

  > ~~the dual-mode cdylib branch comes out of the ~10 retained files that dispatch
  > through `core/plugin/host_services` (`gpu_context`, `texture`, `bus`,
  > `iceoryx2/input`, `vulkan_present_target`, `host_rhi`,
  > `processor_instance_factory`, `runtime_shutdown_request`)~~ — the eight named are
  > a subset, not the surface; and the strip is re-sequenced. Amended 2026-08-05 from
  > the #1743 implementation audit (PR #1751, `153f84e4`); the re-sequencing is an
  > owner ruling of the same day.
  >
  > **Scale.** The retained surface is materially larger than eight files and spans the
  > whole RHI — the Vulkan kernel and command-recorder files alone are denser than
  > anything on the list. It resists enumeration because the coupling takes two shapes no
  > single pattern catches: a runtime branch on `host_callbacks()`, and a direct
  > `host_services::host_*_vtable()` call that never mentions `host_callbacks()` at all
  > (`iceoryx2/input`, `vulkan_present_target`, `processor_instance_factory` are the second
  > shape only). This change states no file count deliberately — two successive audits
  > produced numbers measuring different predicates. **The inventory belongs to #1715,
  > derived by reading the RHI at implementation time.**
  >
  > **Sequencing.** The strip does not precede the deletions; see the stacked-PR
  > bullet. The clause moving `streamlib-sdk` off its `core::plugin` re-exports and
  > the `auto-build` feature moves there with it, unchanged.
- **`core/streamlib_home.rs`** re-scopes: the `streamlib_modules`/`packages/` walk
  dies; logging paths and the node registry keep a home-dir resolver.

  > ~~the `streamlib_modules`/`packages/` walk dies~~ — Split across two tickets
  > 2026-08-08 (owner ruling, #1713 plan gate); the halves have different blockers.
  >
  > The **`packages/` walk-up** (`find_app_root` / `app_root_from`, reachable only from
  > `get_streamlib_home`) dies on the survivor-rewire ticket, and `get_streamlib_home()`
  > becomes `STREAMLIB_HOME` → the process working directory → `.` — logs land
  > project-local at `./.streamlib/logs`. "Never the user home directory"
  > (`streamlib_home.rs:24`) is **reaffirmed, not reversed**; an XDG data dir was the
  > rejected option. The `current_exe()` fallback goes with the walk rather than surviving
  > it: under the wheel `current_exe()` is `.venv/bin/python`. Five helpers with zero
  > in-tree callers go too — `ensure_streamlib_home`, `get_runtime_dir`,
  > `get_processor_dir`, `get_processor_data_dir`, `get_processor_venv_dir`.
  >
  > The **`streamlib_modules` half** — `app_modules_root`, `installed_package_slot_dir`,
  > `resolved_app_modules_dir`, `set_app_modules_root_override`, `APP_MODULES_DIR_ENV` —
  > is reachable only from `module_loader/` and `core/runtime/install.rs`, so it
  > re-sequences to #1715 with them.
  >
  > The node registry needs nothing here — it already resolves through `XDG_RUNTIME_DIR`
  > (`streamlib-api-server/src/node_registry.rs:122`). Surviving consumers:
  > `core/logging/paths.rs:17`, `streamlib-api-server/src/auth.rs:66`, two shader-dump
  > paths under `vulkan/rhi/`, and `core/runtime/runtime.rs:229`.
- **`sdk/streamlib-idents` / `sdk/streamlib-processor-schema`**: manifest/lockfile
  modules die (`app_modules`, `archive`, `catalog`, `lockfile`, `manifest`,
  `package_source`, `path_artifact_guard`, `release`, `resolver`). The schema-ident core
  and `streamlib-jtd-codegen` survive **this** change only — both are deleted outright by
  `schema-free-ports.md` / `processor-class-identity.md` (amended 2026-08-03; this file
  previously preserved the ident core behind the JTD seam and demoted the codegen crate
  to internal-only, which `[schema-free-ports]` supersedes). Do not invest in reshaping
  either; carry them unchanged and let the successor changes delete them.

  > ~~manifest/lockfile modules die~~ **as a survivor rewire, and the trim splits** —
  > Re-sequenced and corrected 2026-08-08 (owner ruling, #1713 plan gate). Two crates
  > survive #1715 and reach past the ident core into these modules: `streamlib-jtd-codegen`
  > (`resolver`, `package_source`, `lockfile`) and `streamlib-processor-schema` (`manifest`).
  > #1715 carries both unchanged (per the bullet above — it neither deletes nor reshapes
  > either), so those four modules defer with them to the successors: `schema-free-ports`
  > drops jtd-codegen and strips processor-schema, then `processor-class-identity` deletes
  > `streamlib-idents` whole. #1715 trims only the modules whose every consumer it deletes
  > (`app_modules`, `archive`, `catalog`, `path_artifact_guard`, `release` — from
  > `module_loader/`, `core/runtime/install.rs`, `streamlib-pack`, the CLI package verbs,
  > `streamlib-build-orchestrator`).
- **`packages/test-fixtures`** loses its plugin/cdylib arm (the in-process
  attribute-macro tests keep it alive); `test-fixtures-abi-mismatch` dies whole.

  > ~~loses its plugin/cdylib arm~~ **as a survivor rewire** — Re-sequenced 2026-08-08
  > (owner ruling, #1713 plan gate), by this file's own stacked-PR argument. There is no
  > `plugin` feature to drop: `crate-type = ["rlib", "cdylib"]` is unconditional and the
  > generated crate root emits `streamlib_plugin_abi::export_plugin!` unconditionally, and
  > that cdylib is exactly what the `load_project_dylib_*` corpus and `pack_then_load_smoke`
  > dlopen. Those tests die on the contract-deletion ticket "and nowhere earlier, so no
  > earlier stopping point is green" — the arm cannot precede them.
- **CI/xtask**: die — `check-cdylib-reach`, `check-pack-load`,
  `check-package-version-drift`, `check-manifest-schema`,
  `check-no-streamlib-metadata`, `check-schema-versions`,
  `check-processor-source-reachability` (+ `generate_crate_roots`),
  `install-packages`, xtask `StripPublishManifest`/`StaticPackageSource`. Re-scope —
  `check-consumer-rhi-repr` (owner call: rationale was plugin FFI), `check-boundaries`
  (keep RHI wall; drop slpkg/cdylib allowlists), `lint-logging` (drop Deno arm),
  `check-no-inventory-submit`, `check-no-reverse-dns` (doc pointer), `test.yml`,
  `schemas.yml`. Keep — device-wait-idle, no-escalate-in-lifecycle,
  vendored-vulkanalia, license-check, pr-title, release-please.
- The contract-deletion ticket lands as a **pre-approved stacked-PR structure** — its
  blast radius is unreviewable as one diff. **Four more stack levels joined it 2026-08-08**
  (owner ruling, #1713 plan gate), each for the same reason the host-services strip moved:
  the code they depend on is still shipping, so no earlier stopping point is green. They are
  the partial `streamlib-idents` manifest/lockfile trim, `packages/test-fixtures`' plugin/cdylib arm,
  the `streamlib_modules` half of the `core/streamlib_home.rs` re-scope, and the
  `#[processor]` grammar's move into `sdk/streamlib-macros`. Their bullets above state each
  case. **The engine host-services strip is one of
  its stack levels** (moved here 2026-08-05, owner-ruled), alongside the
  `core/plugin/` deletion, carrying `streamlib-sdk` off its `core::plugin` re-exports
  and the `auto-build` feature. The strip has no earlier home because **the cdylib
  test corpus asserts exactly the behaviour it removes**: the 22 `load_project_dylib_*`
  / `pack_then_load_smoke` / `cdylib_owns_tokio_runtime` engine tests, the 4
  `export_plugin_*` plugin-abi tests, and `twin_drift_guard`. Those die on this ticket
  and nowhere earlier, so no earlier stopping point is green. (`check-cdylib-reach`
  asserts the same invariant and retires with the CI/xtask bullet above; its timing is
  that bullet's to state, not this one's.)
  Stripping dispatch ahead of them is not unsafe — it is untestable, and pointless
  while the code it serves still ships.
  - **One part of the surface must outlive the ABI, not merely follow it**: the
    `host_inner()` capability guards. Each panics when reached from cdylib code
    precisely because the alternative is dereferencing host-private layout under the
    cdylib's view of it — UB, by the guards' own stated rationale. They are the last
    thing to go, after the last cdylib.
  - The earlier claim that removing the dead vtable fields would cascade into this
    ticket is **withdrawn** — the fields are inert once unread, and nothing forces
    them out ahead of `core/plugin/`.
  - **Owed to `/reconcile-tracker`**: #1715's body, its stack breakdown, and its file
    count all predate this move and name neither the strip nor the `streamlib-sdk`
    clause — nor, since 2026-08-08, the four levels that joined them. #1713's body owes
    the mirror edit: it still lists all four as its own deliverables, and what remains to
    it is the `sdk/vulkan-jpeg` rewire plus the `packages/` walk-up half of
    `core/streamlib_home.rs`. That residue is small enough that whether #1713 stays
    standalone or folds into the stack is a tracker call, not a plan one.
- **What must survive the strip**: the helper children's privileged-access route — the
  escalate GPU ops over the subprocess IPC path. Its own machinery names `host_services`
  nowhere, but it reaches the GPU through the mode-routed
  `GpuContextLimitedAccess::escalate`; a helper request is serviced in the app process,
  so it always takes the host arm. The strip removes the other arm — this route is not
  merely spared, the branch collapses onto the path it already used. Distinct from the
  cdylib escalate route (`host_services/gpu_context/limited/escalate.rs`), which dies
  with the ABI. #1743 unblocked #1714 on this basis.

## REMOVED

Each bullet is a pattern the ship gate verifies is gone from the tree: **one artifact per
bullet, plain text, on the bullet's first line.** Continuation lines are prose the gate
does not search. Grammar: `changes/README.md`.

> ~~Known gate defect: bullets joining two items or carrying a parenthetical can never
> match and pass vacuously, so a green gate is weak evidence.~~ — Superseded 2026-08-08 by
> PR #1788: the gate now rejects those shapes rather than searching them, and checks path
> existence as well as content. The inventory below is rewritten to the grammar and was
> measured bullet-by-bullet against the pre-#1788 gate; see #1788 for the run.

`streamlib-adapter-cuda-helpers` is already gone, deleted early by #1743 / PR #1751
with its three tests re-homed into `streamlib-adapter-cuda`; its bullet's `-cuda-abi`
half still belongs to this ticket.

- REMOVED: runtime/streamlib-plugin-abi
- REMOVED: sdk/streamlib-plugin-sdk
- REMOVED: export_plugin!
- REMOVED: install_host_services
- REMOVED: HostServices
- REMOVED: ProcessorVTable
- REMOVED: PluginAbiObject
- REMOVED: adapters/streamlib-adapter-abi
- REMOVED: streamlib-adapter-vulkan-abi
- REMOVED: streamlib-adapter-vulkan-helpers
- REMOVED: streamlib-adapter-opengl-abi
- REMOVED: streamlib-adapter-cpu-readback-abi
- REMOVED: streamlib-adapter-cpu-readback-helpers
- REMOVED: streamlib-adapter-cuda-abi
- REMOVED: streamlib-adapter-cuda-helpers
- REMOVED: streamlib-adapter-skia-abi
- REMOVED: runtime/streamlib-engine/src/core/plugin/
- REMOVED: build_fingerprint
- REMOVED: twin_drift_guard

  Vtable backings, the two named trip-wires, and the load handshake. Both symbols get
  their own bullet because both are reached from outside the directory, where the path
  bullet cannot see them: `build_fingerprint` from
  `module_loader/processor_registration.rs:96,:673` and a comment in
  `runtime/streamlib-engine/build.rs:24`; `twin_drift_guard` from comments in
  `core/processors/mod.rs:48` and `xtask/src/check_vendored_vulkanalia.rs`, which cite it
  as a trip-wire style. `build.rs`, `core/processors/`, and the vendored-vulkanalia check
  all survive this change, so those citations are scrubbed with it.
- REMOVED: runtime/streamlib-engine/src/core/runtime/module_loader/
- REMOVED: runtime/streamlib-engine/src/core/runtime/install.rs
> ~~- REMOVED: add_modules_from_lockfile~~ — Deferred 2026-08-10 (owner ruling at the
> #1806 residue scrub). Every remaining reference lives in `sdk/streamlib-idents`'
> lockfile module — a crate this change **carries unchanged** by its own survivor-rewire
> bullet (deleted whole by `processor-class-identity`). The symbol's engine-side
> definition and callers are gone; the residue defers to the successor with its crate.
- REMOVED: native_lib_resolver
- REMOVED: spawn_python_native_subprocess_op
- REMOVED: sdk/streamlib-python/python/streamlib/subprocess_runner.py
- REMOVED: sdk/streamlib-python/python/streamlib/tests/test_clock.py
- REMOVED: sdk/streamlib-python/python/streamlib/tests/test_subprocess_runner_cleanup.py

  The old SDK's subprocess-polyglot runner and its tests, part of this change's
  `sdk/streamlib-python` removal (§Language SDKs: the subprocess-polyglot machinery is
  deleted with the module system); the wheel's `python -m streamlib._helper` is the
  successor.

  > ~~`tests/test_clock.py`~~ / ~~`tests/test_subprocess_runner_cleanup.py`~~ —
  > Corrected 2026-08-08: both were written as repo-relative suffixes, which name no path
  > from the repo root and reference nothing, so they were unprovable in either
  > direction. Spelled in full above.
- REMOVED: STREAMLIB_PYTHON_NATIVE_LIB
  (Added 2026-08-07 when in-process-hosting-ripout shipped: that change did not remove the
  env var, but every file naming it is this change's scope, each covered by a REMOVED
  bullet above — `native_lib_resolver`, `module_loader/from_source.rs`,
  `spawn_python_native_subprocess_op`, and the old SDK's `subprocess_runner.py` + its two
  tests — so the gate can prove the symbol reaches zero.
  `set_iceoryx2_resources` was **not** reassigned here: it is a live `GeneratedProcessor`
  trait method that survives this change, and only its cdylib vtable slot dies, under the
  `ProcessorVTable` / `core/plugin/` / plugin-abi / plugin-sdk removals above.)
- REMOVED: load_project_dylib
- REMOVED: pack_then_load_smoke
- REMOVED: cdylib_owns_tokio_runtime
- REMOVED: polyglot_linux_check_out_deno
- REMOVED: folder_backed_package_build

  The engine dylib-load test corpus. `cdylib_owns_tokio_runtime` already reaches zero —
  it exists nowhere in the tree but this file (confirmed 2026-08-08); the other four are
  live.
- REMOVED: tools/streamlib-build-orchestrator
- REMOVED: BuildOrchestrator
- REMOVED: tools/streamlib-cargo-build
- REMOVED: tools/streamlib-pack
- REMOVED: tools/streamlib-cross-rustc-fixture
> ~~- REMOVED: .slpkg~~ / ~~- REMOVED: streamlib_modules~~ — Deferred 2026-08-10 (owner
> ruling at the #1806 residue scrub). The engine-tree scrub the continuation below lists
> is **done** (#1806): `streamlib_home`, `streamlib-sdk`, `streamlib-macros`,
> `streamlib-error`, `xtask`, `Cargo.toml`, `.gitignore`/`.dockerignore`,
> `docker/README.md`, `sdk/vulkan-jpeg`, `test-fixtures`, `processor-schema` all reach
> zero. Every remaining reference lives in `sdk/streamlib-idents` (a live `.slpkg`
> package-source/resolver subsystem, incl. functional URL/path builders — not prose),
> `sdk/streamlib-jtd-codegen`'s registry-resolved E2E that drives it, and
> `docs/architecture/schema-identity-and-packaging.md` which describes it. All three are
> surfaces this change **carries unchanged** by its own survivor-rewire bullets;
> "scrub the mention" was unsatisfiable for jtd-codegen, whose `.slpkg` test is
> functional. The residue defers to `schema-free-ports` / `processor-class-identity`,
> which delete those crates whole.

  The format and the module directory die outright: distribution moves to
  Python's own mechanism (§Distribution — the wheel is the artifact), which is what
  `ARCHITECTURE.md`'s "deleted in full: `streamlib_modules/`, the `.slpkg` format,
  `streamlib.lock`" already states. Residue after this change's deletions is 27 files for
  `.slpkg` and 9 for `streamlib_modules`, all engine-tree scrubbing this change already
  implies — `core/streamlib_home.rs`'s walk, `operations_runtime.rs`, `streamlib-sdk`,
  `streamlib-macros`, `streamlib-error`, the CLI entrypoint, `xtask`, `Cargo.toml`,
  `.gitignore`/`.dockerignore`, `docker/README.md`, `sdk/vulkan-jpeg`, and
  `packages/test-fixtures/Cargo.toml:19`.

  `sdk/streamlib-jtd-codegen` and `sdk/streamlib-processor-schema` are among the `.slpkg`
  survivors: scrub the mention, but do not reshape either — the successor changes delete
  them.

  > ~~**[NEEDS DECISION]** …`.slpkg` cannot reach zero as written…~~ /
  > ~~**[NEEDS DECISION]** …485 occurrences, the large majority in `examples/**`…~~ —
  > Resolved by owner 2026-08-08. Neither bullet needed narrowing; the gate's *scope* was
  > wrong. PR #1791 drops `examples/**`, the distributable `packages/` entries,
  > `docs/learnings/**` and `docs/plan/**` from the content sweep — consumers that lag by
  > design, an empirical driver record that outlives the format, and the plan itself. The
  > `docs/plan/**` half was a deadlock rather than a preference (`ARCHITECTURE.md:41` names
  > both artifacts, and `/ship-change` gates at step 1 but folds `ARCHITECTURE.md` at step
  > 3); and 151 of `streamlib_modules`' 161 tracked files were `examples/**`, so both
  > bullets were satisfiable all along. Full reasoning in #1791.
- REMOVED: tools/streamlib-cli/src/commands/add.rs
- REMOVED: tools/streamlib-cli/src/commands/install.rs
- REMOVED: tools/streamlib-cli/src/commands/link.rs
- REMOVED: tools/streamlib-cli/src/commands/link/
- REMOVED: tools/streamlib-cli/src/commands/pkg.rs
- REMOVED: tools/streamlib-cli/src/commands/build_on_place.rs
- REMOVED: tools/streamlib-cli/src/commands/generate.rs
- REMOVED: tools/streamlib-cli/src/commands/schema.rs
- REMOVED: tools/streamlib-cli/src/commands/setup.rs
- REMOVED: tools/streamlib-cli/tests/add_remove.rs
- REMOVED: tools/streamlib-cli/tests/install_from_lock.rs
- REMOVED: tools/streamlib-cli/tests/link_unlink.rs
- REMOVED: tools/streamlib-cli/tests/pkg_publish_catalog.rs

  > ~~mutation verbs trimmed from `control.rs` and `mcp.rs`~~ — Superseded 2026-08-08:
  > both files are already gone. `control.rs` went with the observation-only control
  > plane (#1782) and `mcp.rs` with the CLI `mcp` verb (#1785, per
  > `mcp-served-with-the-node.md`). Nothing is left to trim.
- REMOVED: RunnerAutoBuild
- REMOVED: auto-build

  The sdk feature. Survivors that still name it — the api-server and wheel manifests,
  `runtime.rs`, `check_boundaries.rs` — are de-referenced with it.
- REMOVED: runtime/streamlib-runtime
> ~~- REMOVED: sdk/streamlib-deno~~ — Deferred 2026-08-10 (owner ruling at the #1806
> residue scrub). The crate, the untracked codegen dir, and every doc/comment/xtask
> reference are gone (#1806). The two remaining references are `sdk/streamlib-idents`'
> live `deno_sdk_entrypoint_path` link-marker field and its doc example — removing the
> field changes the link-marker wire shape of a crate this change carries unchanged.
> Defers to `processor-class-identity`, which deletes `streamlib-idents` whole.
- REMOVED: sdk/streamlib-deno-native
- REMOVED: spawn_deno_subprocess_op
- REMOVED: sdk/streamlib-processor-extract
- REMOVED: sdk/streamlib-python/python/streamlib/_processor_registry.py
- REMOVED: sdk/streamlib-python/python/streamlib/extract_processors.py
- REMOVED: sdk/streamlib-python/setup.py
- REMOVED: sdk/streamlib-python/python/streamlib/cgl_context.py

  > ~~`setup.py`~~ — Corrected 2026-08-08: it is at `sdk/streamlib-python/setup.py`, not
  > under `python/streamlib/`. The bullet named a path that does not exist, so it would
  > have passed green whatever the gate checked.
> ~~- REMOVED: schemas/streamlib.schema.json~~ — Superseded 2026-08-09 (owner
> ratification at the #1715 `/ship-change` gate). This JSON Schema validates
> `streamlib.yaml`, the package manifest that **survives #1715** (the schema-ident /
> package surface dies in `schema-free-ports` / `processor-class-identity`, not here).
> Deleting it in #1715 was premature and left dangling `$schema` refs in the surviving
> `streamlib.yaml` files. It moves to the successor changes, which retire the manifest
> and its schema together. Not #1715's residue.
- REMOVED: scripts/sync-inter-crate-versions.py
- REMOVED: packages/test-fixtures-abi-mismatch
- REMOVED: docs/architecture/plugin-abi.md
- REMOVED: docs/architecture/package-development-model.md
- REMOVED: docs/architecture/package-source.md
- REMOVED: docs/architecture/package-staging-layout.md
- REMOVED: docs/architecture/runtime-module-materialization.md
> ~~- REMOVED: docs/architecture/schema-identity-and-packaging.md~~ — Superseded
> 2026-08-09 (owner ratification at the #1715 `/ship-change` gate). This doc describes
> the schema-ident core in `streamlib-idents` / `streamlib-jtd-codegen`, which this
> change **carries through unchanged** (see the survivor-rewire bullet: both die in
> `schema-free-ports` / `processor-class-identity`). 15+ surviving crates link it; per
> docs-policy an architecture doc describes current shipped state, so it stays until its
> subject dies in the successor changes.
> ~~- REMOVED: docs/architecture/subprocess-rhi-parity.md~~ — Superseded 2026-08-09
> (owner ratification at the #1715 `/ship-change` gate). Its cross-process
> DMA-BUF / OPAQUE_FD subject lives on in the surviving `streamlib-consumer-rhi`
> carve-out (6+ surviving-code links, including `check-boundaries`); the cdylib framing
> is rewritten, not the doc deleted. Per docs-policy it describes current shipped state.
- REMOVED: docs/architecture/cdylib-reachability.md
- REMOVED: docs/architecture/zero-ceremony-authoring.md

  Plus ABI-half edits in `adapter-authoring`, `adapter-runtime-integration`,
  `surface-adapter`, `texture-registration`, `compute-kernel`, `ray-tracing-kernel`,
  `third-party-gpu-backends`; historical notes on the two cdylib/slpkg learnings.

## Companion operating-model PR (dedicated, per the flow rule)

Retires `.claude/rules/plugin-boundary.md`, agents `plugin-abi-expert` +
`package-source-expert` (re-scopes `polyglot-ipc-expert`), skills
`author-and-submit-processor` + `hot-swap-live-processor`; updates CLAUDE.md +
`engine-doctrine.md` (ABI layout-test non-negotiable, packages-lag doctrine,
`streamlib.yaml` purity line); lifts `.claude/settings.json` deny rules for
`packages/**`/`examples/**` as those trees are deleted; prunes issue-template
package/plugin fields; rewrites the workspace-comment block in `Cargo.toml`.

## What follows this change — derive these next

When the contract-deletion ticket (#1715) lands, two approved changes are unblocked and
**their tickets do not exist yet**. Run, in order:

1. `/derive-tickets docs/plan/changes/schema-free-ports.md`
2. `/derive-tickets docs/plan/changes/processor-class-identity.md` (blocked on 1)

Both were deliberately left unticketed: they are several tickets deep behind #1715, and
the tree they describe is largely deleted by this change, so any file:line inventory
written earlier would be stale before it was picked up.

## Dispositions — deferred re-authoring (recorded, not ticketed)

Old consumers are re-authored in their own planning sessions after the wheel exists:
mavlink-class → plain-Python packages; GPU-heavy → Python packages with native wheels
exposing handles; `polyglot-*` + `camera-deno-subprocess` examples → deleted, replaced
by normal Python app examples; MoQ package → rides surviving `runtime/streamlib-moq`,
disposition when §Networking is scheduled.
