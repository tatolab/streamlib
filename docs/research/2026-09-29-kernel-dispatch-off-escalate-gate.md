# Research memo: may per-frame kernel dispatch leave the escalate scope?

2026-09-29, for issue #2505 (follow-up to #2503 / PR #2504, and #2430). Read-only research
against `main` at `5d93b5c1`. No code was run. The measurements quoted come from the Linux rig
review on PR #2504 (RTX 3090, NVIDIA 595.91.07).

Evidence is tagged **[V] verified** (source line or spec URL read), **[M] measured by someone
else** (source cited) and **[I] inferred** (reasoning, not observed).

## Question

A Python helper's `run_compute_kernel`, `run_compute_kernel_batch`, `run_graphics_draw` and
`run_ray_tracing_kernel` each run inside `GpuContextLimitedAccess::escalate`. That means the
runtime-wide escalate gate plus a `vkDeviceWaitIdle` that takes every queue mutex. May any of
them leave that scope? If so, which kinds, what replaces each job the scope does today, and is
it worth the risk?

## Short answer

- **Yes in principle, and it is not needed now.** The device-idle wait does no job for dispatch
  that the dispatch's own fence wait does not already do, with one exception: it hides a
  graphics-draw write-after-read gap. The gate does one real job, keeping a shared graphics or
  ray-tracing kernel's single command buffer, fence and staged bindings from interleaving.
  Compute already has its own lock for that.
- **Harm is not measured, only rate.** The rig measured 5–6 escalates/s at 5 fps and 63/s at
  60 fps. Nobody has measured a dropped frame, added latency or lost throughput. The one
  60 fps producer held 60 fps.
- **Verdict: not worth moving dispatch now.** Measure first. The trace fields for it already
  exist. If the idle time turns out to matter, the smallest change is option B: keep the gate
  and drop only the idle for dispatch.
- **One thing is worth doing regardless:** widen the graphics colour-target barrier. Reading
  the code, #2504 has already turned the old guarantee into a timing window.

## Evidence

### Where the scope lives and what it does

- [V] `escalate_in_process` (`runtime/streamlib-engine/src/core/context/gpu_context.rs:3731-3770`):
  - enters the gate (`:3737`);
  - runs the closure;
  - then calls `wait_device_idle()` (`:3747`), with the gate still held until the function
    returns.

  The idle runs **after** the closure. It orders the *next* escalate against everything
  submitted before this one ended. It orders nothing inside this one.
- [V] `HostVulkanDevice::wait_idle` (`runtime/streamlib-engine/src/vulkan/rhi/vulkan_device.rs:3197-3226`)
  takes the graphics, transfer, compute, video-encode, video-decode and device mutexes, then calls
  `vkDeviceWaitIdle`. While it waits, no thread in the app can submit to any queue: not the
  camera, the display, the codecs or another helper.
- [V] The gate (`runtime/streamlib-engine/src/core/context/escalate_gate.rs:53-152`) is a
  flag and a condvar. It serialises escalate scopes against each other only. In-process
  built-ins' per-frame submits never enter it. So the gate never orders a helper's dispatch
  against the display, camera or codecs; only the idle's drain does, and only as timing.
