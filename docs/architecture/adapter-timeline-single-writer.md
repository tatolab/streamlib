# Single-writer-per-edge surface-adapter timelines

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).
> Verify against current code before generalizing.

## What this is

Every surface registered through a streamlib subprocess-wired surface
adapter (`streamlib-adapter-cuda`, `streamlib-adapter-vulkan`,
`streamlib-adapter-cpu-readback`) carries **two Vulkan timeline
semaphores**, one per direction of the producer ↔ consumer edge:

| Timeline | Single writer | Read by |
|---|---|---|
| `produce_done` | producer process | consumer process (waits before reading) |
| `consume_done` | consumer process | producer process (waits before re-writing) |

**Single-writer per timeline** is the load-bearing rule. The process
that owns a timeline is the only process that ever issues a
`vkSignalSemaphore` or `vkQueueSubmit2::pSignalSemaphoreInfos` against
it. The other process only ever waits (`vkSemaphoreWaitInfo` /
`vkQueueSubmit2::pWaitSemaphoreInfos`).

With one writer per timeline, the next-value computation is a pure
function of per-process state. No cross-process coordination is
required for monotonicity, and `VUID-VkSemaphoreSignalInfo-value-03258`
("signal value must be strictly greater than current value") holds by
construction.

## Why it exists

Before this lift, every surface had **one** timeline kernel object with
**two writers** racing on its next-value computation: the host
producer signaled it (via `vkQueueSubmit2` or host CPU
`signal_host`), and the subprocess consumer ALSO signaled it (via
CPU `signal_host` against the imported timeline). Each side computed
its next value as `state.current_release_value + 1` from a
per-process counter; the timeline kernel object was shared. The two
writers raced on value computation, tripping
`VUID-VkSemaphoreSignalInfo-value-03258` in production.

The "two timelines, one writer each" shape is what every production
engine surveyed converged on:

- **Unreal RHI** — fences are signaled by one queue submit per
  fence; readers wait. No multi-writer pattern on a single fence.
- **Granite** — `ExternalHandle` cross-process surfaces ride a
  directional pair (Granite produces, vendor API consumes); the
  signal side is always one writer.
- **wgpu-core** — a single tracker in the hub serializes all
  resource state; clients are passive.
- **Chromium GPU process** — all GPU work runs in the GPU process;
  the renderer is a passive client.
- **Khronos `VK_EXT_external_memory_acquire_unmodified` proposal**
  — states the underlying principle: "the kernel can't atomically
  coordinate two writers, so application protocol picks one."

None of these systems implement multi-writer-per-timeline. The
streamlib pre-lift shape was an artifact of treating the timeline as
shared mutable state across the process boundary; lifting to
two-timelines-one-writer-each is the conventional answer.

## The shape

```
                     ┌────────── Surface (surface_id) ──────────┐
                     │                                          │
   PRODUCER process  │   produce_done                           │   CONSUMER process
   ────────────────► │   (timeline)         ─── waits on ────►  │   ────────────────►
   signals ▲         │                                          │         ▼ reads
           │         │   consume_done                           │
   ◄─── waits on ─── │   (timeline)         ◄─── signals ───    │
                     │                                          │
                     └──────────────────────────────────────────┘

   produce_done writer: the producer process — one writer
     (either vkQueueSubmit2::pSignalSemaphoreInfos for GPU-side
     producers, or host CPU signal_host for CPU-side producers).

   consume_done writer: the consumer process — one writer
     (host CPU signal_host on the imported timeline, from
     within the consumer process's address space).
```

**Per-process `SurfaceState<P>` fields** (the per-adapter state
record). The v1 implementation chooses a single per-process signal
counter — within ONE adapter instance, signals to `produce_done` and
`consume_done` come from disjoint code paths (write-release vs.
read-release) so a unified monotonic counter is sufficient. Each
timeline still sees strictly monotonic values from its own writer
site, which is all VUID-03258 requires:

```rust
produce_done: Arc<P::TimelineSemaphore>,
consume_done: Arc<P::TimelineSemaphore>,

// Per-process monotonic counter — advanced on every signal
// regardless of which timeline gets it (see comment above).
current_signal_value: u64,
```

