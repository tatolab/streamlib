# PluginAbiObject `make_*_borrow` MUST populate cached POD fields

## Symptom

A cdylib (Rust plugin loaded via `runtime.add_module(...)`) runs
end-to-end with **zero errors, zero panics, zero validation layer
complaints** — and produces **all-zero output**. Examples:

- Camera-as-cdylib pipeline runs 60 frames clean, encoder + decoder
  report 60 frames each, display surfaces 2 PNG samples — every
  sample is fully black.
- A compute kernel reports successful dispatch but downstream
  consumers see a buffer of zeros where pixel data is expected.

The pipeline LOGS look healthy. The PNG output is the only thing
that surfaces the bug.

## Trigger condition

You added a new cdylib-callable code path that hands a host-side
PluginAbiObject resource (`Texture`, `PixelBuffer`, `StorageBuffer`,
`UniformBuffer`) back through a vtable callback. The host wrapper
constructs a borrowed PluginAbiObject via one of the
`make_*_borrow(handle: *const c_void)` helpers in
`runtime/streamlib-engine/src/core/plugin/host_services.rs` and passes
that borrow to host-side code (e.g. a recorder method, a kernel
binding method) that internally calls a POD getter like
`.width()` / `.height()` / `.byte_size()` / `.mapped_ptr()` on the
borrow.

## Root cause

The cdylib PluginAbiObject carries cached POD fields (`width_cached`,
`height_cached`, `format_raw`, `byte_size_cached`,
`mapped_ptr_cached`, `plane_count_cached`, ...) so the cdylib's
public POD getters resolve as pure field reads with no plugin ABI hop. The
fields are populated at construction time via `from_arc_into_raw`
from the underlying inner.

A `make_*_borrow` helper reconstructs a `ManuallyDrop<PluginAbiObject>`
that wraps the SAME inner via `handle`. The deref still works for
methods that route through `host_inner()` / `buffer_ref()` /
`vulkan_inner()` — but the **cached POD fields are zero-initialized
on the borrow**. Any host-side code path that reads those fields
off the borrow gets `width == 0` instead of `width == 1920`.

## Where this bit streamlib

`color_converter::finish_buffer_to_image` reads
`dst.width()` / `dst.height()` to stuff width/height into
`ColorConverterPushConstants`. In cdylib mode, the dst Texture
borrow had `width_cached = 0`, push constants encoded 0×0
dimensions, the compute shader ran on a 0×0 texture region and
wrote nothing visible. The subsequent `vkCmdCopyImageToBuffer`
copied 1920×1080 of zero-initialized device-local memory into the
HOST_VISIBLE pixel buffer the camera publishes for IPC. Encoder
encoded all-zero frames. Decoder decoded all-zero frames. Display
showed black.

Empirical evidence (push-constant dump during investigation):

```
CDYLIB    push_const_u32s[16..18] = [0, 0]      ← width=0, height=0
BASELINE  push_const_u32s[16..18] = [1920, 1080]
```

## Fix

Each `make_*_borrow` must populate the cached POD fields from the
host-side inner via the engine-internal accessor for that resource
type. The pattern is:

```rust
fn make_texture_borrow(handle: *const c_void) -> ManuallyDrop<Texture> {
    // 1. Construct a minimal borrow first — just enough to deref
    //    through `host_inner()` / `vulkan_inner()` / `buffer_ref()`.
    use crate::host_rhi::HostTextureExt;
    let tex_for_inner = ManuallyDrop::new(Texture {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        width_cached: 0,
        height_cached: 0,
        format_raw: 0,
        _padding: 0,
    });
    // 2. Read the real values off the inner.
    let hvt = tex_for_inner.vulkan_inner();
    let width = hvt.width();
    let height = hvt.height();
    let format = hvt.format();
    // 3. Construct the final borrow with cached fields populated.
    ManuallyDrop::new(Texture {
        handle,
        vtable: host_gpu_context_limited_access_vtable(),
        width_cached: width,
        height_cached: height,
        format_raw: format as u32,
        _padding: 0,
    })
}
```

The two-step dance is intentional: `host_inner()` (and siblings)
only deref `self.handle` and ignore the cached fields, so the
intermediate "minimal" borrow is sufficient to reach the inner. The
second borrow is what the caller receives.

## Detection

The bug fails silently in CDYLIB mode and passes in BASELINE mode
(direct Rust-dep version of the same processor). When you see the
trigger pattern "cdylib pipeline clean / output zeros", first
verify: does any host-side code read a cached POD field off a
borrow returned by `make_*_borrow`? If yes, this is your bug.

A focused regression test that allocates a real resource of known
dimensions, constructs a `make_*_borrow(handle)`, and asserts the
borrow's POD getters return the real values catches the bug at the
data-structure level. See
`make_borrow_cached_field_regression_tests` in
`runtime/streamlib-engine/src/core/plugin/host_services.rs`.

## Why the panic-guards on `host_inner()` don't catch this

`host_inner()` panics from cdylib code (per the panic-guard in
`Texture::host_inner` and siblings). But the panic only fires when
called FROM cdylib code; inside a host-mode vtable callback,
`host_callbacks().is_some()` returns `false` and the deref proceeds
normally. The borrow goes through fine — it just resolves to zero
on the cached-field reads, with no error path firing.

## Reference

- Borrow helpers + the canonical fix pattern: `make_*_borrow` in
  `runtime/streamlib-engine/src/core/plugin/host_services.rs`.
- Regression tests: `make_borrow_cached_field_regression_tests` in
  the same file. Covers all four borrow helpers (texture,
  pixel_buffer, storage_buffer, uniform_buffer).
- The cdylib PluginAbiObject's `from_arc_into_raw` constructors populate
  cached fields at construction — the borrow helpers must mirror
  that contract.