- [V] Escalate ops dispatch from `subprocess_escalate/mod.rs:112-236`.
  - Under the scope: compute run (`compute/linux_and_macos.rs:207`), compute batch (`:486`),
    graphics draw (`graphics/linux_and_macos.rs:342`), ray-tracing trace
    (`ray_tracing/linux_and_macos.rs:535`), all `register_*` ops, and fresh-slot growth
    (`acquisition/mod.rs:324`).
  - Already off it, and per-frame:
    - reused ring slot hand-off (#2504, `acquisition/mod.rs:308-318`);
    - `show_surface_on_processor_owned_window` (`processor_owned_window/mod.rs:136-140`);
    - **`copy_surface_to_surface`** (`surface_copy/linux_and_macos.rs:25`, calling
      `GpuContextLimitedAccess::copy_surface_to_surface`,
      `core/context/surface_to_surface_copy.rs:273-313`).
- [V] The copy is the precedent that matters. It is a helper-driven, per-frame **GPU write**
  that already runs with no gate and no idle (#2491). It is serialised by its own recorder lock,
  held from record through `wait_for_completion`, and ordered by all-commands barriers. The
  "can a per-frame GPU op leave the scope" question was answered once already, for copy, and
  nobody filed it as a risk.

### Every dispatch already waits for its own work

Each dispatch path fence-waits its own submission before it returns:

- [V] Compute: `dispatch_compute_kernel_batch` takes `batched_compute_dispatch_recorder`'s
  mutex, then records, submits (`gpu_context.rs:3326`) and waits (`:3351`) under that one guard.
- [V] Graphics: `VulkanGraphicsKernel::offscreen_render` submits, then calls `wait_for_fences`
  (`runtime/streamlib-engine/src/vulkan/rhi/vulkan_graphics_kernel.rs:726-745`).
- [V] Ray tracing: `trace_rays` submits, then calls `wait_for_fences`
  (`runtime/streamlib-engine/src/vulkan/rhi/vulkan_ray_tracing_kernel.rs:618-632`).
- [V] Spec:
  - `vkDeviceWaitIdle` "is equivalent to calling `vkQueueWaitIdle` for all queues owned by
    `device`" (https://docs.vulkan.org/refpages/latest/refpages/source/vkDeviceWaitIdle.html).
  - `vkQueueWaitIdle` "is equivalent to having submitted a valid fence to every previously
    executed queue submission command … then waiting for all of those fences"
    (https://docs.vulkan.org/refpages/latest/refpages/source/vkQueueWaitIdle.html).

  So the idle gives the dispatch's own work nothing its fence wait did not already give. What
  the idle adds is a wait on *other* work.

### #2430's torch ordering needs only the fence

- [V] The stub states the contract as "Returns when the GPU work has retired and the writes
  are visible — a tensor the dispatch wrote reads back through `torch.from_dlpack`"
  (`sdk/streamlib-python-wheel/python/streamlib/_engine.pyi:1922-1923`, and the same at
  `:2048`, `:2077`, `:2108`).
- [I] The helper launches its torch or CUDA work only after it receives the reply. The reply is
  written only after the host has waited the dispatch's fence. That host-side order is the whole
  ordering #2430 relies on. A device-idle wait adds nothing for the dispatch's own writes. Any
  option below that keeps the per-dispatch fence wait keeps #2430.
- [V] The decided rationale for synchronous dispatch (`docs/decisions/python-kernel-api.md:128-135`)
  assumes that "a helper blocking its own thread costs nothing another processor can observe".
  The idle wait and the gate make that assumption false today. Fixing this would restore the
  decision's premise, not contradict it.

### Claim 1: the idle as an unnamed fence for the display

- [V] The display lets go of a frame when its compose is submitted, not when the GPU finishes
  reading it. `show_named_surface` drops the registration when `render_frame` returns after
  submit (`runtime/streamlib-engine/src/core/processor_owned_window.rs:283-297`). The present
  target waits only on frame N−2's timeline (`runtime/streamlib-engine/src/vulkan/rhi/vulkan_present_target.rs:540-561`).
- [V] The graphics draw's colour-target barrier uses `srcStage NONE`, `srcAccess NONE` and
  `oldLayout UNDEFINED` (`vulkan_graphics_kernel.rs:626-651`). Spec: `NONE` "specifies no stages
  of execution", so the barrier makes no dependency on the earlier compose. With `UNDEFINED`,
  "the contents of that range may be discarded"
  (https://docs.vulkan.org/spec/latest/chapters/synchronization.html).
- [V] Compute's first touch of an image uses `ALL_COMMANDS` / `MEMORY_WRITE`, and records it
  unconditionally, "even when the layout already matches" (`gpu_context.rs:3396-3404`,
  `:3451-3462`).
- [V] Ray tracing, and graphics' bound inputs, take an `ALL_COMMANDS` barrier **only when the
  tracked layout differs** (`subprocess_escalate/surface_bound_kernel_binding.rs:368`,
  `:418-446`). A ray-tracing write into a slot the display composed still gets one: the
  compositor leaves the slot's registration at `SHADER_READ_ONLY_OPTIMAL`
  (`vulkan_present_compositor.rs:238-251`), and a storage image needs `GENERAL`. That protection
  comes from layout bookkeeping, not from the barrier being unconditional.
- [V] Everything here submits to the one main queue:
  - recorder (`vulkan_command_recorder.rs:204`);
  - graphics kernel (`vulkan_graphics_kernel.rs:223`);
  - ray-tracing kernel (`vulkan_ray_tracing_kernel.rs:141`);
  - present (`vulkan_present_target.rs:671`).

  So a source-stage `ALL_COMMANDS` barrier does order a write after every earlier compose in
  submission order. No semaphore is needed.
- [I] **#2504 already turned claim 1 into a timing window for graphics.**
  - Before #2504, the reuse hand-off was itself an escalate, so an idle ran *between* "the slot
    reads as free" and the draw. That was airtight.
  - Now the idle that protects draw N+2 into slot A is the one at the end of the *previous*
    escalate. It covers compose-of-A only if that compose was submitted before that idle.
  - A display running more than one producer frame behind can submit compose-of-A after that
    idle. It then drops A. The next draw into A has no dependency on that compose.
  - The rig's 0-hazard sync-validation runs [M] (60 s per topology, review 5328390281) mean the
    interleaving did not happen in those runs. They do not show it cannot happen. Widening the
    barrier closes it whatever is decided on #2505.

### Claim 2: bindings on a shared kernel

- [V] Kernels are cached runtime-wide by content, and "a kernel survives its registering
  helper" (`gpu_context.rs:819-827`, `:855-865`). Two helpers running the same shader get the
  same kernel object.
- [V] Graphics:
  - Staged bindings live in `pending: Mutex<Vec<PendingState>>` (`vulkan_graphics_kernel.rs:87-89`).
  - `offscreen_scaffold` hands back one command buffer and one fence per kernel, and drops its
    lock at once (`:1013-1020`).
  - `offscreen_render` then waits that fence, resets it and re-records the buffer with no lock
    held (`:583-600`).

  Two concurrent draws on one kernel would reset a command buffer that is still pending and
  cross their bindings. The handler says so: "interleaving is prevented by the escalate gate"
  (`graphics/linux_and_macos.rs:371-374`).
- [V] Ray tracing has the same shape: one `descriptor_set`, one `command_buffer`, one `fence`
  and one `pending` per kernel (`vulkan_ray_tracing_kernel.rs:60-69`).
- [V] Compute does **not** depend on the gate for this. `write_into_kernel`, the barriers,
  `set_push_constants`, submit and wait all run under the recorder mutex
  (`gpu_context.rs:3308-3351`, `:3467`). The field comment (`:843-845`) credits the gate, but
  the `parking_lot::Mutex` already enforces serial use.
- [V] The direction that would retire claim 2 at its root is already decided: bindings supplied
  at dispatch rather than stashed on the kernel ("Rust converges",
  `docs/decisions/python-kernel-api.md:137-145`). It is sequenced separately.

### What the gate and the idle were built for

- [V] Commit `ee4c9afd` (#304) fixed a flaky `DEVICE_LOST` on NVIDIA. One processor's `setup()`
  (H.265 video session, DPB, swapchain creation) raced another processor's concurrent Vulkan
  work. The fix serialised `setup()` and idled the device afterwards, "so every downstream
  processor sees a fully-quiesced device". **It is a resource-creation fix. Dispatch was never
  its subject.** Dispatch inherited the scope because every op a helper sends became an
  escalate.
- [V] #1207 is still open. It describes a latent NVIDIA crash of the same class, where the
  crash site "floats across pipeline-create / command-pool / `wait_idle`".
- [V] The dispatch paths still do some per-frame creation:
  - Graphics and ray tracing build a **fresh command recorder** (pool, buffer, fence) on every
    run that needs an input transition (`surface_bound_kernel_binding.rs:390`, `submit_and_wait`
    at `:401`).
  - A binding whose surface is not in the parent's texture cache re-imports a `VkImage` on every
    call and submits a queue-family acquire (`gpu_context.rs:1356-1400`, Path 2).

  Compute reuses its recorder, so compute creates nothing per frame except the Path-2 import.

## The scope's jobs, one by one

1. **Serialise resource creation against other creation** (#304 and #1207).
   - Load-bearing for `register_*` and for growth. **Not load-bearing for dispatch**, except
     for the per-frame recorder creation (graphics and ray tracing) and the Path-2 import.
   - Narrowest replacement: make dispatch creation-free by reusing one recorder per family, as
     compute already does. Leave the rare Path-2 import where it is.
2. **Drain the device after creation**, so no thread submits against half-built driver state
   (#304). Not load-bearing for dispatch; see job 1.
3. **Order a graphics draw after an in-flight compose of the same slot** (claim 1).
   - Load-bearing today for graphics only, and only as timing since #2504.
   - Narrowest replacement: give the colour-target barrier the same `ALL_COMMANDS` source scope
     as every other write-entry barrier. `UNDEFINED` can stay as the old layout, because the draw
     clears the slot anyway.
   - The alternative is for the display to hold the frame until its frame timeline signals. That
     is a larger change on a path every native producer shares.
4. **Keep bindings, the command buffer and the fence on a shared graphics or ray-tracing kernel
   from interleaving** (claim 2).
   - Load-bearing for graphics and ray tracing. Not for compute.
   - Narrowest replacement: a per-kernel mutex held from the first `set_*` through the fence
     wait. The structural fix is the decided bindings-at-dispatch convergence.
5. **Order Vulkan before torch** (#2430). Not load-bearing on the idle; the per-dispatch fence
   wait does it.
6. **Serialise updates to a texture's registration layout cell across helpers.**
   - Partially load-bearing. It holds between escalate families today, but not against copy,
     which is already off the gate (PR #2491 notes this), and not against the in-process display.
   - Pre-existing and already open. Taking dispatch off the gate would widen it.

## Cost: what is measured and what is not

- [M] Rate:
  - `camera-compute-kernel` at 5 fps: 5–6 escalates/s;
  - pure graphics producer at 60 fps: 63/s (PR #2504 review 5328390281).

  That is about one escalate per dispatch per frame.
- [M] Idle on a quiet device: `wait_idle_ns=21551` (about 22 µs) in PR #912's smoke run. That
  was a test fixture with nothing else in flight.
- **Not measured:** idle time under load, gate wait time, dropped or late display frames,
  camera or codec submit stalls, end-to-end latency. The 60 fps graphics producer held 60 fps,
  which is weak evidence of no visible harm with one producer.
- [I] How it scales. Each dispatch holds the gate for its closure: CPU record time plus the
  kernel's own GPU time plus the idle. The idle blocks every queue for however long the device's
  in-flight tail runs.
  - At 30 fps, 4 helpers × 1–2 dispatches per frame is 120–240 escalates/s.
  - With sub-millisecond kernels and a quiet device, gate occupancy stays in the low tens of
    percent and the per-frame cost is well under a millisecond. Plausibly invisible.
  - The cost climbs in three situations:
    - **Heavy kernels.** 3 ray-tracing helpers at 8 ms each × 30 fps is about 72 % gate
      occupancy. Helpers can never overlap their GPU work, and queueing delay grows fast.
    - **An in-flight video encode or decode.** Every dispatch then waits out the codec's
      submission, typically milliseconds (estimate), while holding every queue mutex.
    - **A display near its vsync deadline.** Its present submit can block behind an idle.

  These are estimates. None is measured.
- [V] The measurement hooks exist. `escalate_in_process` already emits `mutex_wait_ns`,
  `closure_duration_ns` and `wait_idle_ns` at trace level on target
  `streamlib::gpu_context::escalate` (`gpu_context.rs:3750-3758`).

## Options, ranked by risk

**A. Status quo.**
- Zero new risk.
- Leaves the claim-1 timing window #2504 opened. The one-line barrier fix, below, closes it
  without touching the scope.
- Cost: whatever the unmeasured stall is.

**Barrier fix (independent of A–D; recommended now).**
- Change `vulkan_graphics_kernel.rs:635-636` from `NONE` / `NONE` to `ALL_COMMANDS` /
  `MEMORY_READ | MEMORY_WRITE`.
- What could break: nothing functional. The GPU-side wait on earlier queue work matches what
  compute, ray tracing and copy already do. The draw may start a few microseconds later behind
  other queue work.
- How to catch a problem: the existing sync-validation A/B (`VALIDATE_SYNC=true` with the
  duplicate-message limit lifted) on the `RasterizedSceneRenderer → DisplayWindow` topology,
  plus the graphics kernel tests.

**B. Keep the gate; drop only the device-idle wait for the four dispatch ops.**
- Needs a scope variant without the idle. The `register_*` ops and growth keep the full scope.
- Preconditions:
  - the barrier fix;
  - graphics and ray tracing reuse one input-transition recorder per kernel or family instead
    of creating one per run (job 1);
  - optionally, ray-tracing storage-image writes barrier unconditionally, as compute does.
- #2430: preserved, because the fence waits stay.
- Claim 2: preserved, because the gate stays.
- What could break:
  - A latent NVIDIA creation-vs-work race of the #304 or #1207 kind, if any per-frame creation
    is left in a dispatch path. This is the hard-to-find class; the precondition exists to
    remove it.
  - A consumer that silently relied on the drain.
- How to catch it:
  - sync validation on the camera → kernel → display topologies;
  - the #304 codec round-trip loop (20× h265/vivid);
  - a mixed codec plus several helpers soak.
- Win: removes the whole-app stall on camera, display and codecs, which is the part that hits
  processors other than the caller. Helpers still serialise among themselves.

**C. Compute only, off both gate and idle.**
- Compute already has what the gate would give it: its own lock through the fence wait,
  unconditional `ALL_COMMANDS` barriers and no per-frame recorder creation.
- What could break:
  - The layout-cell race (job 6) against graphics, ray tracing, copy and the display on a shared
    image.
  - The Path-2 import runs unserialised.
- Win: slightly larger than B for compute, since compute stops waiting behind graphics, ray
  tracing and allocations. It still does not let compute helpers overlap each other.
- Risk is low, but the extra win over B-for-compute is small. Not preferred.

**D. Everything off the gate.**
- Needs:
  - per-kernel locks for graphics and ray tracing;
  - the barrier fix;
  - a display hold or the timeline;
  - creation-free dispatch;
  - a serialised layout cell.
- Biggest win: helpers overlap on the GPU.
- The most new synchronisation, spread across the most code, and mostly overtaken by the decided
  bindings-at-dispatch convergence. Not now.

## What remains unknown

- The real `wait_idle_ns` and `mutex_wait_ns` distribution with several Python processors,
  a codec and a display at 30 fps. **This is the number the decision hangs on.**
- Whether the display misses vsync, or camera submits stall, because of escalate idles. No
  frame-pacing measurement exists.
- Whether any per-frame dispatch path hits Path 2 (per-call import) in practice. Reading
  suggests not, for helper-acquired and in-process textures.
- macOS (MoltenVK): the same code paths, but the idle cost there is unmeasured.
- The claim-1 window is argued from code, not reproduced. A deliberately slowed display
  (a sleep before compose in a test build) under sync validation would confirm or refute it.

## Recommendation for `/align`

1. **Do not move dispatch now.** The measured fact is a rate. The harm is an estimate, and the
   failure mode of getting it wrong (GPU races, a latent NVIDIA crash) is the expensive kind.
2. **Fix the graphics colour-target barrier now**, as an engine hardening ticket, whatever is
   decided here. It is a one-line change to match every other write-entry barrier, and it
   closes the gap #2504 already opened.
3. **Measure before revisiting.** Run one rig session with 3–4 Python processors plus camera,
   display and a codec at 30 fps, with `RUST_LOG=streamlib::gpu_context::escalate=trace`, and
   record:
   - the p50/p99 of `wait_idle_ns` and `mutex_wait_ns`;
   - the display's frame pacing.

   Suggested trigger for acting: the p99 idle plus gate wait exceeds about 10 % of the frame
   budget, or the display misses vsync because of it.
4. **If the trigger fires, take option B** with its two preconditions (creation-free
   graphics/ray-tracing dispatch, and the barrier fix). Leave the gate and the shared-kernel
   question to the bindings-at-dispatch convergence that is already decided.

## Outcome

- #2505 closed as not planned; dispatch stays in the escalate scope until item 3's measurement
  says otherwise.
- Item 2 shipped as #2546. Sync validation on an M1 Max reported the write-after-read in a
  real graphics producer → `DisplayWindow` run in 3 of 3 runs without the fix and 0 of 3 with
  it; the evidence is on the issue.
