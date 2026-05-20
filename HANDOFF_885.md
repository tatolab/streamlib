# Handoff — Issue #885 (Phase B plugin ABI: RuntimeContext callback table)

**Branch:** `feat/runtime-context-callback-table-885`
**Issue:** https://github.com/tatolab/streamlib/issues/885
**Status:** Engine + engine tests compile cleanly. Package migrations + shim restructure + host vtable wiring remain.

This file is intentionally checked in only on the WIP branch. Delete it before merge.

---

## Where the design lives

The locked design is in the **issue #885 body** under "Locked design — 2026-05-20 (post four-agent research)". Read that first — it captures the architectural decisions (no shared tokio crossing, plugins own their own tokio, sync lifecycle, no SDK-provided runtime helper, host's tokio invisible to plugins, two completely isolated async worlds).

The audit table in the issue body still references "promote tokio to opaque + vtable" — that row is struck through (search for "Superseded 2026-05-20") and the locked design supersedes it.

---

## What this branch already lands

1. **New ABI types in `libs/streamlib-plugin-abi/src/lib.rs`:**
   - `RuntimeContextVTable` (owned-return primitives + opaque-handle accessors for GPU/audio_clock/runtime_ops)
   - `AudioClockVTable` (sample_rate, buffer_size, on_tick with extern "C" trampoline)
   - `RuntimeOpsVTable` (add_processor / remove_processor / connect / disconnect / to_json — all submit-with-completion-callback)
   - `AudioTickContextRepr` (`#[repr(C)]` mirror of `AudioTickContext`)
   - `RuntimeOpCompletionCallback` typedef
   - Layout version constants: `RUNTIME_CONTEXT_VTABLE_LAYOUT_VERSION`, `AUDIO_CLOCK_VTABLE_LAYOUT_VERSION`, `RUNTIME_OPS_VTABLE_LAYOUT_VERSION`
   - `HOST_SERVICES_LAYOUT_VERSION` bumped 2 → 3
   - `STREAMLIB_ABI_VERSION` bumped 3 → 4
   - `HostServices` extended with `runtime_context_vtable`, `audio_clock_vtable`, `runtime_ops_vtable` static pointer fields
   - The "tokio handle is shared-type crossing by design" comment block is replaced with a v3 note explaining the tokio elimination.

2. **Lifecycle methods converted to sync** at the trait surface:
   - `ManualProcessor`, `ContinuousProcessor`, `ReactiveProcessor` — `setup` / `teardown` / `on_pause` / `on_resume` are now `fn(...) -> Result<()>` (no async, no `impl Future`).
   - `GeneratedProcessor` (internal) — `__generated_setup` etc. are sync.
   - `DynGeneratedProcessor` (object-safe wrapper) — sync; no more `BoxFuture` return type.
   - `#[processor]` macro emission in `libs/streamlib-macros/src/codegen.rs` — emits sync `__generated_*` methods.

3. **Phase A wrapper updated** at `libs/streamlib-engine/src/core/plugin/processor_vtable.rs`:
   - `setup` / `teardown` / `on_pause` / `on_resume` wrappers no longer wrap in `ctx.tokio_handle().block_on(...)`.
   - They call the sync `__generated_*` methods directly.

4. **Engine-internal consumers updated:**
   - `processor_instance_factory.rs` `LegacyDyn` paths no longer block_on.
   - `spawn_deno_subprocess_op.rs` `DenoSubprocessHostProcessor` — async lifecycle methods converted to sync (bodies wrapped in `(|| -> Result<()> { ... })()` to preserve `?` propagation).
   - `spawn_python_native_subprocess_op.rs` `PythonNativeSubprocessHostProcessor` — same.
   - `host_services.rs` — `HostServices` initializer populates the 3 new fields with `std::ptr::null()` + a TODO marker pointing at the follow-up issues. Plugins compiled against v3 see null vtable pointers; they must null-check before dispatching (see "shim restructure" below).
   - `test_support.rs` engine-internal mock processors — sync lifecycle.
   - `tests/attribute_macro_test.rs` — sync lifecycle.

5. **Status:** `cargo check -p streamlib-engine` and `cargo check --all-targets -p streamlib-engine` both pass.

---

## What's left — three buckets

### Bucket A: Package migrations (the biggest chunk)

32 files across `packages/` and `libs/test-fixtures/` need their lifecycle method signatures converted from `impl Future<Output = Result<()>> + Send` to plain `Result<()>`, and their `Box::pin(async move { ... })` bodies converted to either a closure-call `(|| -> Result<()> { ... })()` (to keep `?` propagation) or just the body inlined.

