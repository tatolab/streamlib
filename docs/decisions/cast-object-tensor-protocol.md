# The cast object is the tensor-protocol producer

Rationale for the `[cast-object-tensor-protocol]` entries in `docs/plan/ARCHITECTURE.md`
§Packages & extension model, decided by the owner 2026-08-23.

## Trigger

Read this before adding a pixel-access verb to the read path, before giving `VideoFrame` a
capability other cast types lack, before making DLPack reach conditional on entering a scope,
or when someone asks why `torch.from_dlpack(frame)` works on the thing `read()` returned.

## Decision

1. **The bare cast object speaks DLPack.** The object `read(port, into=T)` constructs
   implements `__dlpack__` / `__dlpack_device__` directly — Holoscan's trick: the thing the
   read hands you already is the tensor-protocol object, so the shortest spelling is the fast
   path. The view is GPU-resident and its validity rides the claim the typed cast takes
   (`surface-id-lifetime-contract`): the frame is immutable while the object lives, so the
   view is stable by construction, and the object dropping ends it. A write through the bare
   view is out of contract — the write doors are the scopes.
2. **The performance gradient is spelled, not policed.** Read/inference:
   `torch.from_dlpack(frame)` — shortest, GPU-resident. GPU edit: `with frame.writable() as
   t:` — the block edge is the publication point. CPU / skia / PIL: `with frame.cpu() as
   img:` — the slow path says so in its name. Whether a frame takes an edit at all is the
   engine's one answer for both doors: a write-back belongs to a surface whose only backing
   is its own pooled allocation, so a frame its producer still owns refuses `writable()` by
   name and reaches `cpu()` read-only. `writable()` keeps the write-scope rule the plan
   states for the device-tensor scope: exit publishes, ordered ahead of the engine's next
   read; a propagating exception discards the write and never suppresses the exception.
   `cpu()`'s publication semantics are the host mapping's own — see point 6.
3. **Wheel-layer grammar only.** The protocol is spelled over the shipped primitives —
   per-surface staging, the DLPack export machinery, the typed-cast claim
   (`surface-id-lifetime-contract`) — with no engine change. `VideoFrame` holds no
   privileged position: any library or user cast type gets the same protocol.
4. **The wheel ships the protocol as one public composable piece.** Any cast type
   composes it; `VideoFrame` is itself built from it, which is the proof of no privilege.
   The claim seam underneath is unchanged — the composable rides the same public offer
   any constructing type may already take.
5. **The bare protocol binds exactly one claimed surface.** A type claiming several gets
   no bare `__dlpack__` — the ambiguity is refused by name — and reaches each surface
   through that surface's own protocol object. Guessing a "primary" surface silently
   would be the silent-wrongness posture the lifetime contract exists to kill.
6. **`cpu()` yields a numpy array writable exactly when the frame can take a write-back**
   — the engine's answer, asked over the shipped escalate surface and memoised per pool
   slot; a frame its producer still owns arrives read-only, and numpy enforces the flag at
   the write line itself. The array is the existing host-mapping path — skia and PIL wrap
   numpy trivially — and it is coherent, so where writable, stores publish as they land:
   there is no staging and no block-edge discard, and a raise mid-edit leaves a complete
   edit of fewer pixels, never a torn frame.

## Rejected alternatives

- **A scope for reads.** Requiring an `as_device_tensor()`-style enter/exit for the dominant
  read → infer path would carry ceremony that exists to guard writes. The claim already
  guarantees a read view's stability; a read scope would protect nothing. The internal blit
  from tiled texture to linear view is machinery, not user-visible ceremony — §Packages
  already commits to it.
- **An enforced-read-only bare view.** DLPack's read-only flag exists only in the versioned
  (≥ 1.0) exchange and consumers may ignore it; a CUDA consumer cannot be memory-protected
  out of writing. Claiming an enforcement the engine cannot deliver is the dishonest posture;
  out-of-contract mirrors the raw-fd use bound already on the books
  (`raw-handle-export-contract`).
- **Policing the gradient.** Warning on or refusing the slow paths would gate legitimate
  skia/PIL work; the library's posture is spelled honesty — the slow path is named, never
  blocked.
- **Autobox-from-annotation.** Already rejected by `schema-free-ports` — annotations stay
  human/type-checker-only. Recorded again here because the tensor protocol strengthens the
  temptation: `into=` stays the one cast mechanism.
- **Pure convention instead of a shipped composable.** Documenting how to implement the
  protocol against the public claim and handle functions keeps the wheel smaller, but
  every library would re-derive the lock/staging choreography, and each divergence would
  be a subtly different pixel-access contract wearing the same method names.

## Consequences

- The wheel ships the composable and rebuilds `VideoFrame` on it; the reference
  implementation and the public path are the same code.
- The documented gradient becomes the taught spelling: `torch.from_dlpack(frame)` first,
  the scopes for writes, `cpu()` named as the slow path.
- Cost accepted: a write through the bare view cannot be prevented, only placed out of
  contract — the same honesty posture as the raw-fd use bound.
- Cost accepted: a multi-surface cast type carries per-surface protocol objects instead
  of one flat surface; the bare spelling stays reserved for the unambiguous case.
- Cost accepted: the CPU door has no block-edge atomicity — the array is the surface's
  own mapping, so publication is per store. Stated as where the guarantee ends, never as
  a caller bound: nobody misbehaves by raising inside the block.
- The read-only downgrade is enforced on the CPU door precisely because it is enforceable
  there: numpy honors the DLPack read-only flag, which is the enforcement a CUDA consumer
  of the bare view cannot be given — the same honesty test, answered per consumer.
