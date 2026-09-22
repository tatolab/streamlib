# portable-gpu-interop

One portable path for GPU work from Python, on both floors. The owner decided the shape
2026-09-22 (`[portable-gpu-interop]` in `ARCHITECTURE.md` §Packages and §Graphics; ADR
`docs/decisions/portable-gpu-interop.md`; evidence
`docs/research/2026-09-22-portable-gpu-interop-for-python-processors.md`). This delta builds it:
an engine surface-to-surface copy Python can call, the host-side DLPack request honoured on the
macOS floor as it already is on Linux, the cross-floor check, and the GPU examples moved onto the
portable path. It rides milestone 53, *Full feature parity on Apple Silicon*, beside
`macos-capability-parity`. The engine work never waits on an example.

**Scale gate — this skill, plus an ADR (already landed).** It adds a public Python method on the
Limited and Full GPU capabilities, a new escalate op on the IPC wire, and an RHI record method.
The rationale is `docs/decisions/portable-gpu-interop.md`; no second ADR is owed.

**Precondition.** Every entry this delta touches is DECIDED: the four `[portable-gpu-interop]`
entries (`ARCHITECTURE.md:331-352`, `:1164-1171`), the cast-object tensor protocol (`:281-330`),
the kernel output and write-back contract (`:1001-1029`), "dispatch is synchronous" (`:1144`),
and §Consumers' converted-consumer rule (`:453`). `macos-capability-parity` stays active
beside this delta, and its macOS arms are prerequisites here, never duplicated.

**Verified against the tree, 2026-09-22.**

- No `copy_surface_to_surface` exists (`git grep` finds it only in docs). The copy
  primitives by backing pair:
  - buffer→buffer: `GpuContext::blit_copy` (`gpu_context.rs:2764` → `vulkan_blitter.rs:52`),
    which already builds on macOS (`gpu_context.rs:1052`), and
    `RhiCommandRecorder::record_copy_buffer_to_buffer` (`vulkan_command_recorder.rs:470`).
  - buffer→texture: `record_copy_buffer_to_image` (`:438`) and `copy_pixel_buffer_to_texture`
    (`gpu_context.rs:1625`, cfg(linux); it transitions from UNDEFINED, so the copy is a whole
    overwrite).
  - texture→buffer: `record_copy_image_to_buffer` (`:404`).
  - **texture→texture: no usable primitive.** `CommandBuffer::copy_texture`
    (`core/rhi/command_buffer.rs:74` → `vulkan_command_buffer.rs:44-148`) barriers the source
    from UNDEFINED, copies the `min()` of both extents, and returns silently on a missing
    image. The recorder has no image→image method.
- A new escalate op touches `escalate_request.rs` (variant + `deny_unknown_fields` struct),
  `escalate_wire_encoding_tests.rs`, `subprocess_escalate.rs` (`request_id` arm `:121-148`, a
  dispatch arm plus its cfg refusal twin, the handler, and a parent test), the helper client
  (`python_helper_process_pixel_exchange.rs`), the two `#[pymethods]` blocks
  (`python_processor_context.rs:1079`, `:1236`), and `_engine.pyi` (`:1050`, `:1097`). The
  closest model is `run_cpu_readback_copy` (`escalate_request.rs:1538`, handler
  `subprocess_escalate.rs:1672-1706`). The reply needs no new type: `EscalateResponseOk`
  already carries `handle_id` and `timeline_value`.
- Ordering today: every escalate GPU op records, submits and waits on the host before it
  replies (kernels `subprocess_escalate.rs:2330-2374`; write-backs `submit_staging_copy_and_wait`,
  `surface_export_staging.rs:682-727`). A pooled frame on Linux has no per-surface destination
  timeline. Acquired textures carry a `produce_done` / `consume_done` pair that escalate
  consumers do not drive (`subprocess_escalate.rs:1398-1451`).
- Whether a surface takes a write-back is derived on the parent by `SurfaceExportStaging::writable()`
  (`surface_export_staging.rs:286-295`) and re-checked live by `resolve_write_back_destination`
  (`:1005-1085`). The wheel learns it only through `open_cpu_readback_staging`
  (`python_helper_process_pixel_exchange.rs:1850`), which mints a staging.
- Any-backing resolution exists: `resolve_device_export_source` → `ResolvedBlitSource`
  (`surface_export_staging.rs:414-440`), plus the layout save and restore of `record_write_back`
  (`:817`, `:1088-1150`).
- DLPack: the surface handle's Linux arm honours `dl_device` (`python_processor_context.rs:567-599`);
  its macOS arm discards it (`:600-604`, `let _ = dl_device;`), which is right today only
  because macOS has no device side yet. `copy=True` is refused before the platform split
  (`:552-556`, and the scope at `:822-826`). The one test of the explicit host request is
  `requires_gpu` and CUDA-shaped (`test_device_exchange.py:145`, probe
  `device_exchange_probes.py:189-198`), so nothing asserts it on macOS. No test asserts the
  `copy=True` refusal.