**Files (from `cargo check --workspace 2>&1 | grep "incompatible type for trait" | grep -oE '[a-zA-Z0-9_-]+/src/[a-zA-Z0-9_/-]+\.rs'`):**

```
api-server/src/processor.rs
audio/src/audio_channel_converter.rs
audio/src/audio_mixer.rs
audio/src/audio_resampler.rs
audio/src/buffer_rechunker.rs
audio/src/chord_generator.rs
audio/src/linux/audio_capture.rs
audio/src/linux/audio_output.rs
camera/src/linux/camera.rs
debug-utilities/src/bgra_file_source.rs
debug-utilities/src/jpeg_bytes_source.rs
debug-utilities/src/video_frame_counter.rs
display/src/linux/display.rs
h264/src/linux/decoder.rs
h264/src/linux/encoder.rs
h265/src/linux/decoder.rs
h265/src/linux/encoder.rs
jpeg/src/linux/decoder.rs
mavlink/src/mavlink_decoder.rs
mavlink/src/mavlink_encoder.rs
moq/src/moq_publish_track.rs
moq/src/moq_subscribe_track.rs
mp4/src/linux/mp4_writer.rs
network/src/udp_sink.rs
network/src/udp_source.rs
opus/src/opus_decoder.rs
opus/src/opus_encoder.rs
plugin/src/lib.rs
test-fixtures/src/test_configured_processor.rs
vadr-vision/src/depayloader.rs
webrtc/src/webrtc_whep.rs
webrtc/src/webrtc_whip.rs
```

**These split into two flavors:**

1. **Trivial sync conversion** — packages whose `async fn setup/teardown/on_pause/on_resume` bodies just do sync work wrapped in `std::future::ready(...)` or do a single `.await` that translates trivially. ~25 files. Mechanical work.

2. **Own-runtime migration** — packages that actually use `ctx.tokio_handle()` to spawn or block_on tokio work. These need to:
   - Add a `Option<tokio::runtime::Runtime>` field on the processor struct (keeps the runtime alive)
   - Add a `Option<tokio::runtime::Handle>` field (the handle the rest of the code uses)
   - In `setup` (now sync), construct the runtime: `tokio::runtime::Builder::new_multi_thread().enable_all().build()?` (or `new_current_thread` for lighter footprint), stash the Runtime, clone the Handle, stash it.
   - In `teardown`, drop the runtime (`self.runtime.take();`).
   - Replace every `ctx.tokio_handle()` call with `self.tokio_handle.as_ref().unwrap()` or similar.

   The 5 packages using `ctx.tokio_handle()` today (verified via `grep -rln "ctx\.tokio_handle()" packages/`):
   - `api-server` — has `block_on` for `tokio::net::TcpListener::bind` in `start()` plus `spawn` for `axum::serve`
   - `webrtc/webrtc_whip.rs` + `webrtc/webrtc_whep.rs`
   - `network/udp_source.rs` + `network/udp_sink.rs`
   - `moq/moq_subscribe_track.rs` (only one of the moq files actually calls `ctx.tokio_handle()`; the others have separate sync conversion work)

**Reference pattern for the "own-runtime" migration:**

```rust
// Before:
pub struct ApiServerProcessor {
    handles: Option<StashedHandles>,
    // ...
}
struct StashedHandles {
    runtime: Arc<dyn RuntimeOperations>,
    tokio_handle: tokio::runtime::Handle,  // ← this is the host's handle (eliminated)
    runtime_id: String,
}

impl ManualProcessor for ApiServerProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>)
        -> impl Future<Output = Result<()>> + Send {
        self.handles = Some(StashedHandles {
            runtime: ctx.runtime(),
            tokio_handle: ctx.tokio_handle().clone(),
            runtime_id: ctx.runtime_id().to_string(),
        });
        std::future::ready(Ok(()))
    }
    // ...
}

// After:
pub struct ApiServerProcessor {
    handles: Option<StashedHandles>,
    runtime: Option<tokio::runtime::Runtime>,  // ← own runtime, owned by this processor
    // ...
}
struct StashedHandles {
    runtime_ops: Arc<dyn RuntimeOperations>,
    tokio_handle: tokio::runtime::Handle,  // ← now the cdylib's own runtime's handle
    runtime_id: String,
}

impl ManualProcessor for ApiServerProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| Error::Runtime(format!("tokio runtime build: {e}")))?;
        let tokio_handle = runtime.handle().clone();
        self.runtime = Some(runtime);
        self.handles = Some(StashedHandles {
            runtime_ops: ctx.runtime(),
            tokio_handle,
            runtime_id: ctx.runtime_id().to_string(),
        });
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // Drop runtime to clean up background threads.
        self.runtime.take();
        self.handles.take();
        Ok(())
    }
    // ...
}
```

