# Authoring a StreamLib surface adapter

This guide is for engineers building a new surface adapter — either
in-tree (`streamlib-adapter-vulkan`, `-opengl`, `-skia`,
`-cpu-readback`) or as a 3rd-party crate.

If you haven't yet, read
[`docs/architecture/surface-adapter.md`](architecture/surface-adapter.md)
first — it covers the customer-facing shape and the rationale.

## TL;DR

1. Depend on `streamlib-adapter-abi`.
2. Implement `SurfaceAdapter` for your adapter type, picking the right
   capability markers (`VulkanWritable`, `GlWritable`, `CpuReadable`,
   `CpuWritable`) for your view types.
3. Run `streamlib_adapter_abi::testing::run_conformance(&adapter, …)`
   from a `#[test]` to validate the shape.
4. Add a layout regression test if you cross any new ABI boundary
   (FFI, IPC wire format).

## Implementation recipe

### 1. Define your view types

A view is whatever your customer wants to interact with. For a Vulkan
adapter that's a struct holding the `VkImage` handle and current
layout; for a CPU-readback adapter it's a `&[u8]` slice; for Skia it's
an `SkSurface`. Views are short-lived, scope-bound, and parameterized
by the guard's lifetime via GATs:

```rust
struct VulkanWriteView<'g> {
    image: VkImageHandle,
    layout: VkImageLayoutValue,
    info: VkImageInfo,
    _marker: std::marker::PhantomData<&'g ()>,
}

impl VulkanWritable for VulkanWriteView<'_> {
    fn vk_image(&self) -> VkImageHandle { self.image }
    fn vk_image_layout(&self) -> VkImageLayoutValue { self.layout }
}

// Implement the extended marker so outer adapters that need full
// VkImage description (Skia, debug snapshotters) can compose on top.
impl VulkanImageInfoExt for VulkanWriteView<'_> {
    fn vk_image_info(&self) -> VkImageInfo { self.info }
}
```

### 2. Implement `SurfaceAdapter`

```rust
use streamlib_adapter_abi::{
    AdapterError, ReadGuard, StreamlibSurface, SurfaceAdapter, SurfaceId,
    WriteGuard,
};

pub struct MyAdapter { /* ... */ }

impl SurfaceAdapter for MyAdapter {
    type ReadView<'g> = MyReadView<'g> where Self: 'g;
    type WriteView<'g> = MyWriteView<'g> where Self: 'g;

    fn acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'g, Self>, AdapterError> {
        // 1. Refuse if a writer holds the surface (WriteContended).
        // 2. Wait on the acquire-side timeline-semaphore value
        //    (blocking — caller asked for blocking acquire).
        // 3. Transition the image layout if needed.
        // 4. Build the view and return it inside a ReadGuard.
        Ok(ReadGuard::new(self, surface.id, view))
    }

    fn acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'g, Self>, AdapterError> {
        // Same shape; exclusive — fail with WriteContended if any
        // reader or another writer holds the surface.
        Ok(WriteGuard::new(self, surface.id, view))
    }

    fn try_acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'g, Self>>, AdapterError> {
        // Same as acquire_read, but if a writer holds the surface
        // return Ok(None) instead of WriteContended (or blocking).
        // Used by processor-graph nodes that must not stall their
        // thread runner.
        Ok(Some(ReadGuard::new(self, surface.id, view)))
    }

    fn try_acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'g, Self>>, AdapterError> {
        // Same as acquire_write, but contention returns Ok(None).
        Ok(Some(WriteGuard::new(self, surface.id, view)))
    }

    fn end_read_access(&self, surface_id: SurfaceId) {
        // Signal the release-side timeline value. Update the layout
        // record. Decrement the per-surface read counter.
    }

    fn end_write_access(&self, surface_id: SurfaceId) {
        // Same shape, write side.
    }
}
```

The `end_read_access` / `end_write_access` methods are called by guard
`Drop` — they MUST be `&self`-callable (typically guarded by an
internal `Mutex` over per-surface state) and idempotent.

### 3. Validate against the conformance suite

```rust
#[test]
fn my_adapter_passes_conformance() {
    let adapter = MyAdapter::new(/* ... */);
    streamlib_adapter_abi::testing::run_conformance(&adapter, |id| {
        // Build a StreamlibSurface that matches what your adapter
        // expects. For a CPU-only adapter you can use the helper:
        //   streamlib_adapter_abi::testing::empty_surface(id)
        my_test_surface(id)
    });
}
```

