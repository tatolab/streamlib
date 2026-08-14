# Change: surface-id-lifetime-contract

**From the #1755 owner ruling, 2026-08-13** (recorded on the ticket; the align happened
in-session). ADR: `docs/decisions/surface-id-lifetime-contract.md`. Implements no new
GPU machinery — it restores the pool's original immutable-once-written /
taken-until-released model across the process boundary and states the export routing
that model implies.

Scale tier: change artifact **plus ADR** — it changes what a surface id promises every
consumer (the Python API's public contract) and the RHI's device-export source policy.
The IPC wire format is untouched except for one surface-share release verb; the
processor model is untouched.

Recon verified at HEAD `d14f9be3` on 2026-08-13, by two independent code sweeps
(producer ring + timelines; consumer export path + wheel surface) recorded in the
#1755 `[NEEDS DECISION]` comment.

## Behavior after this change

A published surface id names an immutable frame. From publish until every holder
releases it, the pixels under that id change only through the surface's own explicit
write-back protocol, never through producer reuse: the pool slot backing a held
surface is never rehanded to the producer — in-process via the existing refcount,
cross-process via a checkout lease the consumer's host takes at bag receipt. The producer never waits on a consumer: the pool
skips leased slots and grows to its cap, and at cap the producer drops its own frame.
A producer-internal transient (the camera's frames-in-flight ring texture) never backs
a cross-process export; the export blit sources the surface's pooled backing whenever
one exists. `numpy.from_dlpack(surface, device="cpu")` and `torch.from_dlpack(surface)`
describe the same frame, and that frame is the one the bag delivered.

## Current state — one id, two backings, three lifetimes

- The pool already implements taken-until-released — in-process only. Availability is
  an Arc refcount test (`gpu_context.rs:258-276`, `strong_count() <= 2`; baseline 2 =
  pool Vec + cache), skip-if-taken with growth to `POOL_MAX_BUFFER_COUNT = 64`
  (`gpu_context.rs:20-23`, `:279-290`). The lineage is the CVPixelBufferPool /
  IOSurface model (`gpu_context.rs:87`).
- A helper child's checkout never sets the taken bit: `resolve_surface` dups DMA-BUF
  fds through surface-share with no parent-side Arc
  (`python_helper_process_pixel_exchange.rs:340-347`), and the camera drops its own
  handle immediately after publish (`camera_source.rs:1176`) — so a camera pool slot
  recycles ~4 frames after publish regardless of who is reading it.
- The camera registers its transient 2-deep ring texture under the pool surface's
  public id (`camera_source.rs:998-1005`, `RING_TEXTURE_COUNT = 2` at `:41`) so the
  in-process display finds a device-resident copy (`display_window.rs:480`). The
  hand-off itself works: ring → pooled buffer copy every frame
  (`camera_source.rs:1078-1085`) with a host wait before publish (`:1116-1122`).
- The device export resolves texture-first (`device_export_staging.rs:189-200`), at
  the child's request, after the IPC round trip — so a cross-process GPU view reads
  the ring slot frame N+2 has already overwritten, while the CPU view reads the pool
  member. Observed as #1755's reproduction; not a cold-start artifact (the CUDA
  import is memoised per id, `python_helper_process_pixel_exchange.rs:266-276`).
- The camera's per-ring-slot `produce_done` / `consume_done` timeline pairs are
  created (`camera_source.rs:640-648`) and registered (`:786-793`) but never signaled
  and never waited; the comment at `:446-449` claims otherwise. The ring-reuse wait is
  on the camera's private timeline only (`:968-980`).
- The buffer arm of the export staging is fully implemented, including the write-back
  path the green TestPatternSource tests exercise (`device_export_staging.rs:472-545`).

## ADDED

- ADDED: a DECIDED entry in §Packages & extension model, after the handle-shaped
  primitive surface entry:

  > **DECIDED** — A published surface id names an immutable frame: from publish until
  > every holder releases it, the pixels under that id change only through the
  > surface's own write-back protocol (an explicit, engine-ordered edit other holders
  > are meant to observe) — never through producer reuse. The pool slot
  > backing a held surface is never rehanded to a producer — in-process via the
  > existing refcount, cross-process via a checkout lease minted by the surface-share
  > service at checkout, released explicitly by the consumer and reclaimed on
  > connection drop. The consumer's host performs that checkout eagerly at bag
  > receipt, not when user code first touches the surface, so the guarantee runs
  > from delivery; the publish-to-checkout transit is protected by pool depth. The producer never waits on a consumer: the pool skips leased
  > slots and grows to its cap; at cap the producer drops its own frame — a slow
  > consumer costs memory, then its own frames, never another processor's cadence. A
  > producer-internal transient (a frames-in-flight ring texture) never backs a
  > cross-process export: the export blit sources the surface's pooled backing
  > whenever one exists, read-only; texture-backed export remains for surfaces with
  > no pooled backing (kernel outputs). [surface-id-lifetime-contract]

