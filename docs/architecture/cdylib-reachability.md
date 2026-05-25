# Cdylib reachability

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).

How to decide whether a new `Host*` Vulkan RHI primitive needs a
FullAccess vtable slot, or can be reached directly from workspace
plugin cdylib code through the existing v9 `host_vulkan_device_arc`
bridge.

## Why this doc exists

The cdylib boundary has a known trap: seeing a `host_inner()` panic
on one `Host*` β-shape (e.g., `Texture::host_inner()`) and assuming
by analogy that every other `Host*` constructor on the engine needs
a bridge. That assumption is wrong by default — most `Host*`
constructors take only `&Arc<HostVulkanDevice>` plus primitives and
touch only `pub` accessors on the device. Workspace plugin cdylibs
that hold the device Arc (via the v9 bridge) can call them directly,
no new ABI surface needed.

This doc captures the decision tree, the route-1 vs route-2 split,
and the enforcement layers — so a future agent picking up "the cdylib
can't reach X" doesn't file a defensive bridge before reading the
constructor body.

## Decision tree

```
Workspace plugin cdylib needs Arc<HostX> (a new resource type, or a
new constructor on an existing type).

  ┌─────────────────────────────────────────────────────────┐
  │ Q1: Does the constructor / extractor body reach for     │
  │     `host_inner()` (β-shape deref) or                   │
  │     `host_callbacks()` (cdylib-mode guard)?             │
  └───────────────┬─────────────────────────────────────────┘
                  │
        ┌─────────┴─────────┐
       no                  yes
        │                   │
        ▼                   ▼
   Route 2 works       Route 1 needed
   (direct call)       (new vtable slot)
```

## Route 2 — direct constructor call (preferred)

Applies when:

- The constructor takes only `&Arc<HostVulkanDevice>` (or other
  cdylib-reachable Arcs) and POD primitives.
- Its body uses only `pub` accessors on `HostVulkanDevice`
  (`allocator`, `device`, `dma_buf_buffer_pool`,
  `dma_buf_image_pool*`, `opaque_fd_buffer_pool*`,
  `opaque_fd_image_pool*`, `drm_modifier_table`, etc.) plus
  `vulkanalia-vma`.
- No `host_inner()` deref, no `host_callbacks()` check, no β-shape
  indirection.

Cdylib path:

```rust
gpu.escalate(|full| {
    let device_arc = full.host_vulkan_device_arc()?;
    // Direct constructor call — same code the host calls.
    let resource = HostVulkanXxx::new(&device_arc, ...args)?;
    let arc = Arc::new(resource);
    // ...use arc through the adapter...
    Ok(())
})
```

This is the cheapest path. No ABI surface ships; the cdylib-reachability
invariant is captured in:

- The type-level `# Cdylib reachability` docstring on each
  `Host*` type, enumerating the two paths and warning reviewers
  against introducing a guard inside the constructor body.