- `run` and `dev` share `launch_app_node` (`cli.py:215-280`). The app anchor directory and
  entry file are known at `:229-230`, before anything executes. Processor code is not always
  under `processors/`: `camera-python-effects` keeps it in `src/camera_python_effects/processors/`.
  No test walks the wheel's own Python; `tests/test_platform_markers.py` is the closest
  precedent, a closed list held by a test.
- `__cuda_array_interface__` exists nowhere in the wheel. The closed list's floor-shaped names
  as the stub spells them: `VirtualCameraSink` (`_engine.pyi:425`), `create_ray_tracing_kernel`
  (`:1209`), `build_triangles_blas` (`:1235`), `build_tlas` (`:1247`), `export_dma_buf`
  (`:1276`), `export_opaque_fd` (`:1291`), `import_dma_buf` (`:1315`).
- Examples:
  - **Only for the landing copy:** `camera-compute-kernel` (`grayscale_compute.py:22`, `:164`),
    `camera-halftone` (`halftone_compute.py:30`, `:236`) and `camera-virtual-camera`
    (`shader_effect.py:23`, `:129`) each import cupy for one line,
    `cupy.from_dlpack(texture)[...] = cupy.from_dlpack(frame)`, and pin `cupy-cuda13x>=14.2` in
    `pyproject.toml:10`.
  - **camera-python-effects** also uses `cupy.asnumpy` for host-side decimation
    (`cyberpunk_avatar.py:198-199`, `lerobot_recorder.py:113-114`, beside
    `camera_frame_to_texture.py:59-65`). It pins skia-python, mediapipe and moderngl with no
    platform markers, and none of them has been measured on macOS arm64.
  - **fisheye-object-detection** spells the device twice (`undistorting_object_detector.py:214`,
    `:216`) and pins `torch>=2.6`, below the Metal capsule's floor.
  - `camera-virtual-camera`'s app stays Linux-only whatever happens: `VirtualCameraSink` is on
    the closed list.
- The torch floor "≥ 2.12" appears only in `macos-capability-parity.md:90` and #2404's body.
  No refusal message and no stub states it. The source floor is 2.9 for the `kDLMetal` mapping
  and 2.10 for the sliced-tensor fix; 2.14 is what was measured (memo §5).

---

## §Graphics — RHI / GPU

- ADDED: an RHI image→image copy record method on `RhiCommandRecorder`, beside the three copy
  records it already has. Same format and extent, whole-image, and every layout is barriered
  from the image's *known* layout. It never uses UNDEFINED on a source, and it refuses rather
  than returning silently when an image is missing. This is the one backing pair with no
  primitive, and the copy cannot be built without it.
- ADDED: `copy_surface_to_surface(source_surface_id, destination_surface)` on
  `GpuContextLimitedAccess` and `GpuContextFullAccess`, as a new escalate op
  (`copy_surface_to_surface`) from a helper process.
  - **The engine chooses the copy:** it resolves both surfaces through the any-backing
    resolver and picks buffer→buffer, buffer→image, image→buffer or image→image from the two
    backings.
  - **Refused by name:** a format or extent mismatch, a retired frame generation (the existing
    `refuse_a_retired_frame_id`), and a destination that cannot take a write-back.
  - **The write-back answer comes from `writable()`'s derivation on the parent,** not from
    minting a staging.
  - **The source is held for the whole copy** by the parent's resolved `Arc` across record and
    wait.
  - **A registered texture's settled layout is republished,** as a dispatch does.
- MODIFIED: the §Graphics entry's ordering clause becomes "ordered ahead of the
  destination's next read" (decision 1, resolved A). The copy records, submits and waits on
  the host before its reply, the `submit_staging_copy_and_wait` shape without the staging,
  as dispatch and every other escalate GPU op do.
