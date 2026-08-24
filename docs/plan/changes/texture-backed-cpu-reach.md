# Change: texture-backed-cpu-reach

Implements the `[texture-backed-cpu-reach]` DECIDED entry (§Graphics) and the staged
`cpu()` arm of the cast-object entry (§Packages), decided in the #1758 align. ADR:
`docs/decisions/texture-backed-cpu-reach.md` (owner, 2026-08-24).

Scale tier: change artifact **+ ADR** — it deletes an escalate wire op and response
variant, adds an RHI memory pool, and changes the Python API's public contract; the ADR
is already written by the align. Recon verified at HEAD `dbc99aed` by two read-only
sweeps (escalate wire + wheel doors; RHI pool + probe machinery).

## Sequencing

Independent of every in-flight change. Everything it rides is on main: the cast-object
composable and its `cpu()` door, `open_cpu_readback_staging`, the blocking copy ops,
the surface-share checkout, and `ConsumerVulkanBuffer::from_opaque_fd`. Within the
change, the engine half (pool + memory-type plumbing + wire deletion) sequences before
or with the wheel half — the wheel's readback checkout binds a stated memory type
index the engine must first put on the wire.

## Behavior after this change

A numpy-only author reads a kernel output on the CPU — no CUDA runtime, no GPU
package:

```python
frame = ctx.inputs.read("depth_from_upstream", into=DepthFrame)
with frame.cpu() as img:          # texture-backed: staged read-in, numpy over the mapping
    peak = img.max()
```

