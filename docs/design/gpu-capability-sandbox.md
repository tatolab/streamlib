# Design: `GpuContextSandbox` + `GpuContextFullAccess`

Status: draft — gating the #319 umbrella (tasks #321–#326).

Umbrella: [#319 GPU capability-based access](https://github.com/tatolab/streamlib/issues/319).
Parent plan file: [`plan/319-gpu-capability-based-access.md`](../../plan/319-gpu-capability-based-access.md).

## Problem

`GpuContext` today is a single type handed to every processor lifecycle
method. `setup()` and `process()` both see the same API, so nothing
prevents a `process()` body from calling `acquire_output_texture()`,
creating a new video session, or triggering a pool-growth allocation on
the hot path. Every past regression in this area — NVIDIA
`DEVICE_LOST` during concurrent resource creation (#304), camera MMAP
pool growth during the first frame (#288), H.265 decoder DPB allocation
racing with display swapchain setup (#304 again) — resolves to the same
pattern: resource creation happened where it wasn't supposed to.

The runtime half of that invariant already shipped in #304: a
`processor_setup_lock` mutex held across `setup()` + `wait_device_idle()`
in `spawn_processor_op.rs` Phase 4 serializes resource creation during
processor spawn. That handles concurrency, but it does nothing to keep
`process()` from allocating anyway. The error still compiles.

This design moves the invariant into the type system. Two capability
wrappers around `GpuContext`:

- **`GpuContextSandbox`** — handed to `process()`. Cheap, pool-backed,
  pre-reserved operations only. Heavy-allocation methods are not in
  scope; calling one is a compile error.
- **`GpuContextFullAccess`** — handed to `setup()` and inside
  `sandbox.escalate(|full| …)` closures. Full GPU API.

Escalation from a running processor (e.g. mid-stream reconfigure)
reuses the existing `processor_setup_lock`. The lock is no longer a
peer of `setup()` — it becomes a private implementation detail of
`escalate()`. "Setup is the compiler pre-escalating on your behalf."

## Goals / non-goals

Goals:

- Make the wrong program uncompileable for in-tree Rust processors.
- Keep the runtime guarantee (serialized resource creation, wait-idle
  on exit) that #304 shipped.
- Provide a migration path for Python/Deno subprocess processors so
  they get the same guarantee over IPC.
- Preserve current performance on the hot path: no extra locks, no
  extra allocations, no extra layers of indirection beyond the
  newtype.

Non-goals:

- Redesigning `GpuContext` itself. The two capability types are thin
  newtype wrappers on the existing `GpuContext`; internals stay put.
- Auditing RHI internals. The RHI boundary rule (CLAUDE.md) is
  unchanged — `GpuContext` remains the one type processors see.
- Changing execution topology. Each processor still runs on its own
  thread; `process()` is still serial per processor.

---

## 1. API split table

Every public / `pub(crate)` method on `GpuContext` classified. Call
sites are representative, not exhaustive.

Legend:

- **S** — Sandbox. Cheap, never allocates new GPU memory (pool hit,
  sampler, map write, read-only query).
- **F** — FullAccess. Creates Vulkan/Metal objects or allocates GPU
  memory.
- **Split** — has a fast path (pool hit / cache hit → Sandbox) and a
  slow path (pool miss, broker XPC, growth → FullAccess). Must be
  decomposed into a Sandbox method that never allocates and an
  escalated slow path.

### Lifecycle / constructors

| Method | Cap | Notes |
|---|---|---|
| `new()` | F | Constructor. Only called from runtime init. |
| `with_texture_pool_config()` | F | Constructor. |
| `init_for_platform()` / `init_for_platform_sync()` | F | One-time runtime startup. |

These never appear in processor code. Classified for completeness.

### Device / queue accessors

| Method | Cap | Notes |
|---|---|---|
| `device()` | F | **Escape hatch.** Returns the RHI device, which can do anything. Must NOT be on Sandbox. Available only on FullAccess. |
| `command_queue()` | S | Returns a shared queue handle. Used to submit recorded command buffers in `process()`. |
| `create_command_buffer()` | S | Allocates a CPU-side command buffer (stack-sized object); does not allocate GPU memory. |
| `wait_device_idle()` | F | A `vkDeviceWaitIdle` is a device-wide barrier — a running processor has no business calling it. Available only on FullAccess. |

Ambiguous case: `command_queue()` is Sandbox today, but a hostile
caller can use the queue to submit work that creates resources
indirectly (e.g. transient image views). The runtime constraint is
that submitted work does not allocate new `VkDeviceMemory`. The type
boundary accepts this as "allowed in process()"; the RHI still owns
whatever submission validates.

### Pixel buffer pool

| Method | Cap | Notes |
|---|---|---|
| `acquire_pixel_buffer(w, h, format)` | Split | Pool hit (ring slot available) = Sandbox. Pool miss or first call for a new (w,h,format) = FullAccess (pre-allocates `POOL_PRE_ALLOCATE_COUNT=4` buffers; may grow to `POOL_MAX_BUFFER_COUNT=64`). See §2 for the split shape. |
| `get_pixel_buffer(id)` | Split | Local cache hit = Sandbox. Broker (`SurfaceStore`) miss = FullAccess (XPC call; may trigger GPU memory registration). |
| `resolve_videoframe_buffer(frame)` | Split | Thin wrapper around `get_pixel_buffer`. |

### Textures

| Method | Cap | Notes |
|---|---|---|
| `acquire_output_texture(w, h, format)` | F | Always `device.create_texture()` → `vkCreateImage` + VMA alloc. No pool. Sandbox must never call this. |
| `acquire_texture(desc)` | Split | Pool hit (atomic flag flip) = Sandbox. Pool miss + grow (`allocate_slot` → `create_texture`) = FullAccess. Blocking path (exhaustion policy = `Block`) is Sandbox-allowed (cheap wait, no new allocations). |
| `register_texture(id, texture)` | S | `HashMap` insert under a `Mutex`. No GPU work. |
| `resolve_videoframe_texture(frame)` | Split | Same-process cache hit = Sandbox. Cross-process DMA-BUF import path = FullAccess. |
| `upload_pixel_buffer_as_texture(id, buf, w, h)` (Linux) | F | `create_texture_local()` + GPU copy. Always allocates a new texture. |
| `texture_pool()` | F | Accessor returning `&TexturePool`. `TexturePool` has its own sub-API; see below. Exposing it on Sandbox would leak `prewarm` and similar. Available only on FullAccess. |

### Blitter (GPU buffer copies)

| Method | Cap | Notes |
|---|---|---|
| `blit_copy(src, dest)` | S | Uses cached blitter; GPU copy, no allocation. |
| `blit_copy_iosurface(src, dest, w, h)` (macOS) | S | Platform blit; no allocation. `unsafe` — caller responsible for IOSurface lifetime. |
| `clear_blitter_cache()` | S | Flushes a cache; no GPU allocation. Arguably `setup()`-only by convention, but not enforced. |

### Timeline semaphore

| Method | Cap | Notes |
|---|---|---|
| `set_camera_timeline_semaphore(raw)` | S | Atomic store. |
| `camera_timeline_semaphore()` | S | Atomic load. |

### Surface store (cross-process, primarily macOS)

| Method | Cap | Notes |
|---|---|---|
| `set_surface_store()` / `clear_surface_store()` (crate-private) | F | Internal; only called from runtime start/stop. |
| `surface_store()` | S | Option accessor. |
| `check_in_surface(buf)` (macOS) | F | XPC call + broker registration. |
| `check_out_surface(id)` (macOS) | Split | Cache hit = Sandbox; first XPC = FullAccess. |

### Platform escape hatches

| Method | Cap | Notes |
|---|---|---|
| `metal_device()` (macOS) | F | Raw Metal device. Same class as `device()` — must not be on Sandbox. |
| `create_texture_cache()` (macOS) | F | Allocates `MTLTextureCache`. |

### #304 mutex

| Method | Cap | Notes |
|---|---|---|
| `lock_processor_setup()` | (private) | Becomes `pub(crate)`; `escalate()` is the only caller. Removed from the public API on both capability types. |

### `TexturePool` sub-API (reachable via `texture_pool()` → FullAccess-gated)

| Method | Cap | Notes |
|---|---|---|
| `acquire(desc)` | Split | Same shape as `GpuContext::acquire_texture`. |
| `prewarm(desc, count)` | F | Explicit pre-allocation. Only called from setup. |
| `stats()` | S | Read-only snapshot. |
| `clear_unused()` | S | Evicts unused slots; no allocation. |

`PooledTextureHandle` accessors (`texture`, `width`, `height`, `format`,
`slot_id`, `iosurface_id`, `native_handle`, `metal_texture`) are all
Sandbox — field accessors on an already-acquired handle.

### Classification confidence

Clear-cut: the whole table except for three cases that need user
sign-off before #324 lands:

1. **`command_queue()` / `create_command_buffer()`** — Sandbox today,
   but a submitted command list *can* reference newly-imported images.
   If we later decide submissions must go through an escalation
   boundary for tracing/telemetry reasons, these move to Split. Not
   today.
2. **`blit_copy` family** — Sandbox today; one open question is
   whether the blitter's internal `RhiTextureCache` can grow on a
   cold key. If it can, this is a Split and the first call for a new
   key needs to escalate. The alternative is to pre-warm the cache
   in `setup()` for all expected sizes.
3. **`resolve_videoframe_texture` / `check_out_surface`** — the
   first-call DMA-BUF import path is FullAccess but is unusual in
   that the caller doesn't know a priori whether it's the first
   call. See §2 for the "Sandbox-fast-path + transparent escalation"
   proposal.

---

## 2. `escalate()` closure signature

```rust
impl GpuContextSandbox {
    pub fn escalate<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&GpuContextFullAccess) -> Result<T>;
}
```

Design choices and rationale.

### Sync, not async

`process()` is sync today (see `ReactiveProcessor::process` in
`libs/streamlib/src/core/processors/traits/reactive.rs`). `setup()` is
async (returns `impl Future`). `escalate()` needs to be callable from
both.

Making `escalate()` sync is the simpler choice and costs nothing:
every heavy GPU op (`create_texture`, `vmaCreateImage`, session
creation) is already blocking on the RHI side. Wrapping a blocking
call in async buys nothing and makes the sandbox API reach for a
runtime handle.

Setup already has a tokio handle (`runtime_context.tokio_handle()`)
if an async need appears inside the closure; the closure itself is
sync, but can `block_on` if needed. For 99% of callers, the closure
body is pure Vulkan/VMA calls — no async anywhere.

Open question: if we ever need async escalation (e.g. awaiting an
XPC response inside the escalated region), we add an
`escalate_async` variant rather than making the primary form async.

### `FnOnce`, not `FnMut`

Escalation should be rare. A closure that the compiler lets you call
multiple times invites patterns like

```rust
for frame in incoming {
    sandbox.escalate(|full| full.acquire_output_texture(…))?;
}
```

which is exactly the misuse this doc exists to prevent. `FnOnce`
pushes the reusable value out of the closure and makes the rare
nature explicit. A debug-only `escalate`-rate counter (see §4) makes
accidental high-frequency use a loud warning even when the type
system allows it.

### `FullAccess` lifetime

The closure receives `&GpuContextFullAccess` whose lifetime is tied
to the closure's stack frame. The `FullAccess` is constructed
from-scratch inside `escalate()` and dropped when the closure
returns; the user cannot stash it in `self` or return it. This is
the type-system enforcement of "FullAccess is only valid inside an
escalated region."

Implementation note: `FullAccess` is a newtype around
`Arc<GpuContext>` (same `Arc` the sandbox holds). The lifetime trick
works because the `&GpuContextFullAccess` is a borrow of a stack
local inside `escalate`, not of anything reachable from
`GpuContextSandbox`. Leaking a `FullAccess` out requires cloning its
inner `Arc`, which we don't expose — only `&GpuContextFullAccess` is
handed to `f`.

### Error propagation

`Result<T>` returned from the closure flows straight out of
`escalate()`. Errors inside the closure abort the escalation; the
lock is released on drop regardless. `wait_device_idle()` still
fires on exit unless the error was the `wait_device_idle()` itself.

### Internals

```rust
impl GpuContextSandbox {
    pub fn escalate<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&GpuContextFullAccess) -> Result<T>,
    {
        let _guard = self.inner.lock_processor_setup(); // Phase 4's mutex
        let full = GpuContextFullAccess { inner: Arc::clone(&self.inner) };
        let result = f(&full);
        // Drop FullAccess before wait_device_idle so the user can't
        // stash a reference through any side channel.
        drop(full);
        self.inner.wait_device_idle()?;
        result
    }
}
```

This is the only place `lock_processor_setup()` is called. `setup()`
in Phase 4 (`spawn_processor_op.rs:388`) is rewritten to call
`sandbox.escalate(|full| user_setup(full, …))` — it becomes a
straight user of the same primitive.

### Split-method resolution (revisiting §1's three Split groups)

Split methods get two-tier shapes:

- **Sandbox fast-path method** (`sandbox.acquire_pixel_buffer(w, h, f)`)
  returns `Result<(Id, Buf)>` on a pool hit and `Err(Exhausted)` on a
  pool miss. No allocation; callers either handle the error or
  pre-reserve in `setup()`.
- **FullAccess method** (`full.acquire_pixel_buffer(w, h, f)`) has
  the old behavior: pool hit or allocate.
- Transparent escalation (§4): a debug-build helper
  `sandbox.acquire_pixel_buffer_or_escalate(w, h, f)` that wraps
  "try fast, escalate on miss" for mid-run reconfigure. Not used by
  default — explicit pre-reserve is the expected path.

The runtime behavior that ships in #324 is the first two; the
transparent form is optional and can be deferred or cut.

---

## 3. Compiler integration

### Phase 4 rewrite

Today (`libs/streamlib/src/core/compiler/compiler_ops/spawn_processor_op.rs:373–417`):

```rust
let _setup_guard = runtime_ctx_clone.gpu.lock_processor_setup();
// … block_on(guard.__generated_setup(processor_context.clone())) …
runtime_ctx_clone.gpu.wait_device_idle()?;
// _setup_guard drops here
```

After #323:

```rust
runtime_ctx_clone.gpu_sandbox.escalate(|full_access| {
    tokio_handle.block_on(
        guard.__generated_setup(processor_context.with_full_access(full_access))
    )
})?;
```

- `lock_processor_setup()` becomes `pub(crate)` and is only called
  from `GpuContextSandbox::escalate`.
- `wait_device_idle()` is no longer called inline; `escalate()` does
  it on closure exit.
- Phase 4 no longer has any awareness of the mutex. It calls
  `escalate()` and hands the `FullAccess` into the processor's
  generated setup wrapper.

### What happens to the #304 mutex

Unchanged mechanically. `processor_setup_lock` stays a field of
`GpuContext`. It's still a `std::sync::Mutex<()>` held across
`setup() + wait_device_idle`. The only change is that the one place
that *grabs* the lock is inside `escalate()`, and `escalate()` is
now the only way anyone acquires it — `setup()` included.

Effect: "Phase 4's serialization" and "mid-run reconfigure
serialization" become the same primitive. If a running processor
calls `sandbox.escalate(…)` to grow a pool for a resolution change,
it contends for the same mutex that a processor-being-spawned
contends for. That's the correct behavior: the driver constraint
(NVIDIA concurrent resource creation races) doesn't care which one
is doing it.

### Reconfigure

There is no mid-run reconfigure path today (the code search in the
research phase found none). The escalation primitive provides one:
a running `process()` calls `sandbox.escalate(|full| …)` to reshape
its GPU resources without tearing down and respawning. This is a
capability we get for free once #323 lands; exercising it is a
future task (relevant to #310 pipeline-wide resolution
propagation).

---

## 4. `RuntimeContext` changes

Today (`libs/streamlib/src/core/context/runtime_context.rs:12–34`):

```rust
pub struct RuntimeContext {
    pub gpu: GpuContext,
    // … time, runtime_id, processor_id, pause_gate, …
}
```

After #322:

```rust
pub struct RuntimeContext {
    // Sandbox accessor available always; process() uses this.
    gpu_sandbox: GpuContextSandbox,
    // FullAccess is injected only during setup/escalate windows.
    gpu_full: Option<GpuContextFullAccess>,
    // … time, runtime_id, processor_id, pause_gate, …
}

impl RuntimeContext {
    pub fn gpu(&self) -> &GpuContextSandbox { &self.gpu_sandbox }
    pub fn gpu_full(&self) -> Option<&GpuContextFullAccess> { self.gpu_full.as_ref() }
}
```

- `gpu_sandbox` is the always-available accessor.
- `gpu_full` is `None` at construction time. Phase 4 populates it
  inside the `escalate()` closure before handing the context into
  `__generated_setup`. On closure exit, the field is cleared. Same
  mechanism for mid-run `escalate()`.

### `ReactiveProcessor` trait changes

Today (`libs/streamlib/src/core/processors/traits/reactive.rs:14–37`):

```rust
fn setup(&mut self, _ctx: RuntimeContext) -> impl Future<Output = Result<()>> + Send { … }
fn process(&mut self) -> Result<()>;
```

Two options for the new shape. The design doc recommends **Option A**
and flags Option B for user review.

**Option A — hand a typed `SetupCtx` / sandbox through `self.ctx`
stored at construction time** (recommended).

`setup(&mut self, ctx: SetupCtx)` where `SetupCtx` wraps
`RuntimeContext` but exposes `gpu_full()` directly as
`&GpuContextFullAccess` (not Option-wrapped). `process()` stays
`&mut self`; the processor is expected to stash its own
`GpuContextSandbox` (cloned from `ctx.gpu()` in setup) for use in
`process()`. Migration stays mechanical.

Pros: smallest diff to processor bodies. Processors already stash
`gpu: Option<GpuContext>` today (see `H265DecoderProcessor.gpu_context`
in research §3); swapping that for `Option<GpuContextSandbox>` is
mostly a type rename.

Cons: `process()` still accesses GPU via `self.sandbox.…` rather
than `ctx.gpu_sandbox.…`. Not a problem in practice.

**Option B — flip `process()` to take `&mut self, ctx: &ProcessCtx<'_>`**.

`ProcessCtx` exposes only `gpu_sandbox()`. More invasive change to
the trait and every call site. The win is that processors no longer
stash context fields — the sandbox is passed per-call.

Pros: stronger "only touch what's handed to you" story; forces
re-threading of every access.

Cons: every in-tree processor body changes. We'd amortize that
against a genuine benefit: per-call liveness of ctx is useful if
we later want the runtime to hot-swap the sandbox under the
processor (e.g. during reconfigure). That use case is hypothetical
today.

**Recommendation:** go with Option A for #322. Keep Option B as a
future refactor; don't make it a blocker for the capability split.
The user should flag if they want Option B instead before #322
lands.

### Other processor traits

`ManualProcessor` (used by camera / display / screen capture — see
research §3) has its own lifecycle (`start`, `on_frame`, etc.). The
same Option-A-style migration applies: `start(ctx: SetupCtx)` gets
full access; frame callbacks get sandbox only. Details deferred to
#322.

---

## 5. Polyglot mapping — IPC for escalate-on-behalf

### Current state (research §4)

Python and Deno subprocess hosts (`PythonNativeSubprocessHostProcessor`,
`DenoSubprocessHostProcessor` — see
`libs/streamlib/src/core/compiler/compiler_ops/spawn_python_native_subprocess_op.rs`
and `spawn_deno_subprocess_op.rs`) run in the parent's Manual mode
with no input mailboxes / output writer. The subprocess manages its
own iceoryx2 I/O via FFI to a native lib (`libstreamlib_python_native`,
`libstreamlib_deno_native`). Lifecycle commands go over stdin/stdout
as JSON RPC (not JTD). Port wiring info is passed to the subprocess
in the `setup` command.

There is no "allocate GPU resources on behalf of the subprocess"
channel today — the subprocess runs its own iceoryx2 publishers /
subscribers for per-frame data and doesn't touch the parent's
`GpuContext`.

### What #325 needs to add

With Sandbox enforced in Rust, we need the same discipline for
Python and Deno. Subprocess sees only a Sandbox-equivalent surface;
any escalation routes through the host. The host executes the
request inside its own `sandbox.escalate(|full| …)` call, which
serializes on the same mutex as every other escalation in the
process.

### Proposed IPC shape

Control-channel schema (in `libs/streamlib/schemas/`, YAML → JTD →
codegen, matching existing conventions):

```yaml
# com.streamlib.escalate_request@1.0.0.yaml
discriminator: op
mapping:
  acquire_pixel_buffer:
    properties:
      request_id: { type: string }
      width: { type: uint32 }
      height: { type: uint32 }
      format: { enum: [...] }     # PixelFormat enum
  acquire_texture:
    properties:
      request_id: { type: string }
      descriptor: { ref: TexturePoolDescriptor }
  release_handle:
    properties:
      handle_id: { type: string }
```

```yaml
# com.streamlib.escalate_response@1.0.0.yaml
discriminator: result
mapping:
  ok:
    properties:
      request_id: { type: string }
      handle_id: { type: string }
      # Shape-specific metadata (pool ID, DMA-BUF FD index, iceoryx2 topic).
  err:
    properties:
      request_id: { type: string }
      message: { type: string }
```

Transport: extend the existing stdin/stdout JSON RPC channel used
for lifecycle. Each subprocess stays single-threaded on that
channel (same pattern as setup → ready today). The host-side
handler:

```rust
// In SubprocessHostProcessor (Python and Deno):
fn handle_escalate(&self, req: EscalateRequest) -> EscalateResponse {
    self.sandbox.escalate(|full| match req {
        EscalateRequest::AcquirePixelBuffer { width, height, format, .. } => {
            let (id, buf) = full.acquire_pixel_buffer(width, height, format)?;
            // Register the buffer ID so the subprocess can reference it
            // in future per-frame messages. Buffer lives in host pools.
            Ok(EscalateResponse::ok_with_buffer(id, buf))
        }
        // …
    })
}
```

### Lifetime and ownership

Host owns the allocations. Subprocess references by string ID (UUID
or pool ID). Release happens either by explicit `ReleaseHandle`
request or by subprocess teardown (host scans and drops held
handles on subprocess death).

### FFI surface on subprocess side

Python binding (`streamlib-python`): `ctx.escalate(op)` helper that
sends the request over the existing control pipe and blocks on the
response. Deno binding: same shape (`ctx.escalate(op)` returning a
Promise).

### Out of scope for #325

- Bidirectional streaming of escalation requests (one-at-a-time RPC
  is fine for resource creation).
- Direct DMA-BUF FD passing on Linux for cross-process textures —
  handled by the existing `SurfaceStore` on macOS; Linux cross-
  process paths land with the broker work that's downstream of this
  umbrella.

---

## 6. Migration plan

Order of operations across #321–#326.

1. **#321 — introduce newtypes**. `GpuContextSandbox` and
   `GpuContextFullAccess`, each holding `Arc<GpuContextInner>`
   (the existing struct renamed to distinguish the internal from
   the wrappers). Both types implement every current `GpuContext`
   method by delegation. Zero behavior change. Zero call-site
   change. `cargo check` clean, `cargo test` green.
2. **#322 — flip trait signatures**. `RuntimeContext` grows
   `gpu_sandbox` + `gpu_full` fields. `ReactiveProcessor::setup`
   receives a `SetupCtx` with `gpu_full()` access; `process()`
   sees only sandbox (via `self` stashing per Option A). Every
   in-tree processor updated to the new signatures (research §3
   lists the files). API is still full-surface on both types, so
   changes are mechanical renames. E2E roundtrip per
   `docs/testing.md` for h264 + h265 on vivid — no regression.
3. **#323 — implement `escalate()`**. Add
   `GpuContextSandbox::escalate`; rewrite Phase 4 in
   `spawn_processor_op.rs` to call it instead of grabbing
   `lock_processor_setup` directly. Verify via #304's 20× h265 loop
   on `/dev/video2` (zero `DEVICE_LOST`) and a new unit test:
   multiple threads concurrently calling `escalate` see serialized
   closures.
4. **#324 — restrict the Sandbox API surface**. Strip FullAccess
   methods from `GpuContextSandbox` per §1's table. Every
   resulting compile error in `process()` bodies is fixed by one
   of:
   - moving the call to `setup()` and stashing the resource,
   - wrapping the call in `escalate(|full| …)` (for mid-run
     reconfigure paths),
   - rewriting the call to use a Sandbox-fast-path method (for
     Split methods where a pool hit is the common case).
   Full E2E roundtrip on vivid + Cam Link after this task.
5. **#325 — polyglot escalate-on-behalf**. IPC schema, host-side
   `EscalateRequest` handler, Python and Deno client bindings.
   Example subprocess processor demonstrating mid-stream pixel
   buffer acquisition.
6. **#326 — learning doc**. `docs/learnings/gpu-capability-typestate.md`
   capturing the invariant, the NVIDIA driver constraint that
   motivates it, and the rejected alternatives. CLAUDE.md pointer.
   Cites this design doc for depth.

Which tasks are compile-only vs behavior-changing:

- Compile-only: **#321**, **#322**.
- Behavior-changing: **#323** (Phase 4 rewrite; runtime identical
  but path through the mutex changes), **#324** (any process()
  body that used to slow-path through pool growth now errors at
  compile time; the fix either pre-reserves or escalates).
- IPC schema additions: **#325**.
- Docs: **#326**.

---

## 7. Alternatives considered

### A. Runtime phase check only (ship the #304 mutex, stop there)

What it is: the #304 `processor_setup_lock` already serializes
resource creation during `setup()`. We could add a runtime
`Phase::{Setup, Processing}` field to `RuntimeContext` and panic in
debug / log-warn in release if a FullAccess-classified method is
called when `Phase::Processing` is current.

Rejected: the whole point is to shift the failure earlier. A
runtime panic still ships broken binaries. A debug-only check adds
noise in test runs and does nothing in release builds, which is
where the NVIDIA `DEVICE_LOST` bugs manifested. Keeping only the
mutex also doesn't give us a path to polyglot enforcement — Python
processors would need their own runtime phase check, replicated.

### B. Single render thread

What it is: force every GPU op through a single owned-thread
actor; processors send commands and await results.

Rejected: kills per-queue parallelism. The current architecture
lets multiple processors submit to the same queue concurrently,
which is correct Vulkan and meaningful for throughput (H.264
encoder + display render fighting for the same queue today). A
single render thread makes this impossible and is far more
invasive than the capability split.

### C. Builder pattern on `GpuContext`

What it is: every method on `GpuContext` returns an intermediate
`GpuContextBuilder` that you .build() into a handle. Separate
setup/process by which builders are exposed.

Rejected: doesn't compose. The API surface balloons (every method
becomes a named builder), IDE autocomplete suffers, and the
enforcement story is the same newtype split wrapped in more
ceremony. Bare newtypes with method subsets are clearer.

### D. Phantom-typed `GpuContext<Phase>`

What it is: `GpuContext<Setup>` vs `GpuContext<Process>` via a
phantom type parameter; methods constrained with trait bounds on
the phase tag.

Rejected: the ergonomics are worse than two named newtypes for
negligible marginal benefit. Every method signature grows a
`where Phase: AllowsX` bound; IDE tooltips show trait bounds
instead of plain types. The user has explicitly called out (see
`MEMORY.md → feedback_claude_md_trimmed.md`) that naming clarity
matters more than academic elegance.

### E. Do nothing; rely on review discipline

What it is: the #304 mutex is shipping; just don't write
`process()` bodies that allocate.

Rejected: already tried. Every bug this document cites is a case
where a human didn't follow the discipline. The compile-time
enforcement exists exactly to stop that class of error at the
earliest possible point.

---

## 8. Open questions

1. **Option A vs Option B for `process()` signature (§4).** Option A
   keeps processors stashing a sandbox field, Option B threads a
   `ProcessCtx` through every `process()` call. A says "smaller diff
   now"; B says "cleaner threading if we ever hot-swap context." The
   design recommends A unless the user has a reason to prefer B.
2. **`blit_copy` classification (§1).** If the blitter's texture
   cache can grow on a cold key, `blit_copy` is Split, not Sandbox.
   Research §11 (blitter internals) wasn't exhaustive enough to
   resolve this. Needs a short read-through of the Metal + Vulkan
   blitter implementations before #324 ships. Either answer is
   workable: if Split, callers pre-warm the cache in `setup()`.
3. **Transparent escalation helpers (§2).** The
   `acquire_pixel_buffer_or_escalate` form is convenient for
   reconfigure but easy to misuse. The design punts on shipping this
   in #324; the primary API is explicit escalate or pre-reserve.
   User may want this as a debug-only crutch for Option B (per-call
   `ProcessCtx`) or may want it cut entirely. Not a blocker.
4. **Polyglot response shape for DMA-BUF FDs (§5).** The Linux
   cross-process path will eventually need to ship DMA-BUF file
   descriptors across the stdin/stdout channel, which doesn't
   natively pass FDs. Options: Unix domain socket side-channel,
   SurfaceStore-style broker. Out of scope for #325's initial shape
   (pool IDs are enough for the first pass) but needs a followup.
5. **`command_queue()` classification (§1).** Left on Sandbox for
   now; may need to move if we decide command submissions must go
   through a telemetry/tracing boundary. Not urgent.
6. **Escalate-rate budget.** Proposed debug-only counter that warns
   if a single processor calls `escalate` more than N times per
   second. N = ? (10/sec feels right but no data. Possibly unneeded
   if the `FnOnce` signature already signals "rare use".)

---

## 9. Verification (gating #321)

- Every current `GpuContext` method appears in §1's table.
- User signs off on classifications, especially the three flagged
  ambiguous cases.
- User picks Option A vs Option B for §4's `process()` signature.
- User decides whether §8's open questions block further work or
  are followup-able.

Once those four are resolved, #321 (the newtype introduction) can
start on its own branch.