The fixture exercises:
1. Single `acquire_read` / drop pairs leave no holders behind.
2. Single `acquire_write` / drop pairs leave no holders behind.
3. Two concurrent reads are permitted.
4. Write contends with a live read (returns `WriteContended`).
5. Write contends with a live write (returns `WriteContended`).
6. `try_acquire_read` returns `Ok(None)` (not an error) while a
   writer is held; `Ok(Some(_))` once released.
7. `try_acquire_write` returns `Ok(None)` while a reader is held.
8. Multiple-reader threads acquire/release in parallel without
   panicking — `Send + Sync` smoke test.

What the fixture does NOT exercise: real GPU work crossing the scope
(timeline values monotonically advance under load, layout transitions
succeed, parallel reads under frame-rate budget). `MockAdapter` does
no GPU work, so a clean conformance run is a *necessary* but not
*sufficient* gate. Vulkan/OpenGL/Skia adapters should add their own
adapter-specific tests on top.

## Subprocess crash safety

If your adapter is consumed from a polyglot subprocess (Python or
Deno over FD-passing IPC), use `SubprocessCrashHarness` from
`streamlib_adapter_abi::testing` to validate that your adapter
handles "subprocess SIGKILL'd while holding write" cleanly. The
harness spawns a subprocess, runs a `post_spawn` hook (typically to
close the parent's copy of inherited fds), waits a configurable
delay, SIGKILLs, then polls a caller-provided observer until it
reports cleanup or the timeout fires.

```rust
let outcome = SubprocessCrashHarness::new(cmd)
    .with_timing(CrashTiming::AfterDelay(Duration::from_millis(100)))
    .with_cleanup_timeout(Duration::from_secs(2))
    .with_post_spawn(|_child| { /* close parent fd, set up observer */ Ok(()) })
    .run(|| /* return Ok(()) when host-side cleanup is observed */);
```

The host-side surface-share watchdog wired to `EPOLLHUP` on the
per-subprocess socket is tracked in
[#520](https://github.com/tatolab/streamlib/issues/520). Once it
ships, your adapter's crash test should demonstrate it integrates
correctly. Until then, write a self-contained crash test that
exercises your adapter's own state machine using a pipe-based
observer (as the harness self-test in
`libs/streamlib-adapter-abi/tests/subprocess_crash.rs` does).

## Polyglot considerations

Subprocess Python/Deno cdylibs MUST NOT call `vkAllocateMemory`,
`vkCreateImage`, or any Vulkan/IOSurface/Metal allocation API —
allocation only happens host-side, behind the surface-share IPC.
The carve-out: subprocess code MAY call `VkImportMemoryFdInfoKHR` +
`vkBindBufferMemory` + `vkMapMemory` on an FD the host already
handed it. See the polyglot workflow in
`.claude/workflows/polyglot.md`.

## Layout regression tests

If your adapter introduces any new `#[repr(C)]` type that crosses
an FFI or IPC boundary, lock its layout with
`mem::size_of::<T>()` + `offset_of!` assertions, just like the
core ABI does in
`libs/streamlib-adapter-abi/src/surface.rs::tests`. If the type is
mirrored in Python or Deno, ship the twin test on the polyglot side
in the same commit.

## Stability contract

`STREAMLIB_ADAPTER_ABI_VERSION` is the major version, currently `1`.
Bumps only on a breaking change. The trait does not carry a
`trait_version()` method — Rust vtable layouts already enforce
in-process compatibility at compile time; the constant becomes
load-bearing at the cdylib boundary when dynamic adapter loading
lands (planned via the same `#[repr(C)] AdapterDeclaration` shape
`streamlib-plugin-abi` uses).

Non-breaking changes (do NOT bump the major):
- Adding new methods to `SurfaceAdapter`.
- Adding new variants to `AdapterError`. (Callers must not match
  exhaustively without `_`.)
- Filling reserved bytes in `SurfaceSyncState` / `VkImageInfo` with
  named fields. The reserved zones are explicitly there for this.

Breaking changes (do bump):
- Renaming or removing a method or trait.
- Changing an existing method signature.
- Changing the `#[repr(C)]` layout of `StreamlibSurface`,
  `SurfaceTransportHandle`, `SurfaceSyncState`, or `VkImageInfo`
  (offsets, sizes, alignment).

When the major bumps, the polyglot mirrors update in lockstep — Python
`STREAMLIB_ADAPTER_ABI_VERSION` and Deno `STREAMLIB_ADAPTER_ABI_VERSION`
both flip in the same commit.
