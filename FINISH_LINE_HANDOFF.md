# Plugin-SDK Extraction — Finish-Line Handoff (dev.10 republish + proof)

> **Transient working doc.** Delete when the proof lands. Supersedes the
> "Next steps" of `PLUGIN_SDK_HANDOFF.md` — Phases 1, 2a, 2b and the
> vulkan-jpeg split described there are DONE. Only the dev.10 republish +
> the crash-vanishes proof remain.

## TL;DR

The whole engine-free SDK extraction is built and committed. **All five
drone-racer pipeline packages are engine-free.** What's left is purely
operational: publish a coherent `0.4.35-dev.10` (cargo crates + `.slpkg`
packages), point the drone-racer host + racer-pilot at it, and run the
proof. With every plugin off the engine there is only ONE engine copy (the
host), so the concurrent-`vkDeviceWaitIdle` race that caused the SIGSEGV is
gone by construction.

## Branch / repos / env

- streamlib repo: `~/Repositories/tatolab/streamlib`, branch
  `fix/vulkan-jpeg-cdylib-device-panic`. **Commit locally; do NOT push
  without Jonathan's OK.** (PR #1205 tracks this branch with separate fixes;
  decide PR-split at the end.)
- drone-racer repo: `~/Repositories/tatolab/drone-racer` (proprietary layer).
  `/add-dir` it so Read/Edit/Write reach it.
- Registry: self-hosted Gitea at `http://localhost:3300`, org `tatolab`.
- **Every publish + every drone-racer build needs the token sourced first:**
  ```bash
  source ~/Repositories/tatolab/streamlib/scripts/gitea/registry-token.local.sh
  # publish-crates.sh additionally needs:
  export CARGO_REGISTRIES_GITEA_TOKEN="Bearer $GITEA_PUBLISH_TOKEN"
  ```
- GPU box: NVIDIA RTX 3090, driver 595.71.05. vivid camera at `/dev/video0`.
- **Bash/ripgrep output redacts long identifiers to a single letter in this
  environment — use the Read/Glob tools for code content + exact identifiers.**

## What is DONE (11 commits; all validated, workspace + SDK green)

1. `refactor(plugin-sdk): collapse the engine Error duplicate onto streamlib-error`
2. `refactor(plugin-sdk): move processor descriptor types to streamlib-processor-schema`
3. `harden(plugin-sdk): #[repr(C)] + layout-lock the RuntimeContext ABI views`
4. `feat(plugin-sdk): scaffold streamlib-plugin-sdk with the engine-free shared surface`
5. `feat(plugin-sdk): relocate the cdylib arm of the dual-mode plugin machinery`
6. `feat(plugin-sdk): auto-detect the real SDK crate in #[processor] + make export_plugin! SDK-agnostic`
7. `fix(gitea-publish): bump the plugin/ zone Cargo.tomls in the dev-version rewrite`
8. `feat(plugin-sdk): migrate network / vadr-vision / mavlink packages to the engine-free SDK`
9. `feat(plugin-sdk): relocate the Vulkan-compute GPU FullAccess surface into the SDK` (Phase 2b)
10. `feat(plugin-sdk): split vulkan-jpeg — engine-free plugin/vulkan-jpeg + parked nvJPEG`

- `plugin/streamlib-plugin-sdk` is the engine-free authoring SDK (cdylib arm of
  the dual-mode types + the full Vulkan-compute GPU FullAccess surface). `cargo
  tree -p streamlib-plugin-sdk | grep -c streamlib-engine` == 0.
- The 5 pipeline packages are engine-free: **racer-pilot** (drone-racer repo),
  **network / vadr-vision / mavlink** (`packages/`), **jpeg** (`packages/jpeg`,
  now via the engine-free `plugin/vulkan-jpeg`, Vulkan-compute only).
- nvJPEG is parked DISABLED at
  `libs/streamlib-engine/src/vulkan/_nvjpeg_impl_pending_/` (NOT in the module
  tree), tracked by **issue #1206** (cdylib-safe nvJPEG via new OPAQUE_FD /
  device-UUID / `third_party_gpu_capabilities` FullAccess primitives — a perf
  follow-up, NOT competition-blocking).
- `cargo run -p xtask -- check-boundaries` is clean.
- `0.4.35-dev.9` was published earlier but is **STALE** — it predates Phase 2b
  (SDK GPU surface) and the split (engine-free `plugin/vulkan-jpeg`). The
  packages currently pin dev.9 in their cargo deps; they will NOT build until
  dev.10 exists. **Must publish dev.10.**

