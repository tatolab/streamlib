# Change: raw-handle-export-contract

Implements the spelling half of the `[raw-handle-export-contract]` DECIDED entry in
§Packages: `export_opaque_fd` on the wheel's Full capability surface, returning the
typed `OpaqueFdTextureExport`, with `export_dma_buf`'s refusal redirected. The gate
half (Full-only minting, the use bound, allocation-not-frame semantics) is plan text
already merged and needs no code; the OPEN zero-copy-per-frame entry is out of scope.

Scale tier: change artifact **+ ADR** — this is the Python API's public contract. The
ADR arm is discharged by `docs/decisions/raw-handle-export-contract.md`, written by
the align that decided the spelling (the kernel-kind-parity-bar precedent: the ADR
that owns the decision exists; a second would restate it).

Recon verified at HEAD `da1192ba` by two read-only sweeps (helper IPC/wire path;
wheel surface + tests). Ticket: #1900 — this change maps to it 1:1.

## Behavior after this change

```python
def setup(self, ctx: RuntimeContextFullAccess) -> None:
    self.output = ctx.gpu_full_access.acquire_texture(
        WIDTH, HEIGHT, "rgba16_float", ["storage_binding", "copy_src"])
    with ctx.gpu_limited_access.resolve_surface(self.output.surface_id) as resolved:
        export = ctx.gpu_full_access.export_opaque_fd(resolved)
        third_party.attach(
            fd=export.fd,                     # caller-owned; importer adopts on success
            size=export.allocation_byte_size, # whole VkDeviceMemory, offset 0
            width=export.width, height=export.height, fmt=export.format,
            device_uuid=export.exporting_device_uuid)
```

The export answers child-locally: the OPAQUE_FD memory fd arrived over SCM_RIGHTS at
checkout (`resolve_surface` → the surface-share `check_out`), so `export_opaque_fd`
dups and hands it with no parent round trip — the same answering pattern as
`export_dma_buf` (`python_processor_context.rs:1381-1402`, two-line body, GIL
detached). `OpaqueFdTextureExport` carries the plan-fixed allocation-stable set:

| field | type | source |
|---|---|---|
| `fd` | `int` | fresh dup of the checkout's plane fd |
| `allocation_byte_size` | `int` | `vk_image_allocation_size` (wire, refused-if-absent today) |
| `width`, `height` | `int` | wire |
| `format` | `str` | wire (`GpuSurfaceHandle.format` precedent) |
| `vk_image_tiling`, `vk_image_usage_flags`, `vk_image_mip_levels`, `vk_image_array_layers`, `vk_image_samples` | `int` | wire — already echoed on every texture checkout, raw Vulkan numeric values (the ABI-stable spec constants a foreign `VkImageCreateInfo` needs; `current_image_layout` precedent) |
| `dedicated_allocation` | `bool` | `True` by construction for the flavour (`vulkan_texture.rs:644-646`) |
| `vk_memory_type_index` | `int` | NEW wire field |
| `exporting_device_uuid` | `bytes` (16) | NEW wire field |

Refusals, all `PyRuntimeError`, all naming the fix: a DMA-BUF-flavoured texture →
points at `export_dma_buf` (the mirror of the redirect below); an acquired-by-name
texture with no checkout → "resolve its surface id first" (the existing
`AcquiredDeviceTexture` refusal shape, `python_helper_process_pixel_exchange.rs:624-628`);
a pixel buffer → wrong flavour, points at `export_dma_buf`; a checkout whose
registration lacks a new field → refused naming the field (the
`allocation_byte_size` precedent, `:293-304`).

## ADDED

- `PythonGpuContextFullAccess::export_opaque_fd` (`python_processor_context.rs`,
  beside `export_dma_buf` at `:1381`), Linux-gated, `python.detach`-wrapped;
  dispatches to a new `HelperCheckedOutTextureSurface::export_opaque_fd` beside the
  DMA-BUF twin (`python_helper_process_pixel_exchange.rs:488-505`) and a
  three-arm fan-out beside `:620-630`.
- `OpaqueFdTextureExport` frozen pyclass: private fields + `#[getter]` methods (the
  `GpuSurfaceCheckOutLease` convention, `python_processor_context.rs:771-786` —
  `#[pyo3(get)]` is unused in this tree); registered in `lib.rs:50-54`; re-exported
  in `__init__.py` (alias form `:18-35`, `__all__` entry sorted between
  `"MonotonicTimer"` and `"ProcessorInputPortReference"`); `@final` class with
  `@property` getters in `_engine.pyi` beside `GpuSurfaceCheckOutLease` (`:717`).
  Docstrings carry the two contract sentences: adopt-on-success fd ownership
  (never close after a successful import, always after a failed one) and
  consume-as-image (a linear buffer mapping over OPTIMAL-tiled memory yields
  block-linear bytes, never pixels). Class docstring notes the deliberate
  `GpuSurface*`-prefix departure — the object names an allocation, not a
  frame-bearing surface.
