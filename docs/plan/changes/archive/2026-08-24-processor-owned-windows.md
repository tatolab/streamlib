# Change: processor-owned-windows

Implements the `[processor-owned-windows]` DECIDED entry (§Media I/O), decided in the
#1731 align. Rationale: `docs/decisions/processor-owned-windows.md`. Scale tier: change
artifact + ADR — the delta adds escalate wire surface and public Python API; the ADR
landed with the align (PR #1924).

Window ownership becomes a processor capability: any processor may request a window and
own its policy; the engine mints it and, for owners outside the app process, runs that
window's native present loop, fed by surface ids the owner names.

## Current state (tree at `b29b5421`)

- No present-class escalate op exists. The op inventory is 21 texture/kernel/staging
  ops (`runtime/streamlib-engine/src/core/compiler/compiler_ops/subprocess_escalate_wire_types/escalate_request.rs:18-81`);
  dispatch matches exactly those
  (`runtime/streamlib-engine/src/core/compiler/compiler_ops/subprocess_escalate.rs:279`).
- The present seams are app-process-only: `GpuContextFullAccess::create_present_target`
  (`runtime/streamlib-engine/src/core/context/gpu_context.rs:3878`) and
  `create_present_compositor` (`gpu_context.rs:3895`, doc-marked in-process only). Sole
  live caller: the built-in display
  (`runtime/streamlib-media-builtins/src/display_window.rs:296-306`).
- The pump mints windows and forwards exactly two events, coalesced —
  resize and close-request
  (`runtime/streamlib-engine/src/core/window_event_pump.rs:46-53,61-67,126-131`); a
  process that cannot build an event loop answers with the same error forever
  (`window_event_pump.rs:190-199`).
- The built-in display's loop is the machinery this change hoists: a `latest` mailbox
  (`display_window.rs:119`), a render thread whose iteration is
  resolve-by-surface-id → `present_target.render_frame(compositor.compose…)`
  (`display_window.rs:262-279,372-504`).
- Setup-phase hooks hold the Full typestate and escalate ops execute parent-side under
  `sandbox.escalate(|full| …)` (`subprocess_escalate.rs:279-282`,
  `gpu_context.rs:3507-3532`); `process()` holds Limited only
  (`sdk/streamlib-python-wheel/src/python_processor_context.rs:1812-1855`).

## ADDED: the engine-owned present-loop seam

One present-loop machinery, made a named engine seam instead of choreography private to
the built-in display: per window, a native loop that resolves a named surface id and
composes it to the present target — acquire, blit (letterboxed by default), present —
paced by vsync, latest-wins. Naming no frame leaves the last one up. Each window's loop
runs on its own thread (the decided non-serialisation clause), never the pump's. The
compositor stays internal to the seam — no new public spelling.

The seam has two drivers:

- a **cross-process owner**, via the escalate ops below — the engine owns the loop
  thread, created at window request and joined at processor teardown;
- the **built-in display**, whose render thread drives the same seam fed from its input
  port. Its private copy of the choreography folds into the seam in the same change —
  that fold is the proof of "one machinery".

## ADDED: present-class escalate ops

Worked wire spelling (names follow the zero-context rule; final names land with the
implementing tickets):

- `create_processor_owned_window { window_title, initial_width_in_physical_pixels,
  initial_height_in_physical_pixels }` → window id. Setup-phase, Full-gated like every
  minting op. Refusal (no display server, dead pump — the pump's cached error) crosses
  the wire and raises at `setup()`.
- `show_surface_on_processor_owned_window { window_id, surface_id }` — per-frame,
  reachable from `process()`. Resolution honours the surface-id lifetime contract: a
  retired id is a loud recycled-frame error, never another frame.
- `drain_processor_owned_window_events { window_id }` → coalesced
  `{ current_width, current_height, close_requested_by_user, window_is_closed }`.
- `close_processor_owned_window { window_id }` — explicit release; also implied by
  processor teardown.

An unread close-request closes the window engine-side; after that,
`show_surface_on_processor_owned_window` is a no-op that reports the window closed
rather than an error — a user gesture never takes down a pipeline.

## ADDED: the wheel's window object

Worked spelling (attachment point follows the kernel-object convention — constructed in
`setup()` where the typestate is Full, driven in `process()`):

```python
def setup(self, ctx):
    # raises if the process cannot get a window — optional windows are try/except
    self.debug_window = ctx.gpu_full_access.create_window(
        title="pose debug", width=640, height=480)

def process(self, ctx):
    frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
    overlay = self.kernel.dispatch(...)
    self.debug_window.show(overlay)      # cast object, kernel-output handle, or bare id
    events = self.debug_window.drain_events()   # coalesced; polling is optional
    if self.debug_window.is_closed:
        ...                              # owner's close policy — react, don't prevent
```

`show()` accepts anything that names a published surface: the cast object (whose claim
guarantees the id un-recycled), a kernel-output handle, or a bare surface id as the
escape hatch. Events are polled coalesced state — no callback crosses the hop. The stub
(`_engine.pyi`) and py.typed entries land with the class, per the typing posture.

## MODIFIED

- The built-in display: its render thread drives the engine seam instead of carrying
  the acquire/compose/present choreography privately (`display_window.rs:372-504`).
  Behaviour is unchanged — same mailbox, same latest-wins, same drain-and-discard
  fallback.
- A native (Rust) processor may drive its own render thread against its present target
  exactly as the built-in does — the request seam is shared; the deadline constraint
  binds only code outside the app process.
  > ~~No new Rust API is required by this change.~~ — Superseded 2026-08-24 by #1934
  > (PR shipped): folding the built-in display onto the shared seam forces a public
  > SDK export (`sdk/streamlib-sdk/src/lib.rs`), because the built-in lives in a
  > separate crate. Mechanically forced by the fold this file mandates, not a widened
  > design — the compositor still stays engine-private.

## REMOVED

Nothing. The change is additive; the one-machinery proof is the built-in display
driving the shared seam, shown by the implementing diff rather than a grep pattern.

## Behavior after this change

A pip-installed Python processor requests its own debug window in `setup()` and names
frames to it from `process()`; the window presents at vsync regardless of the helper's
pace. N processor-owned windows coexist with N built-in displays under the one pump.
Closing the window by hand leaves the pipeline running and the processor informed. A
headless run raises at `setup()` unless the author opted out with `try/except`.

## Sequencing

Independent of `cast-object-tensor-protocol` (the wheel-only sibling from the same
align). Engine seam first, wire ops second, wheel object last — each step testable at
its own layer; the display-window live test
(`runtime/streamlib-media-builtins/tests/two_display_windows_live.rs`) plus a
processor-owned-window equivalent are the rig evidence.

## Out of scope

- Input events beyond resize and close-request — the pump discards the rest,
  uninterpreted, unchanged.
- A scaling-mode dial on the request (letterbox is the default the compositor already
  implements); adding one later is additive wheel surface, not plan text.
- Health/metrics naming processor-owned windows — deliberately not taken in the align.
- The Apple main-thread pump; `packages/display` (pre-pivot consumer, lags by design).
