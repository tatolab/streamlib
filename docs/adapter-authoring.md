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
struct VulkanReadView<'g> {
    image: VkImageHandle,
    layout: VkImageLayoutValue,
    _marker: std::marker::PhantomData<&'g ()>,
}

impl VulkanWritable for VulkanWriteView<'_> {
    fn vk_image(&self) -> VkImageHandle { self.image }
    fn vk_image_layout(&self) -> VkImageLayoutValue { self.layout }
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
        // 1. Refuse if a writer holds the surface.
        // 2. Wait on the acquire-side timeline-semaphore value.
        // 3. Transition the image layout if needed.
        // 4. Build the view and return it inside a ReadGuard.
        Ok(ReadGuard::new(self, surface.id, view))
    }

    fn acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'g, Self>, AdapterError> {
        // 1. Refuse if any reader or another writer holds the surface
        //    (return AdapterError::WriteContended).
        // 2. Wait on the acquire-side timeline-semaphore value.
        // 3. Transition the image layout if needed.
        Ok(WriteGuard::new(self, surface.id, view))
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
1. `trait_version()` reports the right ABI major.
2. Single `acquire_read` / drop pairs leave no holders behind.
3. Single `acquire_write` / drop pairs leave no holders behind.
4. Two concurrent reads are permitted.
5. Write contends with a live read (returns `WriteContended`).
6. Write contends with a live write (returns `WriteContended`).
7. Multiple-reader threads acquire/release in parallel without
   panicking — `Send + Sync` smoke test.

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
per-subprocess socket is a separate piece (see follow-up issue) —
your adapter's crash test should demonstrate it integrates
correctly with that watchdog when both are wired up. Until the
watchdog ships, write a self-contained crash test that exercises
your adapter's own state machine using a pipe-based observer (as
the harness self-test does).

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

`STREAMLIB_ADAPTER_ABI_VERSION` is the runtime gate. As of `1`:

- Adding new methods to `SurfaceAdapter` is non-breaking.
- Adding new variants to `AdapterError` is non-breaking (callers must
  not match exhaustively against it without `_`).
- Adding new fields to `StreamlibSurface`'s `pub(crate)` substructures
  is non-breaking — only the public field offsets are layout-locked.
- Renaming or removing any of the above is a major bump.

When the major bumps, the polyglot mirrors update in lockstep — Python
`STREAMLIB_ADAPTER_ABI_VERSION` and Deno `STREAMLIB_ADAPTER_ABI_VERSION`
both flip in the same commit.