## FINISH LINE — coordinated dev.10 republish + proof

### Why dev.10 must be coherent across host + all plugins

The crash is the **concurrent-`vkDeviceWaitIdle` race across multiple
statically-linked engine copies** (gdb-pinned: `HostVulkanDevice::wait_idle →
vkDeviceWaitIdle → libnvidia-glcore`, inside `with_cdylib_scope`'s post-setup
wait). Each engine-linked plugin = another copy whose per-copy queue mutexes
don't coordinate across copies. Fix = no plugin links the engine. **Also:** a
host on dev.N + a plugin built against a DIFFERENT engine version reintroduces
the `host_vulkan_device_arc` layout-skew hazard (`slpkg-raw-device-rhi-construction`)
— so host + every plugin MUST share one engine build (dev.10).

### Step 1 — bump pins to dev.10

In `~/Repositories/tatolab/streamlib`:
- `packages/{network,vadr-vision,mavlink,jpeg}/Cargo.toml`: bump every
  `streamlib-plugin-sdk` / `streamlib-macros` / `streamlib-plugin-abi` /
  `streamlib-jtd-codegen` (and jpeg's `vulkan-jpeg`) pin `0.4.35-dev.9` →
  `0.4.35-dev.10`. (Preserve each file's `{version=...}` vs `{ version = ... }`
  spacing style.)
- Bump each of those 4 packages' `streamlib.yaml` `version:` by one patch (e.g.
  jpeg 1.0.5 → 1.0.6, network 1.0.0 → 1.0.1, etc.) so `publish-packages.sh`
  emits a FRESH `.slpkg` version the runner's `any_version` resolver picks over
  the stale engine-linked one. (Confirm the current versions first:
  `grep -A2 '^package:' packages/*/streamlib.yaml` or read each.)
- In `~/Repositories/tatolab/drone-racer`:
  - `racer/packages/racer-pilot/Cargo.toml`: bump streamlib-plugin-sdk /
    streamlib-macros / streamlib-plugin-abi / streamlib-jtd-codegen dev.9 → dev.10.
  - `racer/runner/Cargo.toml`: `streamlib = "0.4.35-dev.9"` → `"0.4.35-dev.10"`.

### Step 2 — publish the cargo crates at dev.10

```bash
cd ~/Repositories/tatolab/streamlib
source scripts/gitea/registry-token.local.sh
export CARGO_REGISTRIES_GITEA_TOKEN="Bearer $GITEA_PUBLISH_TOKEN"
STREAMLIB_PUBLISH_ALL_LIBS=1 ./scripts/gitea/publish-crates.sh --dev 10
```
ALL_LIBS mode publishes every internal lib (engine w/ nvJPEG parked, the SDK w/
Phase 2b, `plugin/vulkan-jpeg`, plugin-abi, macros, processor-schema,
streamlib-error, consumer-rhi, …) in topo order. The script bumps versions
in-place + restores on exit (commit 8bf03d69 made it also cover the `plugin/`
zone). Verify it printed `done — streamlib closure @ 0.4.35-dev.10 published`
and that the crates are live:
```bash
for c in streamlib streamlib-plugin-sdk vulkan-jpeg streamlib-engine; do
  idx=$(echo "$c" | sed -E 's/^(..)(..).*/\1\/\2/')
  echo "$c dev.10: $(curl -s "http://localhost:3300/api/packages/tatolab/cargo/$idx/$c" | grep -c '0.4.35-dev.10')"
done
```

### Step 3 — repack the engine-free `.slpkg` packages

```bash
cd ~/Repositories/tatolab/streamlib
source scripts/gitea/registry-token.local.sh   # sets GITEA_PUBLISH_TOKEN
./scripts/gitea/publish-packages.sh network vadr-vision jpeg mavlink
```
This builds the `streamlib` CLI (release) then packs each named package's
source as a fresh `.slpkg` to the generic registry at its (bumped)
`streamlib.yaml` version. They are source-only — the runner's orchestrator
builds them at load time against the dev.10 cargo crates (so Step 2 must come
first).

### Step 4 — build the host + run the proof

