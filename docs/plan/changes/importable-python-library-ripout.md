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
- **`sdk/vulkan-jpeg`** rewires its pervasive `streamlib_plugin_sdk::sdk::*` imports
  to `streamlib-sdk`; the cdylib-safety constraint drops.
- **`sdk/streamlib-macros`**: the `#[processor]` attribute grammar relocates out of
  doomed `streamlib-processor-extract` into a surviving home (per the superseded
  manifest-extraction ADR: exactly one parser, still true).
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
  > **Scale.** The retained surface is materially larger than eight files and spans
  > the whole RHI — the Vulkan kernel and command-recorder files alone are denser than
  > anything on the list. It resists enumeration because the coupling has two distinct
  > shapes that no single pattern catches: a runtime branch on `host_callbacks()`, and
  > a direct `host_services::host_*_vtable()` call with no `host_callbacks()` mention
  > at all. Three of the eight named — `iceoryx2/input`, `vulkan_present_target`,
  > `processor_instance_factory` — are the second shape only. This change states no
  > file count deliberately: two successive audits produced numbers that measured
  > different predicates. **The inventory belongs to #1715, derived by reading the RHI
  > at implementation time.**
  >
  > **Sequencing.** The strip does not precede the deletions; see the stacked-PR
  > bullet. The clause moving `streamlib-sdk` off its `core::plugin` re-exports and
  > the `auto-build` feature moves there with it, unchanged.
- **`core/streamlib_home.rs`** re-scopes: the `streamlib_modules`/`packages/` walk
  dies; logging paths and the node registry keep a home-dir resolver.
- **`sdk/streamlib-idents` / `sdk/streamlib-processor-schema`**: manifest/lockfile
  modules die (`app_modules`, `archive`, `catalog`, `lockfile`, `manifest`,
  `package_source`, `path_artifact_guard`, `release`, `resolver`). The schema-ident core
  and `streamlib-jtd-codegen` survive **this** change only — both are deleted outright by
  `schema-free-ports.md` / `processor-class-identity.md` (amended 2026-08-03; this file
  previously preserved the ident core behind the JTD seam and demoted the codegen crate
  to internal-only, which `[schema-free-ports]` supersedes). Do not invest in reshaping
  either; carry them unchanged and let the successor changes delete them.
- **`packages/test-fixtures`** loses its plugin/cdylib arm (the in-process
  attribute-macro tests keep it alive); `test-fixtures-abi-mismatch` dies whole.
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
  blast radius is unreviewable as one diff. **The engine host-services strip is one of
  its stack levels** (moved here 2026-08-05, owner-ruled), alongside the
  `core/plugin/` deletion, carrying `streamlib-sdk` off its `core::plugin` re-exports
  and the `auto-build` feature. The strip has no earlier home because **the cdylib
  test corpus asserts exactly the behaviour it removes**: the 22 `load_project_dylib_*`
  / `pack_then_load_smoke` / `cdylib_owns_tokio_runtime` engine tests, the 4
  `export_plugin_*` plugin-abi tests, `twin_drift_guard` and `check-cdylib-reach`.
  Those die on this ticket and nowhere earlier, so no earlier stopping point is green.
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
    clause.
- **What must survive the strip**: the helper children's privileged-access route —
  the escalate GPU ops carried over the subprocess IPC path, which is independent of
  `host_services` and always was. This is distinct from the cdylib's escalate route
  (`host_services/gpu_context/limited/escalate.rs`), which dies with the ABI; the two
  share a name and nothing else. #1743 unblocked #1714 without the strip on exactly
  this basis.

## REMOVED

Each bullet is a pattern the ship gate verifies is gone from the tree.

Known gate defect, recorded here because it changes what these bullets are worth: the
gate takes everything after `- REMOVED: ` on one line and greps it as a fixed string,
so any bullet carrying two `/`-joined items or a parenthetical can never match and
passes vacuously. Several below are in that state. Splitting them is the fix; it is
not this change's scope, and until then a passing gate is weaker evidence than it
looks.

`streamlib-adapter-cuda-helpers` is already gone, deleted early by #1743 / PR #1751
with its three tests re-homed into `streamlib-adapter-cuda`; its bullet's `-cuda-abi`
half still belongs to this ticket.

- REMOVED: `runtime/streamlib-plugin-abi`
- REMOVED: `sdk/streamlib-plugin-sdk`
- REMOVED: `export_plugin!` / `install_host_services` / `HostServices` / `ProcessorVTable` / `PluginAbiObject`
- REMOVED: `adapters/streamlib-adapter-abi`
- REMOVED: `streamlib-adapter-vulkan-abi` / `streamlib-adapter-vulkan-helpers`
- REMOVED: `streamlib-adapter-opengl-abi`
- REMOVED: `streamlib-adapter-cpu-readback-abi` / `streamlib-adapter-cpu-readback-helpers`
- REMOVED: `streamlib-adapter-cuda-abi` / `streamlib-adapter-cuda-helpers`
- REMOVED: `streamlib-adapter-skia-abi`
- REMOVED: `runtime/streamlib-engine/src/core/plugin/` (vtable backings,
  `build_fingerprint`, `twin_drift_guard`, load handshake)
- REMOVED: `runtime/streamlib-engine/src/core/runtime/module_loader/`
- REMOVED: `runtime/streamlib-engine/src/core/runtime/install.rs` / `add_modules_from_lockfile`
- REMOVED: `native_lib_resolver`
- REMOVED: `load_project_dylib` (engine dylib-load test corpus) / `pack_then_load_smoke` /
  `cdylib_owns_tokio_runtime` / `polyglot_linux_check_out_deno` / `folder_backed_package_build`
- REMOVED: `tools/streamlib-build-orchestrator` / `BuildOrchestrator`
- REMOVED: `tools/streamlib-cargo-build`
- REMOVED: `tools/streamlib-pack`
- REMOVED: `tools/streamlib-cross-rustc-fixture`
- REMOVED: `.slpkg`
- REMOVED: `streamlib_modules`
- REMOVED: `tools/streamlib-cli/src/commands/{add,install,link,pkg,build_on_place,generate,schema,setup}.rs`
  (+ `commands/link/`; mutation verbs trimmed from `control.rs` and `mcp.rs`; doomed
  CLI tests `add_remove`/`install_from_lock`/`link_unlink`/`pkg_publish_catalog`)
- REMOVED: `RunnerAutoBuild` and the sdk `auto-build` feature
- REMOVED: `runtime/streamlib-runtime`
- REMOVED: `sdk/streamlib-deno` / `sdk/streamlib-deno-native`
- REMOVED: `spawn_deno_subprocess_op`
- REMOVED: `sdk/streamlib-processor-extract`
- REMOVED: `sdk/streamlib-python/python/streamlib/_processor_registry.py` /
  `extract_processors.py` / `setup.py` / `cgl_context.py`
- REMOVED: `schemas/streamlib.schema.json`
- REMOVED: `scripts/sync-inter-crate-versions.py`
- REMOVED: `packages/test-fixtures-abi-mismatch`
- REMOVED: `docs/architecture/{plugin-abi,package-development-model,package-source,package-staging-layout,runtime-module-materialization,schema-identity-and-packaging,subprocess-rhi-parity,cdylib-reachability,zero-ceremony-authoring}.md`
  (plus ABI-half edits in `adapter-authoring`, `adapter-runtime-integration`,
  `surface-adapter`, `texture-registration`, `compute-kernel`, `ray-tracing-kernel`,
  `third-party-gpu-backends`; historical notes on the two cdylib/slpkg learnings)

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