**Critical detail:** existing code does `tokio_handle.block_on(async { tokio::net::TcpListener::bind(...).await })`. With the cdylib's own runtime, this works because the cdylib's tokio is statically linked into the cdylib's address space and its TLS slots are set when block_on enters the runtime. axum, hyper, quinn etc. continue to work because they live inside the same cdylib and see the same tokio TLS.

### Bucket B: Shim restructure + host vtable wiring (the real architectural payoff)

The current `RuntimeContextFullAccess` / `RuntimeContextLimitedAccess` shims in `libs/streamlib-engine/src/core/context/runtime_context.rs` still hold a `base: &'a RuntimeContext` field and dispatch methods via `self.base.foo()`. That's still a cross-DSO struct-layout-shared crossing. To finish Phase B's architectural promise:

1. Change the shim struct shape to `{ handle: *const c_void, vtable: *const RuntimeContextVTable, gpu_full: GpuContextFullAccess, gpu_limited: GpuContextLimitedAccess, _phantom: PhantomData<&'a ()> }`.
2. Each accessor method (`runtime_id()`, `processor_id()`, `is_paused()`, `should_process()`) forwards through the vtable. (Note: signatures should return owned values — `String` instead of `&RuntimeUniqueId` — since the audit found all current callers do `.to_string()` already.)
3. Drop the dead accessors: `tokio_handle()`, `time()`, `platform()`, `surface_socket_path()`, `iceoryx2_node()`. The host-internal compiler ops that currently use the last three (`spawn_python_native_subprocess_op.rs:141`, `spawn_deno_subprocess_op.rs:158`, `open_iceoryx2_service_op.rs`) need to reach the underlying `RuntimeContext` through a different crate-internal path. Suggest adding a `pub(crate) fn base(&self) -> &RuntimeContext` to the shim that's only callable from inside the engine, OR change those compiler ops to take `&RuntimeContext` directly instead of `&RuntimeContextFullAccess<'_>`.
4. Construct the shim from `(host_ctx_ref, &HOST_RUNTIME_CONTEXT_VTABLE)` in `RuntimeContextFullAccess::new` etc. The shim shape is uniform — host and cdylib both use the same construction.
5. Implement the host-side static `HOST_RUNTIME_CONTEXT_VTABLE` in `host_services.rs`. Each callback casts `handle: *const c_void` back to `&RuntimeContext` and calls the original accessor. Populate `HostServices::runtime_context_vtable` with `&HOST_RUNTIME_CONTEXT_VTABLE` instead of `std::ptr::null()`.
6. The cdylib-side shim still exposes `gpu_full_access() -> &GpuContextFullAccess` and `gpu_limited_access() -> &GpuContextLimitedAccess` returning the Phase-A-shape GpuContext types (Phase C / #886 wires their internals through the new opaque-handle pattern).

`audio_clock_handle` and `runtime_ops_handle` getters return opaque pointers. The cdylib-side wrapping (AudioClockShim, RuntimeOpsShim) is Bucket C work.

### Bucket C: AudioClockVTable + RuntimeOpsVTable host implementations + cdylib-side shims

Two follow-up issues — file as blockers on the shim restructure landing.

**AudioClock FFI (one consumer: `packages/audio/chord_generator.rs`):**
- Implement static `HOST_AUDIO_CLOCK_VTABLE` in `host_services.rs` with `sample_rate`, `buffer_size`, `on_tick` callbacks delegating to the host's `SharedAudioClock`.
- The `on_tick` callback takes an extern "C" fn + userdata + drop_userdata. Wrap them in a Send+Sync struct and pass to `clock.on_tick(Box::new(move |ctx| { ... unsafe { (callback)(userdata, repr) } ... }))`.
- Populate `HostServices::audio_clock_vtable` with `&HOST_AUDIO_CLOCK_VTABLE`.
- Cdylib-side: add `AudioClockShim` in the SDK that wraps `(handle, vtable)` and exposes Rust-flavored `sample_rate()` / `buffer_size()` / `on_tick(Box<dyn Fn(...)>)`. The `on_tick` wraps the user closure in extern "C" trampolines.

**RuntimeOps FFI (one consumer: `packages/api-server/handlers.rs`):**
- Implement static `HOST_RUNTIME_OPS_VTABLE` in `host_services.rs`. Each op spawns on the host's tokio (held as a `OnceLock<tokio::runtime::Handle>` initialized at engine startup), decodes the msgpack request, calls the real `RuntimeOperations::*_async`, encodes the response (msgpack), fires the completion callback.
- Populate `HostServices::runtime_ops_vtable` with `&HOST_RUNTIME_OPS_VTABLE`.
- Cdylib-side: add `RuntimeOpsShim` in the SDK. Each method on the shim builds a `tokio::sync::oneshot::channel()`, boxes the `Sender` as `user_data`, calls the vtable's `add_processor` / etc. with a completion callback that takes the boxed Sender and sends the result through it. The shim returns a `BoxFuture` that awaits the receiver. `RuntimeOpsShim` impls `RuntimeOperations` (the existing trait) so `state.runtime.add_processor_async(spec).await` keeps working.

---

## Strategy for the next session

The team-based parallelization the user originally suggested is still the right approach for Bucket A (32-file package migration). Now that the engine compiles, teams can be spawned safely against the current state of `main` + this branch.

**Recommended order:**

1. **Land Bucket A as a first follow-up PR (or continue on this branch)** — the package migrations. Spawn 5-6 teams:
   - `api-server-migrator` — api-server (own runtime + sync lifecycle)
   - `webrtc-migrator` — webrtc whip + whep (own runtime + sync lifecycle)
   - `network-migrator` — network udp_source + udp_sink (own runtime + sync lifecycle)
   - `moq-migrator` — moq subscribe_track + others (own runtime + sync lifecycle where applicable)
   - `sync-sweeper` — all other packages (~25 trivial sync conversions: audio, camera, display, codecs, mp4, debug-utilities, mavlink, opus, plugin, test-fixtures, vadr-vision)
   - `integration-tester` — write a dlopen test that exercises sync lifecycle + a processor that owns its own tokio + binds a `tokio::net::TcpListener`

   Each team gets a focused brief based on the patterns in this doc.

2. **Bucket B as second follow-up PR** — shim restructure + host RuntimeContextVTable wiring. Smaller and self-contained.

3. **Bucket C as two parallel follow-up PRs** — AudioClock FFI + RuntimeOps FFI. Independent and small enough.

## Next-session prompt (for the user to give to a fresh Claude Code agent)

```
I'm continuing work on issue #885 (Phase B plugin ABI). The previous session
landed the engine foundation; read HANDOFF_885.md on this branch
(feat/runtime-context-callback-table-885) for full context — that file has
the locked design pointer, what's done, and what's left across three buckets.

