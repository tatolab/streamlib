# Texture-backed CPU reach

Rationale for the `[texture-backed-cpu-reach]` entry in `docs/plan/ARCHITECTURE.md`
§Graphics, decided by the owner 2026-08-24. Discharges the last undesigned cell of the
kernel-kind parity bar: a numpy-only author reading and writing the pixels of a
texture-backed surface (a kernel output, an acquired texture) with no CUDA runtime and
no GPU package installed.

## Trigger

Read this before adding any CPU-facing pixel surface to the wheel, before proposing a
`readback()`-style vocabulary, before touching the export stagings' memory-type
selection, or when someone asks why the non-blocking readback ops are gone.

## Decision

1. **One door, no backing vocabulary.** The existing CPU spellings — the cast object's
   `cpu()`, the surface handle's CPU lock and `as_numpy()` — start working on
   texture-backed surfaces, routed over the host-visible export staging. No separate
   readback verb exists.
2. **The staged door publishes at the block edge and discards on a propagating
   raise** — the same rule as every device-write scope. The coherent pixel-buffer
   mapping keeps per-store publication. The door's stated contract spans both: a raise
   leaves the frame already held or a complete edit of fewer pixels, never a torn
   frame; which of the two is the backing's own.
3. **Entering always reads the frame in**, a pure write included. The read-in is what
   makes the write-back legal under the read-before-write guard, and that guard cannot
   be conditioned on "was this id ever published" — the engine inspects no bag content,
   so it cannot know.
4. **Every staging copy blocks; `contended` reaches no author.** The unconsumed
   non-blocking surface — the `try_`-variant readback wire op, its `contended` response
   variant, and the engine's `try_`-prefixed staging copies — is deleted.
5. **The helper child checks out and maps the staging itself.** Pixel bytes never cross
   the escalate socket; the fd-based surface-share checkout is the transport, exactly
   as the pixel-buffer and device-export arms already work.
6. **The readback staging allocates host-cached** — a third OPAQUE_FD pool probed
   `HOST_ACCESS_RANDOM` — falling back to the sequential-write pool on a device with no
   cached exportable memory type: slower there, never refused.
7. **Python's `acquire_texture` implies `copy_src` and `copy_dst`.** Rust's descriptor
   stays explicit. A texture whose usage still cannot take the copy (a foreign
   registration without transfer usage) refuses the door by name.

## Rejected alternatives

**A named readback vocabulary** (`readback()`, or resurrecting the pre-pivot
`acquire_read`/`acquire_write` context managers). Rejected because it forces the author
to branch on an allocation flavour the engine chose for them — the same reason
importability is derived, never a Python dial. One door also serves both ruled-on call
sites with one spelling: the LUT fill on an acquired texture and the downstream read of
a kernel output.

**Per-store publication on the staged arm** (making the staging pretend to be a
coherent mapping). The plan's per-store rule was a forced consequence — "there is no
staging whose discard this could promise" — not a chosen semantic. Where a staging
exists, per-store would mean a copy per store or a lie about visibility; the block-edge
rule is what the machinery honestly provides, and it buys back the discard-on-raise
guarantee the device scopes already have.

**A write-only entry that skips the read-in.** The read-before-write guard exists
because a staging spans every frame its pool slot publishes, and the engine cannot
distinguish a fully-overwritten staging from uninitialised memory — nor a private
surface from a published one, since it never inspects bags. A zero-fill-and-mark-read
variant would fabricate a frame that never existed. The cost of always reading in is
one small copy at setup for the LUT case.

**Surfacing `contended` to Python** (a `try_`-door returning `None`). Rejected as the
poll-and-retry vocabulary the synchronous-dispatch decision already keeps out of
Python. With no consumer in any language, keeping the non-blocking ops as dead wire
surface would oblige every future child to grow an arm for a response nothing sends.

**Parent-side reads shipping bytes over the escalate socket.** Costs a full-frame copy
per read on a socket sized for control traffic, and abandons the fd-based checkout
shape both existing exchange arms use. Child-side mapping is shipped machinery
(OPAQUE_FD import as a mapped host-visible buffer).

**Re-probing the shared OPAQUE_FD buffer pool to host-cached.** That pool also serves
producer-write paths (the CUDA-interop upload among them) where sequential-write is the
correct hint; fixing the read path by slowing the write paths trades one defect for
another. A third pool scoped to the readback residency fixes only what is wrong.
"Refuse readback where no cached exportable type exists" was rejected as a functional
regression against a capability that works — merely slower — everywhere today.

**Requiring authors to spell `copy_src`/`copy_dst` at `acquire_texture`.** The
zero-ceremony bar loses: the LUT author would write three usage tokens to fill a
texture they own, and the refusal they hit otherwise is about a flag, not a real
constraint. Implying both never breaks flavour derivation (the OPAQUE_FD fixed usage
set contains both; `render_attachment` is unaffected), and the write-back refusals that
carry meaning — a producer-owned pooled frame — are ownership-shaped and survive
intact.

## Consequences

- The wheel grows a readback-staging checkout-and-map arm beside the CUDA
  device-export arm; both key the same memoisation and the same surface-share
  transport.
- `surface_can_take_write_back` answers true for every Python-acquired texture — honest,
  since they can take a recorded copy in; the refusal semantics narrow to
  producer-owned pooled frames and foreign registrations without transfer usage.
- Exception-safe author code must not depend on *which* of the two no-torn-frame states
  a raise leaves; code that must not publish on failure edits outside the scope.
- A third staging pool means device-init probes one more memory type; devices without a
  cached exportable type silently keep today's speed.
- Deleting the `try_` ops shrinks the wire; any future non-blocking need re-enters
  through a plan change, not by resurrecting the variant.
- CPU readback of texture-backed surfaces stays Linux-only with the rest of the surface
  exchange.