- Two registration fields on the surface-share texture register (OPAQUE_FD branch,
  `surface_store.rs:1429-1452`), echoed by the service (`unix_socket_service.rs:841-867`)
  and parsed into `TextureCheckOutRegistrationMetadata`
  (`python_helper_process_pixel_exchange.rs:242-340`):
  - `vk_memory_type_index` — from VMA `get_allocation_info(...).memoryType`
    (safe wrapper exists, `allocator.rs:129-134`; the export site already calls it
    for the size, `vulkan_texture.rs:1191-1199`);
  - `exporting_device_uuid` — 32-hex-char string, from the exporting texture's own
    device (`HostVulkanDevice::physical_device_uuid`, `vulkan_device.rs:3423-3432`;
    own-device sourcing per the `export_storage_buffer_opaque_fd` precedent,
    `gpu_context.rs:2353-2360`; hex encoding per the device-export staging
    precedent, `escalate_response.rs:98-105` / `parse_device_uuid`).
  The surface-share wire is additive JSON (no `repr(C)`, no layout test applies);
  the escalate wire is untouched.
- Child-side parse of the recipe fields already on the wire
  (`vk_image_type/mip_levels/array_layers/samples/tiling/usage` — echoed on every
  texture checkout since `unix_socket_service.rs:321-351`, currently unread by the
  wheel), retained on `HelperCheckedOutTextureSurface`.
- Tests (land with the implementation, `requires_gpu` where marked):
  - probe: `export_opaque_fd` on a resolved kernel output returns the fd + full
    metadata (recipe constants match `new_opaque_fd_export`, UUID non-zero,
    `allocation_byte_size >= tight size`) — extends `TextureHandleRoundTripProbe`
    (`device_exchange_probes.py:433-529`);
  - rig round trip (#1900's validation shape): the exported fd + metadata imported
    by independent external-memory code (the in-tree CUDA import path driven as a
    foreign consumer) reads the kernel's pixels;
  - the fd-outlives-teardown probe (audit addendum on #1900): an imported export
    survives engine destruction of the source allocation;
  - refusals: DMA-BUF-flavoured texture, unresolved acquired texture, pixel
    buffer — each named; and the redirect assertion below;
  - stubtest (`python-wheel.yml:121-123`) and pyright (`:188-190`) cover the new
    class by construction.

## MODIFIED

- `export_dma_buf`'s OPAQUE_FD refusal points at the new name — the Rust string
  (`python_helper_process_pixel_exchange.rs:497-501`) and its doc comment, and the
  stub docstring (`_engine.pyi:568-571`).
- `test_a_texture_handle_round_trips_across_the_process_boundary` asserts the
  refusal now names `export_opaque_fd` (`test_device_exchange.py:272-275`).
- `README.md:247-249` — doc rot: `export_dma_buf` reads as a surface method; it is
  a `GpuContextFullAccess` method, and the OPAQUE_FD door is unmentioned.

## REMOVED

Nothing. No `REMOVED:` bullets; the ship gate has nothing to verify gone.

## Facts resolved by reading (not decisions)

- A kernel's acquired output holds **no fd child-side** (`HelperAcquiredTexture`,
  `:372-385`; the escalate socket carries zero fds) — resolve-first is the shipped
  fd-delivery path (`device_exchange_probes.py:490-528`), so the refusal, not a new
  delivery protocol, is correct for the unresolved arm.
- The recipe constants on the wire mirror `new_opaque_fd_export` by construction
  (`surface_store.rs:1410-1414` says so); exporting them re-reads what the engine
  already publishes.
- Naming note: Rust `HostVulkanTimelineSemaphore::export_opaque_fd`
  (`gpu_context.rs:2253`) shares the method name on an unrelated type; the Python
  spelling is plan-fixed and the surfaces never meet.

## Out of scope

The OPEN zero-copy-per-frame entry (its own align); `export_dma_buf`'s typed-object
grandfathering (ADR-noted future rename); any Rust-side `GpuContext` texture-flavour
opaque-export public surface beyond what registration needs; the in-tree consumer
importer's dedicated-chain/memoryTypeIndex conformance gaps (PR-noted on #1903,
NVIDIA-tolerated, separate work).
