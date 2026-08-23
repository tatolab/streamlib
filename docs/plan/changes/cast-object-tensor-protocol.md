# Change: cast-object-tensor-protocol

Implements the `[cast-object-tensor-protocol]` DECIDED entry (§Packages & extension
model), decided in the #1731 align. Rationale:
`docs/decisions/cast-object-tensor-protocol.md`. Scale tier: change artifact + ADR —
the delta changes the Python API's public contract; the ADR landed with the align
(PR #1924). Wheel-layer only: no engine change, no wire change.

The object `read(port, into=T)` returns becomes the tensor-protocol producer: bare
DLPack as the read path, two scopes as the write doors, one shipped composable any cast
type composes.

## Current state (tree at `b29b5421`)

- The claim seam is public and unprivileged: `read(into=T)` opens a thread-local offer
  (`sdk/streamlib-python-wheel/src/python_bag_conversion.rs:68-141`) that any
  constructing class may take via
  `streamlib.gpu_limited_access_of_the_typed_read_in_progress()`
  (`python_bag_conversion.rs:164`; exported `python/streamlib/__init__.py:38,76`).
- `VideoFrame` takes the claim by hand: a private helper claims the surface its
  `surface_id` field names (`python/streamlib/video_frame.py:140-161`) and stashes the
  lease on the frozen instance (`video_frame.py:282-287`).
- Pixel reach today requires ceremony the decided entry retires for cast objects:
  resolve + lock + `from_dlpack` (the probe spelling,
  `sdk/streamlib-python-wheel/tests/cast_claim_probes.py:119-122`). The machinery the
  protocol rides already ships: `GpuSurfaceHandle.__dlpack__` under an explicit lock
  (`sdk/streamlib-python-wheel/src/python_processor_context.rs:436-489`, natural device
  side chosen once per handle at `:112-115`), the always-writable device-tensor scope
  with blit-back-on-exit / discard-on-raise (`:575-740`), and the CPU host mapping
  (`:361-388`; coherent, so stores publish as they land).

## ADDED: the shipped composable

One public piece — worked name `ClaimedSurfacePixelAccess` (zero-context rule; final
name lands with the implementing ticket) — that a cast type composes to become the
tensor-protocol producer for the surface it claims:

```python
from streamlib import ClaimedSurfacePixelAccess

@dataclass(frozen=True, init=False)
class DepthFrame(ClaimedSurfacePixelAccess):   # claims the field named "surface_id"
    surface_id: str
    width_in_pixels: int
    height_in_pixels: int
```

At construction inside a typed read it takes the claim through the existing offer —
the seam underneath is unchanged; outside a typed read (`from_bag`-style construction)
it claims nothing, exactly as `VideoFrame` behaves today. The surface-naming field
defaults to `surface_id` and is declared, never guessed — the engine inspects no bag
content, and neither does the wheel.

What composing it provides:

- `__dlpack__` / `__dlpack_device__` — the bare read path. GPU-resident on the
  surface's natural device side; internally held read-only lock; validity rides the
  claim (immutable while the object lives, ended when it drops). A write through the
  bare view is out of contract.
- `writable()` — the GPU write scope, riding the device-tensor scope mechanics: enter
  blits out, exit publishes ordered ahead of the engine's next read, a propagating
  exception discards and never suppresses.
- `cpu()` — the CPU write scope, riding the host-mapping path: yields a numpy array
  writable exactly when the frame can take a write-back (a producer-owned frame arrives
  read-only, numpy-enforced); where writable, the coherent mapping publishes stores as
  they land — no staging, no block-edge discard — and a raise is never suppressed.

```python
frame = ctx.inputs.read("depth_from_upstream", into=DepthFrame)
tensor = torch.from_dlpack(frame)          # shortest spelling is the fast path
with frame.writable() as t:                # GPU edit; block edge publishes
    t.mul_(0.5)
with frame.cpu() as img:                   # the slow path says so in its name
    img[0:10, :, :] = 255
```

A type that claims more than one surface gets no bare `__dlpack__` — the ambiguity is
refused by name — and reaches each surface through that surface's own protocol object
(worked spelling: a per-surface accessor on the composable; name lands with the
ticket).

## MODIFIED

- `VideoFrame` is rebuilt on the composable (`python/streamlib/video_frame.py`) — its
  hand-rolled claim-taking becomes the composable's, its behaviour otherwise unchanged.
  Being built from the shipped piece is the proof it holds no privileged position.
- The stub and typing surface: the composable, its scopes, and the protocol methods get
  `_engine.pyi` / `py.typed` entries per the typing posture (a new public class is not
  done until the stub entry exists).
- Docs teach the gradient in this order: `torch.from_dlpack(frame)` first, `writable()`
  for GPU edits, `cpu()` named as the slow path.

## REMOVED

Nothing. Additive wheel surface; the fold of `VideoFrame` onto the composable is a
MODIFIED, verified by the implementing diff and its tests rather than a grep pattern.

## Behavior after this change

The frame a processor reads is directly consumable by any DLPack consumer with zero
ceremony; edits are scoped — GPU edits publish at the block edge, CPU stores land in
the coherent mapping as written — and a frame that takes no write-back says so through
every door; the CPU path is named. The
resolve/lock spelling remains for surface handles reached outside a typed read — the
cast object is grammar over it, not a replacement.

## Sequencing

Independent of `processor-owned-windows` (the engine+wire sibling from the same align);
shippable first and alone. The `into=` dial, the claim seam, and every primitive it
rides are already on main.

## Out of scope

- Any engine or wire change; any change to the claim seam's mechanics.
- Enforcing read-only on the bare view — placed out of contract by the ADR, not
  deferred.
- The zero-copy per-frame foreign-stack hand-off (§Packages OPEN) — unrelated door.
- Consumer packages and examples, which lag by design.