- ADDED: the macOS arm of the copy op rides the parity chain and adds nothing of its own. It
  runs once #2403 un-gates the escalate GPU ops. Its IOSurface-backed textures are #2402's.
  Its ordering is whatever decision 1 settles, carried on macOS by the shared-event timeline
  the parity delta already decides (#2401).
- ADDED: `_engine.pyi` states the method on both capability classes, so `stubtest` gates it
  on both lanes.

## §Packages & extension model

- MODIFIED: the surface handle's macOS `__dlpack__` arm honours `dl_device=(kDLCPU, 0)`.
  `wants_host` becomes platform-neutral, so once #2404 gives macOS a Metal natural side, the
  host request still yields the host mapping (the same IOSurface pages). This lands inside
  #2404's scope. Its body's "as today" is wrong for macOS: today the arm discards the request.
  Acceptance: `numpy.from_dlpack(frame, device="cpu")` equals `as_numpy()`, as one test that is
  green on both lanes, plus a test that `copy=True` is refused at both doors, which nothing
  asserts today.
- ADDED: the cross-floor check, as a wheel-internal Python module with no CLI verb.
  - **What it reads:** every `.py` under the app anchor directory, excluding virtual
    environments, which covers both example layouts. It parses them with stdlib `ast`, and
    reads `pyproject.toml` with stdlib `tomllib`. The wheel's 3.10 floor predates `tomllib`,
    so on 3.10 the dependency rule is skipped and the warning block says so; no parser
    dependency is added.
  - **Floor-bound imports:** cupy, pycuda, `numba.cuda`, `torch.cuda`, and `mlx` outside a
    `sys.platform` guard.
  - **Device literals:** `"cuda"` or `"mps"` passed as a device, and `.cuda()`.
  - **Closed-list names:** exactly the stub's list above.
  - **Dependencies without a platform marker:** a floor-bound distribution (`cupy-*`, `mlx`)
    with no `sys_platform` marker.
  - **Each finding** names the file and line and the portable spelling: the engine copy, the
    tensor's own device, or `torch.accelerator`.
- ADDED: `launch_app_node` runs the check between resolving the entry file and executing it,
  and prints the findings as a warning block. It never blocks and never adds latency beyond a
  parse. The findings are plain text on stdout, which is what an agent driving `streamlib dev`
  reads.
- ADDED: CI gates on the check over the wheel's own Python and over the files
  `scaffold_new_app` emits, as a pytest beside the existing scaffold tests (`test_cli.py:435-541`).
  A clean scaffold is asserted, never assumed.
- MODIFIED: the torch floor for the Metal capsule. It becomes "torch ≥ 2.10 by source, measured
  on 2.14; MLX ≥ 0.32" where the parity delta states it (`macos-capability-parity.md:90`) and in
  #2404's body. The number the capsule's refusal names follows. Recorded as a fact: the 2.12
  had no measurement behind it.

## §Consumers — examples

Conversions under §Consumers' converted-consumer rule. They are sequenced last, blocked only by
the Linux arm of the engine copy, and they never block an engine ticket. Proof that they
landed is the cross-floor check running clean over each example at ship time. The removal gate
cannot prove it, because it excludes `examples/`.

- MODIFIED: `camera-compute-kernel`, `camera-halftone` and `camera-virtual-camera` swap the
  cupy line for `copy_surface_to_surface` and drop `cupy-cuda13x` from their dependencies.
  `camera-virtual-camera`'s processors go portable, but its app stays Linux-only because of
  `VirtualCameraSink`, and its README says so.
- MODIFIED: `fisheye-object-detection` takes its device from `torch.accelerator` and pins
  `torch>=2.10`. `ultralytics` on MPS is unmeasured, and the conversion measures it.
- MODIFIED: `camera-python-effects` swaps its landing copy for the engine copy, and its two
  decimations for `numpy.from_dlpack(frame, device="cpu")` sliced on the host. Its other
  dependencies are measured on macOS arm64 as part of the conversion. A dependency with no
  macOS wheel is marked with `sys_platform`, and the README records which processors that
  leaves Linux-only. This is the conversion most likely to slip. If it does, `/ship-change`
  records the fact against the parity delta's "the examples use only the portable surface"
  line and does not hold the change.

## Removals

- REMOVED: let _ = dl_device;
  The macOS surface-handle arm that discards the host request. It is replaced by the
  platform-neutral `wants_host`.

The examples' cupy removals are proven by the cross-floor check at ship time, not by this
gate, which never searches `examples/`.

---

## Decision 1 — How the copy is ordered (resolved by the owner, 2026-09-22)

The DECIDED §Graphics entry says the engine "orders it on the destination's timeline ahead of
its next read". The tree has no such timeline for a pooled frame on Linux. Every escalate GPU
op today, write-backs included, is ordered by one queue plus a host wait before the reply.
The contract ("ahead of the next read") is decided. The mechanism is not.

**A — Synchronous, like dispatch.** Record, submit, and wait on the host before
the reply, the `submit_staging_copy_and_wait` shape without the staging. It matches "dispatch
is synchronous" (`ARCHITECTURE.md:1144`) and every existing escalate op, and needs no new
timeline. On macOS the same wait sits on the shared-event timeline #2401 brings. Cost: one
host round trip per copy. The kernel dispatch that follows already pays one, so a
copy-then-dispatch pays two. The entry's wording becomes "ordered ahead of the destination's
next read".

**B — Signalled on a timeline, no host wait.** Signal a per-surface or per-texture
`produce_done` and let the next dispatch wait on it GPU-side. It saves the host round trip,
but it needs timelines on pooled frames that Linux does not have, and ordering that no other
escalate op uses. That is a second ordering model, which is the parallel-system shape the
doctrine rejects.

**C — Fold the copy into the next dispatch.** A binding means "copy this surface in first",
so there is one submission and no extra op. It changes the dispatch API and couples two
concerns.

**RESOLVED — A.** If the extra round trip ever shows up in a measured budget, B becomes its
own change.

## Not in scope

The starter effect's default and a shader-body effect helper; model-input preprocessing as an
engine step; buffer bindings from Python kernels (#516's gap). Those go to the next `/align`
round, after a design pass. A ruff courtesy config in the scaffold. Array-API wrappers in any
example.
