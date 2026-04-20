# Research: `blit_copy` cache-growth classification

Closes open question §8.Q2 of
[`docs/design/gpu-capability-sandbox.md`](../design/gpu-capability-sandbox.md),
which left the blitter row of the §1 API split table conditional on
this audit.

Gating: unblocks [#324 Restrict `GpuContextLimitedAccess` API surface to
safe ops](../../plan/324-restrict-sandbox-surface.md).

Parent umbrella: [#319 GPU capability-based access](../../plan/319-gpu-capability-based-access.md).

---

## Question

Can `RhiBlitter::blit_copy` (and its `blit_copy_iosurface_raw` sibling)
grow internal state on a cold key during `process()`? If yes, it is a
**Split** method: callers must pre-warm in `setup()` or wrap the call
in `escalate()`. If no, it stays **Sandbox**.

## Answer

**Split on Metal. Sandbox-safe on Vulkan.** The trait is overall
**Split**, because the contract must be conservative enough to cover
every backend. In practice the growth is bounded, non-`VkDeviceMemory`,
and off the NVIDIA concurrent-resource-creation hazard path that #304
exists to fence, so the cold-key cost is low — but the type boundary
still has to account for it.

Decision for the §1 table: `blit_copy` and `blit_copy_iosurface` move
from **S** to **Split**. See [§ Recommendation for #324](#recommendation-for-324)
below for the shape callers take.

## Evidence

### Trait

```rust
// libs/streamlib/src/core/rhi/blitter.rs
pub trait RhiBlitter: Send + Sync {
    fn blit_copy(&self, src: &RhiPixelBuffer, dest: &RhiPixelBuffer) -> Result<()>;
    unsafe fn blit_copy_iosurface_raw(
        &self,
        src: *const std::ffi::c_void,
        dest: &RhiPixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()>;
    fn clear_cache(&self);
}
```

### Metal backend — cache grows on cold key

`libs/streamlib/src/metal/rhi/blitter.rs`.

`MetalBlitter` owns `texture_cache: Mutex<HashMap<u32, SendableTexture>>`
keyed by **destination IOSurface ID**. Every `blit_copy` /
`blit_copy_iosurface_raw` call runs `get_or_create_texture(dest)`:

- **Cache hit** — returns the cached `MTLTexture` clone. No allocation.
- **Cache miss (cold key)** — calls
  `create_metal_texture_from_iosurface(device, iosurface, 0)`, inserts
  the new `MTLTexture` into the HashMap, returns it.

This is a real cold-key allocation in the `process()` hot path.
Mitigating factors:

1. **Bounded by destination pool size.** Destination IOSurfaces come
   from `acquire_pixel_buffer` / `acquire_texture`, which are pool-
   backed. Typical pool sizes are 4–8 slots. After a short warmup,
   every destination IOSurface ID is already in the blitter cache and
   all blits hit the fast path.
2. **Not `VkDeviceMemory` / `vkCreateImage`.** `MTLTexture` wraps an
   already-allocated IOSurface — the underlying GPU memory was
   allocated by IOSurface when the pixel buffer was acquired. The
   blitter's cache miss creates an MTLTexture *object*, not a new GPU
   backing store. The NVIDIA concurrent-creation race that motivates
   the whole #319 umbrella is a Vulkan driver quirk and does not apply
   to Metal MTLTexture construction.
3. **Source texture is unconditionally re-created, every call.** Both
   `blit_copy` and `blit_copy_iosurface_raw` call
   `create_metal_texture_from_iosurface(device, src, 0)` for the source
   IOSurface on every invocation without caching (by design — source
   IOSurfaces are transient and unique per frame). This per-call
   allocation is unrelated to cold-key cache growth and exists today in
   Sandbox-classed code. It is a pre-existing cost that the capability
   split does not change.

### Vulkan backend — no cache, no cold-key growth

`libs/streamlib/src/vulkan/rhi/vulkan_blitter.rs`.

`VulkanBlitter` has no persistent state keyed by (width, height, format,
usage). `clear_cache()` is a no-op. `blit_copy` does, however, allocate
per call:

- `vkAllocateCommandBuffers` (1 primary CB, freed before return)
- `vkCreateSemaphore` timeline (created, signaled, waited, destroyed,
  all within one call)

These are transient, in-scope allocations and do not grow any cache.
They are not `VkDeviceMemory` and do not go through VMA pools, so they
are outside the #304 / NVIDIA DMA-BUF hazard class. There is no
cold-key path on this backend.

`blit_copy_iosurface_raw` on Vulkan returns `NotSupported` — IOSurface
is Apple-only.

### Cache invalidation

The only path that invalidates a pre-warmed Metal blitter cache is
`GpuContext::clear_blitter_cache() → RhiBlitter::clear_cache()`.

- Defined on the trait (`libs/streamlib/src/core/rhi/blitter.rs:28`).
- Public on `GpuContext` (and its `LimitedAccess` / `FullAccess`
  wrappers at `gpu_context.rs:1102` and `gpu_context.rs:1255`).
- **Not called from any production code path** — grep across
  `libs/streamlib`, `libs/vulkan-video`, `examples/`, and
  `apps/` finds no caller. It is available API only.

Practical implication for #324: a pre-warm performed in `setup()`
cannot be silently invalidated under the current code. If a future
caller starts invoking `clear_blitter_cache()` between
`setup()` and a `process()` tick, the next `blit_copy` will cold-miss
and the processor will see a one-shot unserialized MTLTexture creation.
Because this is an out-of-scope, explicit API call and there is no
existing misuse, we accept this as a documented risk rather than fence
it in the type system.

### Call sites in hot paths

All three hot-path callers are Metal-only, inside AVFoundation /
ScreenCaptureKit / VideoToolbox callback closures (effectively
`process()` in capability-type terms):

| File | Line | Call |
|---|---|---|
| `libs/streamlib/src/apple/processors/screen_capture.rs` | 171 (→ 242) | `gpu_context.blit_copy_iosurface` |
| `libs/streamlib/src/apple/processors/camera.rs` | 236 (→ 325) | `gpu_context.blit_copy_iosurface` |
| `libs/streamlib/src/apple/videotoolbox/decoder.rs` | 413 | `gpu.blit_copy` |

Linux processors do not call `blit_copy`. The Vulkan backend's lack of
cache is therefore not load-bearing for the classification — it's the
Metal backend whose behavior dominates the policy.

---

## Recommendation for #324

Reclassify the `blit_copy` family in
[`docs/design/gpu-capability-sandbox.md`](../design/gpu-capability-sandbox.md)
§1 as follows:

| Method | Cap | Notes |
|---|---|---|
| `blit_copy(src, dest)` | Split | Metal backend caches MTLTexture per destination IOSurface ID; cold-key path creates an MTLTexture. Bounded by destination pool size; not `VkDeviceMemory`. Pre-warm in `setup()` by blitting once to each pool slot. Vulkan backend has no cache. |
| `blit_copy_iosurface(src, dest, w, h)` (macOS) | Split | Same Metal cache. Same pre-warm applies. |
| `clear_blitter_cache()` | F | Only safe in `setup()` / teardown. Not called in any production path today. Moving to FullAccess prevents accidental misuse from `process()`. |

### Pre-warm shape

Callers whose `process()` bodies do `blit_copy*` (camera, screen
capture, videotoolbox decoder) pre-warm in `setup()` by:

1. Acquiring every destination slot their processor will use from the
   pixel buffer pool (which `setup()` can do via FullAccess-only
   `acquire_pixel_buffer` growth path).
2. Running a zero-cost warming blit per slot (e.g., blit from a
   throwaway source IOSurface of matching size — or, simpler, a
   dedicated Metal `fn warm_blitter_cache(&self, dest: &RhiPixelBuffer)`
   helper on `MetalBlitter` that runs `get_or_create_texture(dest)`
   without issuing the actual blit).

The warmer helper is a small additive change to the Metal blitter and
can land in #324 alongside the surface restriction. Vulkan needs no
warmer.

### No transparent-escalate helper

Per #320 §8.Q3 (already decided), no `acquire_*_or_escalate` helpers.
Pool-miss / cold-key paths in Sandbox should return a distinct error
(`StreamError::SandboxColdKey` or similar), so the compiler-visible
shape of an un-pre-warmed call site is an `?` that will propagate a
clear error at runtime — not a silent escalation.

For the blitter specifically: if callers miss a pre-warm, the first
`blit_copy` hits a `StreamError::SandboxColdKey`. The caller either
(a) pre-warms more thoroughly in `setup()` or (b) wraps the blit in
`escalate(|full| full.blit_copy(…))` at the offending call site. The
latter is fine as a bridge while pre-warm coverage is completed, so
long as it doesn't become the steady-state pattern.

### Debug instrumentation

The `sandbox.escalate(…)` trace events from #324 §5 (`tracing::trace!`
with processor id + duration, `tracing::warn!` on >1 escalation/sec)
cover blitter cold-key escalations too — nothing blitter-specific is
needed. If the escalation rate dashboard lights up on any of the three
Metal processors above, the fix is "pre-warm more slots in `setup()`,"
not "add a helper."

---

## References

- Design doc: [`docs/design/gpu-capability-sandbox.md`](../design/gpu-capability-sandbox.md) §1, §8.Q2
- Plan file: [`plan/346-blit-copy-classification.md`](../../plan/346-blit-copy-classification.md)
- Upstream ticket: [#346](https://github.com/tatolab/streamlib/issues/346)
- Downstream ticket: [#324 Restrict `GpuContextLimitedAccess` API surface to safe ops](https://github.com/tatolab/streamlib/issues/324)
- Metal blitter: `libs/streamlib/src/metal/rhi/blitter.rs`
- Vulkan blitter: `libs/streamlib/src/vulkan/rhi/vulkan_blitter.rs`
- Blitter trait: `libs/streamlib/src/core/rhi/blitter.rs`