And fills a LUT texture they own (the owner ruling that widened #1758):

```python
def setup(self, ctx):
    self.lut = ctx.gpu_limited_access.acquire_texture(256, 1, "rgba32_float",
                                                      ["texture_binding"])
    with self.lut as surface:
        surface.lock(read_only=False)      # read-in, then a writable staged array
        surface.as_numpy()[:] = film_curve
        surface.unlock()                   # publishes: one copy, ordered by the engine
```

`copy_src` / `copy_dst` are implied — `usage=["texture_binding"]` suffices. The same
spellings on a pixel-buffer frame behave exactly as today; no door names the backing.
A raise inside `cpu()` over a staging discards the pending publish and propagates; over
a coherent mapping, stores already landed — the one stated contract is never-torn.
`contended` no longer exists on the wire; every copy blocks.

## Facts resolved by recon (not owner decisions)

- **`run_cpu_readback_copy` is caller-less today** — its callers are three engine seam
  tests; the wheel's only readback-wire use is reading the `writable` bool off
  `open_cpu_readback_staging` (`python_helper_process_pixel_exchange.rs:1658`). This
  change *promotes* the blocking op to load-bearing; it does not preserve an in-use op.
- **No `contended` consumer exists in any language.** The helper's response dispatch
  (`_helper.py:197`) recognises only `"ok"` and would surface a `contended` as a
  message-less refusal. The only producer is `subprocess_escalate.rs:1737`.
- **Same-word families that are NOT this surface and must not be touched:**
  `AdapterError::WriteContended` (surface-adapter read/write exclusion,
  `adapters/streamlib-surface-adapter/src/error.rs:20`) and the placement gate's
  contention vocabulary (`xtask/src/check_no_in_process_placement.rs:80`).
- **The child-side OPAQUE_FD import requires `HOST_VISIBLE | HOST_COHERENT`**
  (`consumer_vulkan_buffer.rs:273-278`, `consumer_vulkan_device.rs:377-386` treats the
  flags as required), and nothing on the export-staging path flushes or invalidates —
  repo-wide, `flush_allocation` / `invalidate_allocation` exist only in vulkan-video.
  The cached pool therefore keeps `HOST_COHERENT` **required** and takes `HOST_CACHED`
  as VMA-**preferred** via `HOST_ACCESS_RANDOM` (`vk_mem_alloc.h:4066-4083`: prefers
  cached, "cannot require it"). On the rig the cached type is also coherent; a device
  whose only cached type is non-coherent silently misses the preference — inside the
  decided "slower there, never refused" contract, with no flush machinery invented.
- **The fallback trigger is explicit, not a probe failure.** With HOST_CACHED merely
  preferred, `find_memory_type_index_for_buffer_info` succeeds on a cache-less device;
  detection is reading the returned index's flags and leaving the pool field `None`
  when `HOST_CACHED` is absent — the established soft-absence shape
  (`vulkan_device.rs:1283-1294`, consumed as `opaque_fd_image_pool().is_some()` at
  `subprocess_escalate.rs:470`).
- **A conforming OPAQUE_FD import binds the exporter's memory type index**
  (`VUID-VkMemoryAllocateInfo-allocationSize-01742`; no fd-props query exists for
  OPAQUE_FD). Texture registrations already carry `vk_memory_type_index` on the
  surface-share wire and the wheel refuses a checkout without it
  (`surface_store.rs:1432-1441`, `python_helper_process_pixel_exchange.rs:397-406`);
  **the staging registration omits it** (`surface_store.rs:1309-1349`). Today host and
  child agree by coincidence (both land on the uncached type); the cached pool ends
  the coincidence, so the index plumbing below is mandatory, not hardening.
- **The usage implication site is `parse_texture_usages`**
  (`subprocess_escalate.rs:4525`) — the sole entry point for the wire token list,
  engine-side, used only by the `AcquireTexture` arm, so Rust's
  `TexturePoolDescriptor` stays explicit with no spill. It cannot break flavour
  derivation: the OPAQUE_FD fixed usage set already contains both copy bits
  (`:4511-4517`) and the `RENDER_ATTACHMENT` branch returns earlier (`:4499`).
- **The write-back guard does not change.** `surface_can_take_write_back` is already
  `copy_dst`-derived for textures (`surface_export_staging.rs:480-487`); implying
  `copy_dst` narrows the refusal to producer-owned pooled frames and foreign
  registrations without transfer usage, mechanically.
- **The staging residency axis is untouched.** The cached pool is an allocation source
  behind the existing `HostVisible` residency — no third residency, no new
  surface-share id suffix, no eviction change.
- **No `PROTOCOL_VERSION` bump.** The deleted op was never sent and the deleted
  response was only ever an answer to it; no surviving op changes shape, so no live
  parent/helper pair can observe skew.

## REMOVED

Bare patterns — the ship gate greps each line verbatim as a fixed string.

- REMOVED: try_run_cpu_readback_copy
- REMOVED: TryRunCpuReadbackCopy
  The wire variant + `#[serde(rename)]` (`escalate_request.rs:88-89`), the payload
  struct (`:1971-1990`), the direction enum
  `EscalateRequestTryRunCpuReadbackCopyDirection` (`:1959-1967`), the `request_id()`
  arm (`subprocess_escalate.rs:124`), the dispatch arm incl. the non-Linux refusal
  (`:628-659`), the golden wire vectors (`escalate_wire_encoding_tests.rs:134`,
  `:372-375`), and the use-list entries.
- REMOVED: EscalateResponseContended
  The response variant + struct (`escalate_response.rs:13-14`, `:23-32`), its golden
  vector (`escalate_wire_encoding_tests.rs:143`), the sole producer
  (`subprocess_escalate.rs:1737`), and the now-unreachable
  `EscalateResponse::Contended(_) => panic!` arms in the end-to-end helper
  (`:10438-10529`).
- REMOVED: ReportContended
- REMOVED: SurfaceExportStagingCopyContention
  The whole contention enum (`subprocess_escalate.rs:1552-1556`) and `fn contention()`
  (`:1598-1609`); with one arm left there is no choice left. The 4-way
  `(direction, contention)` match in `handle_surface_export_staging_copy`
  (`:1714-1731`) collapses to a 2-way direction match.
- REMOVED: try_refill_surface_export_staging
- REMOVED: try_copy_surface_export_staging_back_to_surface
- REMOVED: try_submit_staging_copy_and_wait
  The two public `GpuContext` methods (`surface_export_staging.rs:865-881`,
  `:1019-1037`), their `GpuContextLimitedAccess` wrappers (`:1258-1266`, `:1278-1286`),
  the private submit helper (`:728-741`), and one of their two unit tests
  (`a_try_copy_answers_contended_only_while_the_recorder_is_held`, `:2141-2188`).
  The other is rewritten, not deleted — see MODIFIED below.
- REMOVED: while_holding_the_refill_recorder_for_a_test
  The `#[cfg(test)]` recorder hook (`surface_export_staging.rs:284-297`); its sole
  external user is the contended seam test below.
- REMOVED: the_seam_answers_contended_while_the_recorder_is_held
  `subprocess_escalate.rs:5504-5566`.
- REMOVED: surface has no host mapping; it is a DEVICE_LOCAL allocation
  The `lock()` refusal (`python_processor_context.rs:380-384`) — the door opens
  instead.
- REMOVED: its memory is tiled device memory
  The `HelperCheckedOutSurface::Texture` refusal arm of `host_visible_pixel_plane`
  (`python_gpu_surface_pixel_exchange.rs:128-134`).
- REMOVED: whose memory is not mapped into this
  The `AcquiredDeviceTexture` refusal arm (`:135-143`).
- REMOVED: a DEVICE_LOCAL allocation reaches the CPU through the
  The `host_visible_dlpack_capsule` null-base refusal (`:437-442`).

## MODIFIED

- MODIFIED: the residency seam's `HostVisible` arm (`surface_export_staging.rs:517-531`,
  "the one line the residency decides") calls the new host-cached constructor, which
  degrades to the sequential-write pool when the cached pool is absent — the one place
  "never refused" lands. The `HostVisible` doc (`:94-96`) gains the cached preference.
