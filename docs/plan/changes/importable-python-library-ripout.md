# Change: importable-python-library-ripout

**Change B of the pivot pair — rip out the old world.** Blocked on
`importable-python-library.md` (Change A) shipping: the wheel, built-ins, exchange
surface, and dev experience must exist before their predecessors are deleted. Same ADR.
The REMOVED inventory below was verified by a dedicated audit agent against the tree
(reverse-dependency sweep, CI/xtask enumeration) on 2026-08-02.

## MODIFIED — survivor rewires (sequenced before the deletions they unblock)

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
  subprocess-parity tests.
- **`sdk/streamlib-python-native` retires into the wheel**: helper processes import
  the wheel itself (one native artifact, per §Distribution); the iceoryx2 transport
  and `escalate`/`subprocess_bridge` re-scope to same-interpreter spawns
  (exec-of-`sys.executable`, never fork); venv-provisioning and
  `native_lib_resolver.rs`'s cdylib resolution die. Verify-before-delete: the
  iceoryx2 log-forwarding pair in `core/plugin/` — keep a successor if helper-process
  logging rides it.
- **Engine host-services strip**: the dual-mode cdylib branch comes out of the ~10
  retained files that dispatch through `core/plugin/host_services` (`gpu_context`,
  `texture`, `bus`, `iceoryx2/input`, `vulkan_present_target`, `host_rhi`,
  `processor_instance_factory`, `runtime_shutdown_request`); `streamlib-sdk` drops its
  `core::plugin` re-exports and the `auto-build` feature.
- **`core/streamlib_home.rs`** re-scopes: the `streamlib_modules`/`packages/` walk
  dies; logging paths and the node registry keep a home-dir resolver.
- **`sdk/streamlib-idents` / `sdk/streamlib-processor-schema`**: manifest/lockfile
  modules die (`app_modules`, `archive`, `catalog`, `lockfile`, `manifest`,
  `package_source`, `path_artifact_guard`, `release`, `resolver`); the schema-ident
  core behind the JTD seam survives; `streamlib-jtd-codegen` goes internal-only.
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
- The contract-deletion ticket lands as a **pre-approved stacked-PR structure** (the
  86-file blast radius is unreviewable as one diff).

## REMOVED

Each bullet is a pattern the ship gate verifies is gone from the tree.

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

## Dispositions — deferred re-authoring (recorded, not ticketed)

Old consumers are re-authored in their own planning sessions after the wheel exists:
mavlink-class → plain-Python packages; GPU-heavy → Python packages with native wheels
exposing handles; `polyglot-*` + `camera-deno-subprocess` examples → deleted, replaced
by normal Python app examples; MoQ package → rides surviving `runtime/streamlib-moq`,
disposition when §Networking is scheduled.