- ADDED: a surface-share release verb — the wheel releases a checked-out surface when
  the **last share of its `GpuSurfaceOwnedMemory` drops**: handle close releases only
  the handle's share, and every exported view (DLPack capsule, numpy view) holds its
  own until its deleter runs — the ownership contract the wheel already implements
  (`python_gpu_surface_pixel_exchange.rs:12-14`, `:69` — release runs in `Drop`, never
  on `close()`, so a tensor outliving its handle keeps live memory). The lease rides
  that same last-share drop; releasing at handle close alone would let the pool
  recycle a slot under a live view. The existing EPOLLHUP watchdog
  (`state.rs:438-446`, `surface_ids_by_runtime`) is the backstop for a child that
  dies holding one. The release-debt bookkeeping extends the shape
  `EscalateHandleRegistry` already carries for *acquired* buffers
  (`python_helper_process_pixel_exchange.rs:212-248`) to *resolved* ones. The
  checkout moves to the child host's bag-receipt path: today `resolve_surface` is a
  user-facing call (`python_processor_context.rs:596-607`), so the lease could not
  begin until user code touched the surface, leaving a queued bag unprotected for
  its whole queue time; ~~the host's reader thread checks out on arrival instead~~
  the host checks out on arrival instead
  (user-visible behavior unchanged — the handle the callback gets is already
  checked out).

  > ~~the host's reader thread checks out on arrival~~ — Corrected 2026-08-14 by
  > the #1866 implementation, verified at `origin/main` 53d5410f. No thread in the
  > wheel receives bags: `ParentProcessBridge._reader` (`_helper.py:116-120`) serves
  > only the escalate / lifecycle socket, and bags arrive over iceoryx2, pulled
  > synchronously on the processor's own thread by `InputMailboxesInner::receive_pending`
  > (`runtime/streamlib-engine/src/iceoryx2/input.rs`, called from `read_raw` /
  > `has_data` / `any_port_has_data` and nowhere else). Queueing is two-stage — the
  > iceoryx2 subscriber queue, then the per-port mailbox — so "bag receipt" is the
  > mailbox push, not an arrival anything is woken for. The stated *how* did not
  > exist, and the owner re-ruled the *what* on 2026-08-14 (#1866, option D):
  > receipt-time eager checkout is retracted. The claim is taken at the typed
  > cast — `read(port, into=VideoFrame)`, the moment the consumer names what it
  > is holding — and released when the frame object drops, the same last-share
  > RAII the resolved handle already carries. Queue-time transit is bounded by
  > pool depth; an untyped dict read gets depth-only protection. The engine
  > inspects no bag content anywhere. The cast-claim implementation and this
  > entry's plan amendment ride the follow-up ticket.

- ADDED: an engine-level rotating-producer fixture and ground-truth test: a synthetic
  producer replicating the camera's shape (pool surface + transient ring texture
  registered under the pool id, counter-stamped pixels), asserting (a) a device
  export refilled after the producer has advanced ≥2 frames still returns the pixels
  the bag was published with, and (b) a leased slot is never re-acquired while held —
  including by a view that outlives its closed handle (close the handle, keep the
  tensor, assert the slot stays leased until the view's deleter runs) — and the pool
  grows then drops at cap. The exact test spelling is implementation's;
  the obligation is ground truth against the published frame, not view-identity.
- ADDED: `test_camera_device_pixels_match_host_across_ring_cycles` unskips, rewritten
  per the ruling: on the rig it asserts the two views' identity across ring cycles
  *plus* a deliberately lagged consumer still reading its delivered frame — the
  view-identity-only shape it has today cannot fail for the reason #1755 exists
  (`sdk/streamlib-python-wheel/tests/test_device_exchange.py:158-186`).

## MODIFIED

- MODIFIED: §Packages & extension model, handle-shaped primitive surface entry
  (ARCHITECTURE.md:67-75) — the honest zero-copy sentence gains the source clause:
  the one GPU blit into the exportable staging reads the surface's pooled backing
  when one exists; a producer-internal texture never sources a cross-process export.
- MODIFIED: `resolve_device_export_source` (`device_export_staging.rs:189-200`) — the
  match arms swap: pixel buffer first, registered texture as the fallback for
  surfaces with no pooled backing. Kernel-output surfaces (texture-only) keep the
  texture arm unchanged; every green TestPatternSource device-exchange test already
  exercises the buffer arm.
- MODIFIED: the export `writable` discriminator (`device_export_staging.rs:214-243`) —
  a dual-backed surface (registered texture + pool member, the producer-published
  shape) exports read-only; a buffer-only surface keeps today's writable semantics,
  preserving the green write-back tests. The refill for a dual-backed surface becomes
  a host→VRAM upload (~0.3–0.5 ms at 1080p, PCIe 4.0), paid only when a consumer
  takes a device tensor; the producer pays nothing new
  (`camera_source.rs:1078-1085` is unconditional today).
- MODIFIED: pool acquire (`gpu_context.rs:256-290`) — availability stays
  refcount-aware in-process and becomes lease-aware across processes: a slot is
  available only when it is neither held by an in-process refcount nor leased by a
  checkout. The surface-share service owns the lease set (checkout pins,
  release/EPOLLHUP unpins), and the pool consults it through the `surface_store`
  handle acquire already receives (`gpu_context.rs:126-131`). The availability
  check and the slot hand-off are one atomic operation with respect to the lease
  set — a concurrent checkout cannot land between the check and the hand-off (the
  pool lock at `gpu_context.rs:103` and the lease state at `state.rs:188` are
  separate today; the claim mechanism is implementation's, the no-interleaving
  invariant is not). When a surface-share service is running but its lease state
  cannot be read, acquire fails closed and skips reuse; when no service is running,
  no cross-process consumer can exist and the refcount check alone is complete.
- MODIFIED: the camera loses nothing and waits on nothing new; on pool-exhausted it
  already drops the frame through the existing error path
  (`camera_source.rs:1132-1150`).
- MODIFIED: the comment at `camera_source.rs:446-449` — falsified today (it describes
  produce/consume edges no code implements) and removed with the pairs below.

## REMOVED

Bare patterns — the ship gate greps each line verbatim.

- REMOVED: ring_produce_done
- REMOVED: ring_consume_done
  The camera's per-slot timeline pairs: created, registered into surface-share, never
  signaled, never waited (`camera_source.rs:640-648`, `:786-793`, drop at
  `:1222-1223`). The ruling rejected the consume_done backpressure path, and with the
  export routed to the pooled backing no cross-process consumer touches the ring
  texture — the pairs are dead and their existence implies the rejected design.
  `register_texture`'s semaphore params are `Option` (`surface_store.rs:1329-1336`);
  the camera passes `None`. The adapter single-writer contract
  (`docs/architecture/adapter-timeline-single-writer.md`) survives untouched for
  adapters that genuinely write into surfaces; the pre-pivot copy in `packages/`
  lags by design and is outside the sweep.

## Rejected by the ruling (recorded so they do not resurface)

- Unconditional consume_done backpressure: engine-level GPU blocking on a realtime
  source no port opted into; contradicts the isolation axis (ARCHITECTURE.md:122);
  needs the N-consumer timeline model `adapter-timeline-single-writer.md` defers.
- Deepening the ring: widens a race it cannot close and hides the reproduction.
- Per-frame texture allocation/registration: the tree's documented anti-pattern
  (`gpu_context.rs:3051-3060`); leaks an fd pair per registration
  (`device_export_staging.rs:119-124`); pooled `acquire_texture` is not
  cross-process-safe on NVIDIA (`gpu_context.rs:1100-1107`).
- Latest-wins as the terminal contract (routing flip alone): leaves a lagging child
  silently reading frame N+4k in both views — agreement without truth.

## Notes (not tickets)

- `TextureRing` (`core/context/texture_ring.rs:245-254`) and vulkan-jpeg's decoder
  ring (`sdk/vulkan-jpeg/src/simple_decoder.rs:19-36`) rotate under *stable,
  honestly-documented* per-slot ids — a different, disclosed contract. Out of scope;
  any future producer minting per-frame ids over a rotating backing falls under the
  new plan entry.
- The lease makes the *pool member* immutable while held. The in-process display
  keeps reading the ring texture at frame cadence (`display_window.rs:480`) —
  unchanged, inside the transient window by construction.
- Cold-start cost is untouched: staging + CUDA import stay memoised per id
  (`device_export_staging.rs:206-280`).
