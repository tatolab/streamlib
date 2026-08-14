# A surface id names an immutable frame

Rationale for the `[surface-id-lifetime-contract]` entry in `docs/plan/ARCHITECTURE.md`,
decided by the owner 2026-08-13 on #1755 after two independent code sweeps. Change file:
`docs/plan/changes/surface-id-lifetime-contract.md`.

## Trigger

Read this before adding a backing under an existing surface id, before letting any
producer-internal transient (a frames-in-flight ring, a scratch texture) answer for a
published id, before adding any path where a producer waits on a consumer, and when
someone asks why a Python processor's frame cannot change under it.

## The decision

1. A published surface id names an immutable frame: from publish until every holder
   releases it, the pixels under that id do not change. This is not a new semantic — it
   is the pool's original CVPixelBufferPool / IOSurface-lineage model
   (taken-until-released, skip-if-held, grow-on-pressure), which the engine has always
   implemented in-process via an Arc refcount, extended to where consumers now live.
2. Cross-process holders pin by lease: the surface-share checkout mints it, explicit
   release or connection drop clears it, and the pool treats a leased slot exactly like
   a refcounted one — skip and grow.
3. The producer never waits on a consumer. At pool cap the producer drops its own
   frame: a slow consumer costs memory, then its own frames, never another processor's
   cadence.
4. A cross-process export is sourced from the surface's pooled backing whenever one
   exists, read-only. Texture-first export survives only for surfaces with no pooled
   backing (kernel outputs, whose id↔backing binding is stable).

## Why

**The bug was a broken promise, not a missing feature.** #1755: one camera id resolved
to a stable CPU view and a rotating GPU view — `numpy` read the delivered frame,
`torch` read a newer one, silently. Both backings turned out to be recycled
producer-owned rings (texture depth 2, pool depth 4); the pool's "taken" test is an
address-space-local refcount no helper child can bump. Helper-process placement made
the first consumer that is slow, remote, and unpinned all at once — the enforcement
mechanism stopped reaching the consumers the product is for.

**Silent wrongness is the worst failure mode a Python-first library can ship.** The
divergence never crashes, has the right shape, and shifts with load; an inference
result quietly stops matching the timestamp it is reported against. Under this
contract the failure modes are visible and honest: memory growth, then the producer's
own dropped frames.

**The producer's cadence is the isolation axis applied to data.** The plan already
states no processor may block, stall, or degrade another
(`helper-process-placement-only`). A lease that producers skip preserves that; any
consumer-driven wait would put a Python child's speed inside the camera's loop.

## Alternatives rejected

- **Latest-wins (route the export to the deeper pool ring and stop).** Makes the two
  views agree without making either true — a child lagging ≥4 frames reads frame
  N+4k in both, consistently. Kept only as the routing *floor* under the lease.
- **consume_done backpressure.** The per-slot timeline pairs existed unused; joining
  them makes an unopted-in Python consumer set a realtime source's cadence, and
  needs the N-consumer timeline model `adapter-timeline-single-writer.md` explicitly
  defers (the in-process display consumes the same ring and signals nothing).
  Bounded-block-then-drop delivery exists as `lossless` — a per-port, app-authored
  choice, which engine-level GPU backpressure is not.
- **Deepening the ring.** Depth widens a race it cannot close and turns a reliable
  reproduction into a load-dependent one.
- **Per-frame texture allocation/registration.** Documented anti-pattern; leaks an
  fd pair per registration; the pooled texture path is not cross-process-safe on
  NVIDIA.