- `cargo xtask check-cdylib-reach` (see
  [Enforcement](#enforcement) below).
- The `load_project_dylib_*_smoke.rs` integration tests, which
  exercise the cdylib direct-call path against each surface adapter.

The existing route-2 paths today (verified at the docstring sites):

- `HostVulkanBuffer::new` / `::new_opaque_fd_export` / `::new_opaque_fd_export_device_local`.
- `HostVulkanTimelineSemaphore::new` / `::new_exportable` (plus
  the v6 `create_timeline_semaphore` slot for the non-exportable
  flavor when the cdylib doesn't already hold the device Arc).
- `HostVulkanTexture::new_render_target_dma_buf` (low-level
  path; the high-level `acquire_render_target_dma_buf_image`
  FullAccess slot is the recommended entry point for most adapters).

## Route 1 — new vtable slot (only when route 2 doesn't fit)

Applies when:

- The constructor / extractor must touch host-private state
  (`host_inner()` for β-shape deref, host-only registries, etc.).
- Or: the resource crosses the FFI as a non-`Arc` opaque handle
  (β-shape with `(handle, vtable, POD)` layout, like `Texture` /
  `PixelBuffer` / `StorageBuffer`).

Existing route-1 bridges:

- **v9 `host_vulkan_device_arc`** — hands a cloned
  `Arc<HostVulkanDevice>` raw pointer to the cdylib.
- **v10 `host_vulkan_texture_arc`** — extracts an
  `Arc<HostVulkanTexture>` from a `Texture` β-shape (the β-shape's
  `host_inner()` panics in cdylib mode, hence the bridge).

When adding a new slot:

1. Bump `GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION`.
2. Update the layout regression test
   (`gpu_context_full_access_vtable_layout` in plugin-abi): assert
   the new `offset_of!` and the new `size_of`.
3. Update `host_services_layout_versions_pinned` with the bumped
   version constant.
4. Add at least one tier-1 wire-format test
   (`HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.<slot>(null) == null`).
5. Document the new version in the constant's docstring.

## Trap class

The mistake this doc exists to prevent: filing "bridge for X /
bridge for Y / bridge for Z" issues defensively without verifying
whether the constructor bodies actually need bridges. To the best
of our current knowledge, the historical case that motivated this
doc had three of four "bridges" proposed defensively turn out to
be already-working via route 2 — only one extractor (the
`Texture` β-shape's `host_inner()` deref) needed a real bridge.

Before filing a "bridge needed for X" issue:

1. **Read the constructor body** (and any internal helpers it
   delegates to). Does it reach for `host_inner` or
   `host_callbacks`?
2. If no — route 2 already works. The right deliverable is a
   docstring update + a smoke test, not a new vtable slot.
3. If yes — route 1 is needed, follow the checklist above.

## Enforcement

Three layers protect the invariant:

1. **`cargo xtask check-cdylib-reach`** — AST-level scan of every
   `.rs` file under `libs/streamlib-engine/src/vulkan/rhi/`. For
   each `impl HostVulkan*` block, walks every constructor-class
   method's body (`new*` / `create*` / `from_*`) and fails the
   build if any call expression references `host_inner` or
   `host_callbacks`. Adding a new `HostVulkan*` type / new file
   under the directory is covered automatically — the check
   tracks the directory, not a curated file list. Wired into CI
   as `check-cdylib-reach.yml`. Comments and string literals are
   ignored automatically by `syn`'s tokenization.
2. **Type-level docstrings** on `HostVulkanBuffer`,
   `HostVulkanTimelineSemaphore`, `HostVulkanTexture`. Each
   carries a `# Cdylib reachability` section enumerating the two
   paths and warning reviewers against introducing a guard. The
   docstring is what a reviewer sees first on a constructor
   change; the xtask is the backstop when the reviewer skipped it.
3. **Dlopen smoke tests** at
   `libs/streamlib-engine/tests/load_project_dylib_*_smoke.rs`
   (one per surface adapter: cpu-readback, vulkan, opengl, cuda).
   Each test exercises the cdylib direct-call path end-to-end.
   If a constructor regresses to host-private state, the matching
   smoke fails at runtime via `run_host_extern_c`'s panic-safety
   net surfacing the panic as `ERR:<msg>`. These tests are GPU-
   required and run locally only (no GPU CI runner planned per
   `project_ci_strategy_no_gpu`).

The three layers are defense in depth: the xtask catches the
syntactic regression before merge; the docstring deters the
regression at review time; the smoke tests catch any semantic
gap the xtask might miss.

## Reference

- `xtask/src/check_cdylib_reach.rs` — the AST scan + tests.
- `.github/workflows/check-cdylib-reach.yml` — CI wiring.
- `libs/streamlib-engine/src/vulkan/rhi/` — `# Cdylib reachability`
  docstrings on the `Host*` types that documented the route-2
  invariant (currently `HostVulkanBuffer`,
  `HostVulkanTimelineSemaphore`, `HostVulkanTexture`).
- `libs/streamlib-engine/tests/load_project_dylib_{cpu_readback,
  vulkan, opengl, cuda}_smoke.rs` — end-to-end exercise of the
  cdylib path.
