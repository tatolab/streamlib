# Plugin-SDK Extraction — Handoff

> **Transient working doc.** Delete this file when the work lands (or
> convert it to a GitHub issue). It is NOT a permanent architecture doc.

## TL;DR

Out-of-`libs` plugins (racer-pilot, jpeg, …) depend on the `streamlib`
crate (`libs/streamlib-sdk`), which is a pure re-export facade over
`streamlib-engine`. So every plugin cdylib **statically links a second
full copy of the engine**. Two engine copies in one process fight over
duplicated process-global state and corrupt the NVIDIA driver during
concurrent GPU setup → the drone-racer `vkCreateComputePipelines`
SIGSEGV. **Fix:** a thin engine-free `streamlib-plugin-sdk` that crosses
only the plugin ABI; plugins link it instead of `streamlib`. **Proof:**
migrate `racer-pilot` to it and watch the crash vanish.

## Why — root cause (confirmed, not theory)

Pinned by **bisecting the drone-racer pipeline at full speed**:
- vision-only (udp→depay→jpeg): clean (jpeg used nvJPEG, CUDA worked).
- +telemetry (5 procs): clean.
- **+racer-pilot (6 procs): crash.**
- racer-pilot rebuilt at dev.8 (host's version): **still crashes** → not version.
- 7 registry-only procs (no racer-pilot, more processors): clean → not count.
- racer-pilot cdylib **loaded but processor not instantiated**: clean → it's the processor's *setup*, not the dlopen.

Mechanism: the crash is **timing-dependent and instrumentation-invisible**
— gdb, the Khronos validation layer, AND an `LD_PRELOAD` sigaction logger
ALL dodge it (any overhead shifts the race). The signal-clobber lead was
**refuted** (the engine installs no SIGSEGV/SIGBUS handler — only Rust's
normal stack-overflow guard; the `SA_RESETHAND` probe in an early strace
was a cargo/rustc *build* subprocess, 0 in the runtime). The fault itself
is a NULL-deref deep in `libnvidia-glcore` (`mov 0x28(%rdi),%rbx`, rdi=0).
Our call is spec-valid (spirv-val clean, validation clean, `gfxrecon-replay`
of the captured calls runs clean in a quiet process). The jpeg
`cudaErrorInsufficientDriver` we chased was a *downstream symptom* of the
same driver corruption, not a version problem.

Conclusion: a second engine copy's duplicated globals racing the host's is
the cause. Removing the engine from the plugin (this work) deletes the
entire duplicate-engine surface at once.

## Decisions (signed off by Jonathan)

1. **Zones are folders, not convention.** `libs/` = engine-internal
   (compiled WITH the engine). New top-level **`plugin/`** = engine-free
   (safe in a plugin cdylib). Dependency arrow points **`libs/` →
   `plugin/`, never back**. See `plugin/CLAUDE.md` + `libs/CLAUDE.md`.
2. **Plugins depend on `streamlib-plugin-sdk` by its real, explicit name**
   — no disguised-`streamlib` aliasing. (Jonathan: "be specific.") Resolve
   the `#[processor]` macro's emitted `::streamlib::sdk::*` paths cleanly
   — see Phase 2 note.
3. **`Error` lives in a tiny shared `streamlib-error` crate** (done) — one
   canonical type the engine and the plugin-SDK both use; no drift.
4. **Gradual rollout:** crate + racer-pilot crash-vanishes proof FIRST,
   then jpeg + the sweep. Moving the 3 already-engine-free crates
   (`streamlib-plugin-abi`, `streamlib-processor-schema`,
   `streamlib-consumer-rhi`) from `libs/` into `plugin/` is a **mechanical
   follow-up** AFTER the proof — do NOT gate the proof on it.

## The architecture

```
PLUGIN zone (plugin/) — engine-free, safe inside a plugin cdylib
  streamlib-error (DONE) · streamlib-plugin-sdk (TODO)
  [+ later, moved from libs/: plugin-abi · processor-schema · consumer-rhi]
  [+ shared helper libs, e.g. an engine-free vulkan-jpeg]

INTERNAL zone (libs/) — links the engine, never in a cdylib
  streamlib-engine · streamlib (SDK facade, in-process apps) · build-orchestrator · cli

A plugin cdylib deps ONLY plugin/ crates (+ plugin-abi/processor-schema/
consumer-rhi which are engine-free and will move to plugin/). The host
links the engine. xtask check-boundaries enforces: no libs/ dep from a
plugin/ crate; no libs/ dep from a crate-type=cdylib package.
```

**Why a thin SDK is even needed (the subtlety):** the plugin ABI
(`streamlib-plugin-abi`, already engine-free) is only the *wiring* —
`#[repr(C)]` vtables, `HostServices`, `export_plugin!`. The *authoring
types* (`ReactiveProcessor`, `RuntimeContextFullAccess`,
`GpuContextFullAccess`) are **engine types** whose vtable-routing logic is
baked INSIDE them as `if host_callbacks().is_some() { route via ABI } else
{ run engine code }`. They're dual-mode (one type, host + cdylib). To get
the type you must compile the engine in. The plugin-SDK supplies thin
**vtable-only** versions (the cdylib arm only) so the plugin never links
the engine. The `streamlib-consumer-rhi` crate (which the python/deno
subprocess cdylibs already use INSTEAD of `streamlib`) is the working
precedent — `cargo tree -p streamlib-python-native | grep -c '^streamlib v'`
== 0.

## Done — the foundation (committed, additive, compiling)

- `plugin/` zone created.
- `plugin/streamlib-error/` — `Error` + `Result` + `PortDirection`
  relocated out of the engine. Deps: thiserror, anyhow,
  `../../libs/streamlib-processor-schema` (for `SchemaIdent`),
  `../../libs/streamlib-consumer-rhi` (Linux, for `From<ConsumerRhiError>`).
  `cargo check -p streamlib-error` is green. Registered in the workspace.
  NOTE: this is currently a *duplicate* of the engine's
  `core::error::Error` — Phase 1 collapses them.
- `plugin/CLAUDE.md` + `libs/CLAUDE.md` — zone guides.
- Root `Cargo.toml` — `plugin/streamlib-error` added to `members`.

## Next steps — the heavy phase

### Phase 1 — point the engine at `streamlib-error` (collapse the duplicate)
- Replace the body of `libs/streamlib-engine/src/core/error.rs` with:
  `pub use streamlib_error::{Error, PortDirection, Result};`
- Add to `libs/streamlib-engine/Cargo.toml` deps:
  `streamlib-error = { path = "../../plugin/streamlib-error", version = "0.4.30", registry = "gitea" }`
- The 3 remaining `impl From<…> for Error` in the engine (`module_loader/
  errors.rs:276,308` AddModuleError/RemoveModuleError; `vulkan_texture_
  readback.rs:694` TextureReadbackError) are all LOCAL engine types →
  orphan-rule OK, they stay. The `From<ConsumerRhiError>` already moved
  into `streamlib-error`.
- Verify: `cargo check -p streamlib-engine` (slow). Watch for any code
  that constructed `PortDirection`/`Error` via `crate::core::error::…` —
  it still resolves through the re-export.

### Phase 2 — build `plugin/streamlib-plugin-sdk` (THE intricate part)
- New crate, deps: `../streamlib-error`, `../../libs/streamlib-plugin-abi`,
  `../../libs/streamlib-processor-schema`, `../../libs/streamlib-consumer-rhi`,
  serde. **NO streamlib-engine.**
- Reproduce the module layout the `#[processor]` macro emits against:
  `sdk::{processors, context, descriptors, error, iceoryx2, execution, rhi}`
  (the macro hard-codes `::streamlib::sdk::*` — see codegen.rs refs in the
  map). Per decision #2, plugins dep the crate by name; resolve the macro
  path either by (a) parameterizing the macro's path root, or (b) the
  cargo `package = "…"` rename. EVALUATE which is cleaner in fresh context;
  decision #2 ("be specific") leans toward NOT disguising it as `streamlib`.
- Relocate the **thin `{handle,vtable}`/ScopeToken arms** of
  `RuntimeContext{Full,Limited}Access` + `GpuContext{Full,Limited}Access`:
  keep ONLY the cdylib (ScopeToken / vtable-marshal) arm; DROP the
  `Boxed`/host arm and the `host_callbacks()` branch. The host `*Inner`
  backings + `HOST_*_VTABLE` impls STAY in the engine. The Texture /
  PixelBuffer / kernel-handle / OutputWriter / InputMailboxes types are
  ALREADY `#[repr(C)]{handle,vtable}` — relocate the views, leave the host
  backings.
- Relocate the processor traits, port markers, `ProcessorSpec`, and the
  registration glue (`install_host_services`, `RegisterHelper::register`'s
  cdylib `register_via_callback` arm, `vtable_for::<P>`, the
  `ProcessorVTable` thunks). The host `PROCESSOR_REGISTRY.register::<P>`
  arm stays in the engine.
- This is fiddly. Go incrementally; `cargo check` after each move. The
  cdylib arms ALREADY exist and are pure ABI marshaling — it's relocation
  + "delete the host arm in the thin copy," not new design.

### Phase 3 — migrate racer-pilot → THE PROOF (the milestone)
- In the drone-racer repo (`/add-dir` it): edit
  `racer/packages/racer-pilot/Cargo.toml` — replace the `streamlib` dep
  with `streamlib-plugin-sdk` (+ keep streamlib-macros + streamlib-plugin-abi).
  Remove anything that drags the engine.
- Update `racer/packages/racer-pilot/src/{lib,pilot}.rs` imports
  (`streamlib::sdk::*` → the plugin-SDK paths).
- Rebuild + run the FULL drone-racer (see "How to run the proof") **several
  times** — the crash is intermittent at full speed; ~3–5 clean runs =
  proof. exit 139 / "dumped core" = still crashing; exit 124 (timeout) +
  "Setup completed" + jpeg "Initialized" with NO core = FIXED.

### Phase 4 — jpeg + sweep + boundary check (after the proof)
- Close jpeg's one engine-link: `vulkan-jpeg/src/simple_decoder.rs:217`
  does `host_vulkan_device_arc()?.third_party_gpu_capabilities()` (the ONLY
  raw-device use; its kernels already go through FullAccess
  `create_compute_kernel`). Add a `third_party_gpu_capabilities` slot on the
  FullAccess vtable returning a `#[repr(C)]` caps struct; rework `vulkan-jpeg`
  to be engine-free (a plugin-zone shared lib).
- Migrate jpeg, then network / vadr-vision / mavlink.
- Add the `xtask check-boundaries` rule (no `libs/` dep from `plugin/` or
  from a cdylib package) — mirror the existing subprocess-cdylib check.
- Mechanical follow-up: move plugin-abi / processor-schema / consumer-rhi
  from `libs/` into `plugin/` and fix the dep paths.

## File:line map (from the codebase research — verify before relying)

- SDK facade (pulls the engine): `libs/streamlib-sdk/src/lib.rs:79-170`
  (`pub use streamlib_engine::core::*`).
- Dual-mode router: `core/plugin/host_services/mod.rs:314-316`
  (`host_callbacks().is_some()`), `:567` (cdylib installs its copy),
  `:306-308` (host never does).
- `RuntimeContextFullAccess`: `core/context/runtime_context.rs:621`,
  cdylib scope at `:795` (`with_cdylib_scope` / ScopeToken).
- `GpuContextFullAccess`: `core/context/gpu_context.rs` — ScopeToken arm of
  `create_compute_kernel` at `:4479-4505`; **the leak**
  `host_vulkan_device_arc()` at `:4302` (Arc::from_raw `:4330`).
- Registration glue: `host_services/mod.rs:592-625`
  (`register`→`register_via_callback`); `core/plugin/processor_vtable.rs`
  (`vtable_for`, `ProcessorVTable` thunks; `VTABLES` static `:42`).
- Macro: `streamlib-plugin-abi/src/lib.rs:606-645` (`export_plugin!`,
  references `::streamlib::sdk::plugin::install_host_services` `:628`,
  `helper.register::<$processor>()` `:634`). Emitted paths:
  `streamlib-macros/src/codegen.rs:29-33,47-50,104,133,163,402-408,442,466,507-556`.
- `#[repr(C)]{handle,vtable}` handle types: `iceoryx2/output.rs:194-208`
  (`OutputWriter`), `iceoryx2/input.rs` (`InputMailboxes`).
- Engine-free crates (already): `streamlib-processor-schema` (SchemaIdent,
  PortSchemaSpec, descriptors), `streamlib-consumer-rhi` (the template).
- Duplicated globals (what the 2nd engine copy fights over): `PUBSUB`
  `core/pubsub/bus.rs:42`; EscalateGate `core/context/escalate_scope_registry.rs:51`;
  panic hook `core/logging/init.rs:300`; signal handlers `core/signals.rs`;
  `VULKAN_DEVICE_FOR_IMPORT` `vulkan/rhi/vulkan_buffer.rs:21`.
- jpeg clean GPU path: `vulkan-jpeg/kernel.rs:107-109`. jpeg caps gap:
  `vulkan-jpeg/src/simple_decoder.rs:217`.

## How to run the racer-pilot crash-vanishes proof

```bash
cd ~/Repositories/tatolab/drone-racer/racer/runner
source ~/Repositories/tatolab/streamlib/scripts/gitea/registry-token.local.sh
cargo build                       # rebuilds racer-runner + the path-built racer-pilot
RUST_LOG=warn timeout --kill-after=5 60 ./target/debug/racer-runner 2>&1 \
  | grep -aiE "Initialized \(GPU|Setup completed|dumped core|SIGSEGV"
echo "exit: $?"     # 139 = still crashing; 124 (timeout) + no core = SURVIVED
```
No UDP source is needed — the crash is in setup, before any data flows.
Run it ~3–5×; the crash is intermittent at full speed (any instrumentation
hides it, so do NOT wrap it in gdb/strace/validation to "confirm" — those
dodge the race). The current (pre-fix) behavior is an intermittent exit 139.

## Environment / where to run things

- **Primary repo (the build):** `~/Repositories/tatolab/streamlib`. The
  plugin-SDK crates, the engine, the workspace all live here. Start the
  fresh session here.
- **Drone-racer (the proof + racer-pilot migration):**
  `~/Repositories/tatolab/drone-racer`. You WILL edit
  `racer/packages/racer-pilot/*` and run `racer/runner`. **`/add-dir
  ~/Repositories/tatolab/drone-racer`** at session start so Read/Edit/Write
  reach it.
- **Registry token:** every drone-racer build/run needs
  `source ~/Repositories/tatolab/streamlib/scripts/gitea/registry-token.local.sh`
  first (sets `STREAMLIB_REGISTRY_URL` + `STREAMLIB_REGISTRY_TOKEN`).
- **Branch:** stay on `fix/vulkan-jpeg-cdylib-device-panic`. Commit
  locally; do NOT push without Jonathan's OK (PR #1205 tracks this branch
  with the separate device_wait_idle + cache fixes — pushing would fold the
  plugin-SDK into that PR; decide PR-split at the end).
- **GPU box:** NVIDIA RTX 3090, driver 595.71.05 (Open Kernel Module).
  vivid camera at `/dev/video0`.

## Leftovers in the tree (decide / clean up)
- `libs/vulkan-jpeg/tests/cuda_vulkan_repro.rs` — diagnostic test from the
  investigation (proves CUDA-load + Vulkan-kernel in a simple process is
  clean; locks the spawned-thread-clean refutation). Keep as a regression
  doc or remove — your call. NOT part of the plugin-SDK.
- `STREAMLIB_JPEG_BACKEND` env override on the jpeg decoder (committed in
  PR #1205) — a useful operator knob; unrelated to this work.