- MODIFIED: `register_surface_export_staging` (`surface_store.rs:1309-1349`) puts the
  exporter's `vk_memory_type_index` on the surface-share wire, exactly as texture
  registrations do (`:1432-1441` → `state.rs:144` → `unix_socket_service.rs:882-883`);
  one rule for every OPAQUE_FD checkout, matching the wheel's existing refusal text.
- MODIFIED: the child's readback import binds the stated memory type index instead of
  `find_memory_type`'s first-match guess — a stated-index variant of the
  `ConsumerVulkanBuffer::from_opaque_fd` path (`consumer_vulkan_buffer.rs:70-95`,
  `:213-307`). The DMA-BUF and CUDA import paths are untouched.
- MODIFIED: `parse_texture_usages` (`subprocess_escalate.rs:4525-4547`) implies
  `COPY_SRC | COPY_DST`; its two unit tests (`:4892`, `:4901`) assert the implied mask;
  `texture_usages_to_wire` echoes the derived list back to the caller unchanged.
- MODIFIED: the wheel's CPU accessors route texture-backed surfaces over the mapped
  readback staging. `host_visible_pixel_plane`'s two texture arms answer the staging's
  plane view instead of refusing, and `bytes_per_row` / `base_address` / `as_numpy` /
  `__dlpack__`'s CPU arm inherit it with no signature change.
  > ~~`lock()` checks out the staging on first ask (read intent runs the read-in copy;
  > write intent arms the publish).~~ — Superseded 2026-08-24 by the shipped code.
  > `lock()` also gates `__dlpack__`'s *device* arm, which requires the same lock, so a
  > read-in there would cost every `as_device_tensor` user of a texture-backed surface
  > one host staging copy per frame — a path this change does not touch. The read-in and
  > the arming hang off the first host-side accessor instead
  > (`open_the_staged_cpu_door_over_this_frame`, called from `base_address` and
  > `__dlpack__`'s host arm), once per lock scope; `bytes_per_row` maps without reading
  > in, since a pitch yields no pixels and arms nothing. The DECIDED entry's "entering
  > the staged CPU door always reads the current frame in" holds unchanged — taking the
  > host side is what enters the door, not taking the lock.
- MODIFIED: `unlock()` / scope exit publishes a staged CPU write as one
  `run_cpu_readback_copy buffer_to_image`, reusing the `PendingDeviceWriteBack`
  arm/discard/publish protocol (`python_gpu_surface_pixel_exchange.rs:699-751`) — a
  propagating raise discards by never sending the copy. Shipped as
  `PendingStagedWriteBackToSurface`: serving two stagings, the armed state carries which
  one holds the edit, and a second *distinct* source in one lock scope is refused by
  name rather than replacing the first — neither staging holds both edits, so there is
  no order in which publishing both does not overwrite one.
- MODIFIED: `surface_can_take_write_back`'s probe
  (`python_helper_process_pixel_exchange.rs:1644-1671`) merges with the readback
  checkout — same `open_cpu_readback_staging` op, shared per-pool-slot memo; the
  `writable`-probe-only round trip disappears into the arm that also maps.
- MODIFIED: `cpu()`'s docstring (`claimed_surface_pixel_access.py:303-330`) — two
  claims are now false ("publication is per store … there is no staging whose discard
  this could promise"; "Texture-backed pixels … are refused here") — plus
  `_report_the_first_read_only_cpu_door`'s cause list (`:70-90`), the
  `surface_can_take_write_back` stub doc (`_engine.pyi:400-409`), both
  `acquire_texture` stub docs ("not addressable here", `:380-388`, `:428-434`), and
  the surface-handle stub docs (`:672-703`). Stubtest-gated: no pyclass change is done
  until the stub matches.
- MODIFIED: `the_seam_publishes_a_staged_edit_through_the_try_direction`
  (`subprocess_escalate.rs:5604-5679`) is rewritten onto `RunCpuReadbackCopy` and
  renamed — its own doc says it is the only end-to-end proof that a staged edit lands
  in the pooled backing; deleting it with the `try_` op would cost the blocking
  publish path its only coverage. `an_unresolvable_surface_is_refused_by_name…`
  (`:5388`) drops its `TryRunCpuReadbackCopy` case.
- MODIFIED: `a_try_copy_reports_a_guard_refusal_as_an_error_and_never_as_contention`
  (`surface_export_staging.rs:2190-2223`) is rewritten onto blocking
  `refill_surface_export_staging` and renamed
  `a_refill_of_a_surface_this_staging_does_not_export_is_refused_by_name` — it is the
  only cover for `refuse_a_surface_this_staging_does_not_export`, a guard that
  outlives the `try_` surface, so deleting it with the methods would drop the guard's
  only proof. Same reasoning as the seam test above.
- MODIFIED: prose that names the deleted surface, same PR (factual records): the
  golden-vector macro doc (`escalate_wire_encoding_tests.rs:44-47`),
  `EscalateResponseOk::timeline_value`'s doc (`escalate_response.rs:143-147`),
  `SurfaceExportStagingCopyOp`'s "four wire ops" doc (`subprocess_escalate.rs:1526`),
  the module doc (`surface_export_staging.rs:47-51`), and
  `docs/architecture/adapter-runtime-integration.md:69`, `:322-328` — the last is
  inside the ship gate's sweep, so it is gate-required, not optional hygiene.
- MODIFIED: the wheel tests asserting the old refusals flip to positive coverage:
  `test_an_acquired_texture_is_a_name_not_a_local_mapping`
  (`test_compute_kernel.py:142-159` + `compute_kernel_probes.py:207-229` — the probe
  body inverts to reading pixels), the texture-arm assertion in
  `test_device_exchange.py:271-274` (+ `device_exchange_probes.py:502-509`), and the
  two claimed-surface docstrings whose per-store claims narrow to the pixel-buffer
  backing (`test_claimed_surface_pixel_access.py:778-793`, `:796` on).

## ADDED

- ADDED: opaque_fd_buffer_pool_host_cached
  The third pool on `HostVulkanDevice`, template `create_opaque_fd_buffer_pool`
  (`vulkan_device.rs:2248-2290`): same probe usage set (the pool probe's
  `memoryTypeBits` must match the real buffer's or the bind trips
  `VUID-vkBindBufferMemory-memory-01035`), alloc flags
  `DEDICATED_MEMORY | MAPPED | HOST_ACCESS_RANDOM`, required
  `HOST_VISIBLE | HOST_COHERENT`; after the probe, the returned index's flags are
  checked for `HOST_CACHED` and the field stays `None` without it. Joins the
  export-info `Box` lifetime rule, the `Drop` ordering (`:3560-3607`), and the
  prewarm sentinels — `make_opaque_fd_buffer_sentinel` (`:1846`) gains a HOST_ACCESS
  parameter (setting both bits is a VMA assert), the sentinel stays at the tiny size
  per `docs/learnings/nvidia-opaque-fd-after-swapchain.md`, and the ordered-label
  test `opaque_fd_export_sentinels_retained_for_each_supported_pool` (`:3857`) gains
  its entry.
- ADDED: new_opaque_fd_export_host_cached
  The `HostVulkanBuffer` constructor (name illustrative), paralleling
  `new_opaque_fd_export` (`vulkan_buffer.rs:432-497`) with the cached pool and the
  degrade-to-sequential-write fallback; non-null `mapped_ptr` check kept.
- ADDED: vma_allocation_memory_type_index
  On `HostVulkanBuffer`, mirroring `vulkan_texture.rs:1244` — what the staging
  registration publishes.
- ADDED: the wheel's readback-export arm — a mapped sibling of `HelperDeviceExport`
  (`python_helper_process_pixel_exchange.rs:863-872`) holding a
  `ConsumerVulkanBuffer` where the CUDA arm holds `cuda_import`, its own
  per-pool-slot memo beside `device_exports_by_surface` (`:1014-1015`), checked out
  through the existing two-fd order (`[staging_fd, refill_done_fd]`, `:1737-1745`)
  and driven through `run_device_export_copy` (`:1681-1708`), which is already
  parameterised by op name.
- ADDED: engine tests — a cached-pool export round trip asserting the allocation's
  memory type is `HOST_CACHED` or the pool is absent (template
  `vulkan_buffer.rs:1134`); a no-GPU fallback-derivation test in the shape of
  `a_device_without_the_opaque_fd_pool_falls_back_to_not_importable`
  (`subprocess_escalate.rs:4948`); the three residency-level regression tests
  (`surface_export_staging.rs:2017`, `:2059`, `:2106`) pass unchanged, the last
  additionally asserting the staging's memory type is the cached pool's when the
  device has one.
- ADDED: wheel tests — a texture-backed CPU read of a kernel output and a LUT
  fill-and-read-back probe. Shipped in the compute-kernel helper-child harness
  (`compute_kernel_probes.py`) rather than beside the pixel-buffer probe in
  `cast_claim_probes.py`: the cast-claim app wires a *native* source, so a cast object
  over a texture backing needs a texture-publishing upstream that harness has no
  processor for. The cast object's `cpu()` is backing-agnostic Python over
  `resolve_surface` + `lock` + `as_numpy`, which `device_exchange_probes.py` exercises
  on a kernel output; a dedicated cast-object probe is left as harness work.
  Additionally: a staging-backed discard-on-raise twin of
  the existing coherent-arm raise test; an implied-usage assertion that
  `acquire_texture(usage=["texture_binding"])` yields a surface whose
  `surface_can_take_write_back` answers true. GPU-marked tests run on the rig only;
  the Rust seam tests above are what gate a PR.

## Notes (not tickets)

- Every GPU escalate op is Linux-only today and stays so; the platform floor is
  unchanged.
- The `import_single_plane_with_handle_type` first-match guess also underlies today's
  working paths; only the new readback import gets the stated-index bind here.
  Migrating the existing paths onto it is engine hygiene outside this change's scope —
  a note, not a ticket, per the only-file-P0 rule.
- `packages/escalate/schemas` does not exist — the hand-written serde structs plus
  the golden-vector tests are the entire wire contract; prose citing a schema
  directory would be wrong.