We're on Bucket A: per-package migrations. `cargo check --workspace` shows
32 files in `packages/` and `libs/test-fixtures/` with the lifecycle trait
signature mismatch (E0053). Engine itself compiles cleanly.

The user wants to use TeamCreate to parallelize this across multiple agents.
Read HANDOFF_885.md's "Strategy for the next session" section for the
proposed team breakdown (api-server, webrtc, network, moq, sync-sweeper,
integration-tester). Each team brief should reference the "Reference
pattern for the own-runtime migration" in the handoff doc.

Before spawning teams, briefly verify the engine still compiles on this
branch (`cargo check --all-targets -p streamlib-engine`), then proceed
with team spawning.

Do NOT change the architectural decisions in the locked design (issue body)
without explicit user sign-off. The design has converged after extensive
research; the user has been clear about the trade-offs.
```

---

## Files changed in this session (for the PR description)

```
$ git status --short
 M HANDOFF_885.md  (new, this file — delete before merge)
 M libs/streamlib-engine/src/core/compiler/compiler_ops/spawn_deno_subprocess_op.rs
 M libs/streamlib-engine/src/core/compiler/compiler_ops/spawn_python_native_subprocess_op.rs
 M libs/streamlib-engine/src/core/plugin/host_services.rs
 M libs/streamlib-engine/src/core/plugin/processor_vtable.rs
 M libs/streamlib-engine/src/core/processors/__generated_private/generated_processor.rs
 M libs/streamlib-engine/src/core/processors/__generated_private/generated_processor_impl.rs
 M libs/streamlib-engine/src/core/processors/processor_instance_factory.rs
 M libs/streamlib-engine/src/core/processors/traits/continuous.rs
 M libs/streamlib-engine/src/core/processors/traits/manual.rs
 M libs/streamlib-engine/src/core/processors/traits/reactive.rs
 M libs/streamlib-engine/src/core/test_support.rs
 M libs/streamlib-engine/tests/attribute_macro_test.rs
 M libs/streamlib-macros/src/codegen.rs
 M libs/streamlib-plugin-abi/src/lib.rs
```