Read-side wait targets are derived from the peer-timeline's
`current_value()` at acquire time (see [Consumer rules](#consumer-rules)
/ [Producer rules](#producer-rules) below), not from a separate
locally-tracked field. Split counters (`next_produce_value` /
`next_consume_value`) become useful if a future shape ever needs
both edges to advance concurrently within the same adapter
instance; the v1 single-counter shape locks correctness for the
in-tree usage where the writer code paths are disjoint.

The two timelines are independent kernel objects. The producer
process owns the `produce_done` exportable handle and constructs the
imported handle for the consumer process; the consumer process owns
the `consume_done` exportable handle and constructs the imported
handle for the producer. Both are passed through the surface-share
IPC schema as OPAQUE_FD timeline semaphore handles.

## Per-adapter mapping

| Adapter | Producer side | Consumer side | `produce_done` writer | `consume_done` writer |
|---|---|---|---|---|
| **cuda** | host (`submit_host_copy_image_to_buffer` does the `vkCmdCopyImageToBuffer` into the OPAQUE_FD staging buffer) | subprocess (CUDA imports the buffer FD + reads via `cudaExternalMemoryGetMappedBuffer`) | host: `vkQueueSubmit2::pSignalSemaphoreInfos` from the trigger submit | subprocess: CPU `signal_host` in `end_read_access` (against the consumer-rhi imported timeline) |
| **cpu-readback** | host (trigger's `vkCmdCopyImageToBuffer` in `acquire_inner`) | subprocess (mmap-reads the HOST_VISIBLE staging buffer) | host: `vkQueueSubmit2::pSignalSemaphoreInfos` from the trigger submit | subprocess: CPU `signal_host` in `end_read_access` (restored under this lift — was defanged pre-lift because the multi-writer race could not be sound) |
| **vulkan** | the side that calls `begin_write` (host or subprocess, exclusive) | the side that calls `begin_read` | the writer process: CPU `signal_host` in `end_write_access` | the reader process: CPU `signal_host` in `end_read_access` |

For the vulkan adapter, "the side that calls `begin_write`" is the
process that holds the write lock at the time of release. The
`write_held` mutual-exclusion guarantees only one process can write
at a time; the writer process signals `produce_done` from its own
state.

## The v1 model: one producer process + one consumer process per surface

This lift declares **one producer process and one consumer process
per surface**, fixed at registration time. Multi-process concurrent
consumers (two subprocesses concurrently reading the same surface)
are out of scope for v1 — the producer process must publish to a
single consumer process, with fan-out happening at a higher layer
(pre-fan-out at the producer, or one subprocess with multiple
internal readers).

This is the model every production engine surveyed uses. The
"multiple subprocesses concurrently reading the same VkImage"
pattern that the pre-lift vulkan adapter's
`concurrent_reads_two_subprocesses` test exercised isn't a pattern
real engines design for; it was an emergent capability of the
pre-lift shape, not a designed feature. Same-process concurrent
reads (multiple Python threads, multiple in-process processors)
remain fully supported via the existing `read_holders`
last-reader-out semantics — only one `signal_host(consume_done)`
fires per release-episode regardless of how many local readers
participated.

**If a future use case genuinely needs multi-process concurrent
consumers**, the additive extension is N `consume_done` timelines
(one per attached consumer process), with the producer waiting on
ALL of them before re-writing. Lift to that shape when a real
consumer attests to needing it; don't pre-design.

## Producer rules

1. **Allocate both timelines at registration.** The producer owns
   `produce_done` (exportable, OPAQUE_FD) and creates the consumer's
   `consume_done` (exportable, OPAQUE_FD) — both flow through
   surface-share alongside the resource FDs.

2. **Track `next_produce_value` and `last_consume_value_observed`
   per-process.** Never read or write the consumer's
   `next_consume_value`. The consumer's signal is observed only via
   `vkSemaphoreWaitInfo` on `consume_done`.

3. **Signal `produce_done` from exactly one site per release.** For
   GPU-side producers (cuda + cpu-readback triggers), the signal is
   the `vkQueueSubmit2::pSignalSemaphoreInfos` slot on the trigger's
   submit. For CPU-side producers (vulkan adapter's
   `end_write_access`), the signal is `signal_host(next_value)`.
   Never both.

4. **Wait on `consume_done` before re-writing.** A producer that
   wants to re-use a surface must first confirm the consumer is done
   with the prior content — `produce_done.wait(prev_value)` confirms
   GPU drain of the prior write, but `consume_done.wait(...)` is the
   contract that the consumer has actually consumed it.

## Consumer rules

1. **Track `next_consume_value` and `last_produce_value_observed`
   per-process.** Never read or write the producer's
   `next_produce_value`.

2. **Wait on `produce_done` before reading.** Under v1 the consumer
   reads `produce_done.current_value()` at acquire time and waits on
   that snapshot — the peer-timeline's kernel counter is the source
   of truth, no cross-process per-frame value publishing is required.
   That covers steady-state ordering (each completed producer signal
   advances `current_value()` so the next consumer's wait sees it).
   If a future use case needs strict per-frame value publishing
   (e.g. the consumer wants to wait for a specific future frame
   that's already been queued but not yet signaled), the producer
   publishes the `produce_done` value alongside the read-side data
   (typically on the `VideoFrame` IPC payload or the adapter's
   acquire-acquired record) and the consumer waits on that value
   instead of `current_value()`. v1 deliberately doesn't ship that
   IPC plumbing — the kernel-counter snapshot is good enough for
   every in-tree consumer today.

3. **Signal `consume_done` from exactly one site per release.** For
   subprocess consumers (cuda + cpu-readback), the signal site is
   the subprocess-side `end_read_access` — `signal_host(next_value)`
   on the consumer-rhi imported timeline. For the vulkan adapter's
   consumer side, the same `end_read_access` shape applies whether
   the consumer is in-process or out-of-process.

4. **Last-reader-out semantics inside one consumer process.**
   Multiple concurrent readers within the same process coordinate
   via the existing `read_holders` counter; only the last reader to
   release signals `consume_done`. This stays unchanged under the
   lift.

## Anti-patterns

These are the failure modes the single-writer rule exists to
prevent. Each was either tried and rejected, or is the foreseeable
workaround that future agents would attempt without this doc.

1. **Multi-writer per timeline.** The pre-lift shape. Two
   processes signaling the same timeline kernel object from
   independent per-process counters races on monotonicity. The lift
   exists to make this unreachable; do not re-introduce it.

2. **Cross-process atomic counter as next-value oracle.** The
   alternative the issue body ruled out — use shared-memory atomic
   state for the next-value computation. Adds non-Vulkan IPC,
   doesn't match production engines, doesn't generalize to
   additional consumers. Single-writer-per-timeline is the answer.

3. **Safety-net clamp as architectural fallback.** The
   `signal_host` clamp removed under this lift
   (`consumer_vulkan_sync.rs` + `vulkan_sync.rs`, both pre-lift)
   self-corrected to `max(value, current+1)` to dodge VUID-03258.
   That was correct as a bridge while the multi-writer shape was in
   place, but it's not an architecture; it's a fault-tolerance
   patch. Don't re-add it as a "just in case" defense once the lift
   lands — the right way to enforce single-writer-per-timeline is
   the type system and the IPC schema, not runtime clamping.

4. **Conflating `produce_done` with `consume_done` via a single
   "current value" notion.** A surface has two independent monotonic
   counters going forward, not one. Code that reaches for a single
   `current_release_value` to satisfy both edges is re-introducing
   the multi-writer race in disguise.

5. **Producer signaling `consume_done` or consumer signaling
   `produce_done`.** Each timeline has exactly one writer process.
   "But just this once" is exactly the failure pattern; don't
   special-case.

6. **Multiple consumer processes attaching to one surface in v1.**
   Out of scope. If you find yourself needing it, lift to the
   N-`consume_done`-timelines extension cleanly; don't shoehorn it
   into the v1 single-consumer shape.

## Cross-process coordination

Under v1, each side derives the wait value from the peer-timeline's
kernel counter (`vkGetSemaphoreCounterValue`, exposed on consumer-rhi
as `VulkanTimelineSemaphoreLike::current_value()`) at acquire time
and waits on that snapshot. Steady-state ordering holds: a completed
producer signal advances `current_value()`, so the next consumer's
wait sees it; symmetric for `consume_done`.

If a future use case needs strict per-frame value publishing — e.g.
a consumer that wants to wait for a specific future frame the
producer has already queued but not yet signaled — the producer
publishes the `produce_done` value alongside the per-frame work item
(a field on the `VideoFrame` IPC payload or the adapter's
per-acquire record) and the consumer waits on that value instead of
`current_value()`. v1 deliberately doesn't ship that IPC plumbing —
no in-tree consumer needs it today.

No shared-memory state is needed in either shape. Each side's wait
reads the value (kernel counter or remote-published, depending on
shape), waits on the local imported timeline, and proceeds.

## Race model

Each timeline has exactly one writer process; next-value computation
is a pure function of that process's local state. There is no race
on monotonicity.

The cross-process IPC payload publishing `produce_done` /
`consume_done` values can in principle be observed by the consumer
before the corresponding signal has actually fired on the GPU — but
that's what `wait` is for; the consumer's wait blocks until the
signal materializes. No ordering primitive beyond the timeline
itself is required.

## Tests

Per-adapter conformance:

1. **Unit test** exercising concurrent producer + consumer
   acquire/release cycles against a real Vulkan device. Asserts:
   - Zero `VUID-VkSemaphoreSignalInfo-value-03258` occurrences (the
     race the lift exists to fix).
   - Zero `signal_host` clamp warnings (since the clamp is removed,
     this collapses to "the test runs without the clamp's safety
     net").
   - `produce_done.current_value()` and `consume_done.current_value()`
     advance monotonically and independently.

2. **E2E** — `camera-python-display` through the full multi-process
   polyglot pipeline with `VK_LOADER_LAYERS_ENABLE=*validation*`.
   Zero timeline-monotonicity validation errors.

When a new adapter lands, add the same dual-timeline conformance
coverage to its tests; the contract is uniform across all three
subprocess-wired adapters and any future siblings.

## Reference

- **Engine RHI primitives**:
  - `HostVulkanTimelineSemaphore::new_exportable`,
    `from_imported_opaque_fd`, `export_opaque_fd`, `wait`,
    `signal_host`, `current_value` in
    `libs/streamlib-engine/src/vulkan/rhi/vulkan_sync.rs`.
  - `ConsumerVulkanTimelineSemaphore` mirror in
    `libs/streamlib-consumer-rhi/src/consumer_vulkan_sync.rs`.
  - `VulkanTimelineSemaphoreLike` trait in
    `libs/streamlib-consumer-rhi/src/device_capability.rs`.
- **Adapters**:
  - `libs/streamlib-adapter-cuda/src/{state,adapter}.rs`
  - `libs/streamlib-adapter-vulkan/src/{state,adapter}.rs`
  - `libs/streamlib-adapter-cpu-readback/src/{state,adapter}.rs`
- **IPC schema**:
  - `libs/streamlib-engine/src/linux/surface_share/state.rs` —
    `SurfaceMetadata.produce_done_fd` and
    `SurfaceMetadata.consume_done_fd`.
  - `libs/streamlib-engine/src/linux/surface_share/unix_socket_service.rs`
    — wire register/lookup paths.
- **Companion docs**:
  - [`adapter-runtime-integration.md`](adapter-runtime-integration.md)
    — how a subprocess obtains an adapter context.
  - [`adapter-authoring.md`](adapter-authoring.md) —
    implementation contract for new surface adapters.
  - [`subprocess-rhi-parity.md`](subprocess-rhi-parity.md) — the
    consumer-rhi carve-out the imported timelines ride.
  - [`texture-registration.md`](texture-registration.md) —
    engine-wide per-surface lifecycle state record.
- **External references**:
  - [Khronos `VK_EXT_external_memory_acquire_unmodified`
    proposal](https://docs.vulkan.org/features/latest/features/proposals/VK_EXT_external_memory_acquire_unmodified.html)
    — articulates the single-writer principle.
  - [Unreal Engine RHI fence
    overview](https://dev.epicgames.com/documentation/en-us/unreal-engine/rendering-hardware-interface)
    — single-signaler-per-fence pattern.