```bash
cd ~/Repositories/tatolab/drone-racer/racer/runner
source ~/Repositories/tatolab/streamlib/scripts/gitea/registry-token.local.sh
# Optional: clear stale orchestrator-built packages so dev.10 sources rebuild:
#   rm -rf ../.streamlib/cache/packages/*
cargo build      # builds racer-runner (host, streamlib@dev.10)
ulimit -c 0
for i in 1 2 3 4 5; do
  RUST_LOG=warn timeout --kill-after=5 25 ./target/debug/racer-runner > /tmp/proof-$i.log 2>&1
  echo "run $i: exit=$? setup=$(grep -ac 'Setup completed\|Initialized (GPU\|RacerPilot.*setup' /tmp/proof-$i.log) crash=$(grep -ac 'dumped core\|SIGSEGV' /tmp/proof-$i.log)"
done
```
- **SURVIVED / FIXED**: exit 124 (timeout) + no `dumped core` + the processors
  log setup (`UdpSource: setup`, `JpegDecoder` init, `RacerPilot setup`). Run
  3–5× — historically intermittent, but with 0 engine-linked plugins it should
  be clean every run.
- **STILL CRASHING**: exit 139 / `dumped core`.
- **No UDP source needed** — the crash (if any) is in setup, before data flows.
- Do NOT wrap the proof in gdb/strace LIVE to "confirm" a clean run — any
  overhead hid the original intermittent race. (For a DETERMINISTIC crash,
  gdb is fine and is how the earlier diagnosis was done.)

### If it STILL crashes — diagnosis playbook

1. Confirm the loaded plugins are actually engine-free + dev.10:
   `find ~/Repositories/tatolab/drone-racer/racer/.streamlib/cache/packages -name '*.so' | while read so; do echo "$so engine=$(nm "$so" 2>/dev/null | grep -c streamlib_engine)"; done`
   — all must be 0. If non-zero, a stale cached `.slpkg` was loaded → clear
   `racer/.streamlib/cache/packages/*` and rebuild.
2. Verify every loaded plugin built against dev.10 (no version skew with the
   host). A plugin on an older engine version reintroduces the
   `host_vulkan_device_arc` layout-skew crash.
3. Get the crash site (post-mortem doesn't shift the race):
   `coredumpctl gdb` or run once under `gdb -batch -ex run -ex 'bt 20'`. The
   ORIGINAL crash was `HostVulkanDevice::wait_idle → libnvidia-glcore`
   (concurrent `vkDeviceWaitIdle`) and `vulkan_jpeg::host_vulkan_device_arc`
   (version-skew transit) — both should be gone now.

## Key learnings from this session (so a fresh context doesn't re-derive)

- The original SIGSEGV had TWO modes, both now removed: (a) jpeg's Auto-mode
  nvJPEG CUDA probe (`cudaSetDevice`) + the `host_vulkan_device_arc` raw-device
  transit (gone — jpeg is Vulkan-compute via engine-free `plugin/vulkan-jpeg`);
  (b) concurrent `vkDeviceWaitIdle` across the multiple engine copies (gone once
  no plugin links the engine).
- Zone model (Jonathan): `plugin/` = engine-free code SAFE inside a plugin
  cdylib (the SDK, plugin-abi, shared helper libs like `vulkan-jpeg` — the
  npm-common-lib analogy). `libs/` = engine-internal shared code. Engine-only
  code folds INTO the engine (not a standalone `libs/` crate) so other libs
  don't bypass the engine SDK. nvJPEG is engine-internal (raw device / OPAQUE_FD)
  → parked in the engine, no blessed plugin API yet.
- `#[processor]` auto-detects the real SDK crate via `proc-macro-crate`
  (streamlib-plugin-sdk → streamlib facade → `::streamlib` fallback) and
  centralizes all SDK-path resolution; `export_plugin!` is now SDK-agnostic
  (calls `#[processor]`-generated install fns). Real names, no `streamlib`
  aliasing. schema_ident!/module_ident! still emit the facade path (host-only
  today) — thread the root through them when a plugin needs them.
- `vulkan-compute` is streamlib's native JPEG decoder; nvJPEG is a perf
  optimization (dedicated HW). 1080p@30Hz JPEG decode is within budget on the
  native path, so nvJPEG is NOT required for the AGP competition.

## After the proof passes

- Write the camera-display / E2E test report if relevant (per `docs/testing.md`).
- Update `docs/learnings/slpkg-raw-device-rhi-construction.md` with the
  gdb-confirmed `host_vulkan_device_arc` transit crash (concrete evidence the
  hazard is real, not theoretical).
- Consider sweeping `docs/architecture/` for the plugin/ zone model + the
  engine-free SDK (currently described only in `plugin/CLAUDE.md` +
  `PLUGIN_SDK_HANDOFF.md`).
- Decide PR shape with Jonathan (this branch vs PR #1205). Delete the two
  handoff `.md` files.
- frame-tap is NOT engine-free yet (needs a GPU-readback FullAccess primitive)
  — out of scope for the proof; a separate follow-up if it matters.
