# Change: shared-window-event-pump

Moves ownership of the process's one winit event loop from the built-in display processor to
the engine, so N window-owning processors can coexist in one process. Implements #1734.

Scale tier: change artifact only — no ADR. The shape was decided by the owner in #1734's body
("one engine-owned event pump that window-owning processors register windows with"); what this
file records is the plan-text delta that shape implies, which #1734's own staleness comment
(2026-08-23) marked for the announce gate. Owner ruled 2026-08-23 that the amendment rides the
implementation PR rather than a separate `/propose-change` cycle.

## Why the plan text has to move

winit permits one `EventLoop` per process — `EVENT_LOOP_CREATED` is a process-global
`AtomicBool` and a second `build()` returns `RecreationAttempt` for the life of the process,
whether or not the first loop was dropped. §Media I/O's Windowing entry currently reads "the
built-in display owns window creation and **the event pump**", which is true only while
exactly one display exists. A per-processor retry cannot work; the loop has to be owned once,
above every processor that wants a window.

The layer it moves to is forced, not chosen. Three callers can never reach a pump that lives
in `streamlib-media-builtins`, because that crate sits below the SDK:

- a third-party or user-authored Rust processor, which depends on the `streamlib` crate only;
- the engine's escalate path, which is where a Python processor's window request would be
  served (`[helper-process-placement-only]` puts the window app-process-side);
- `rt.run()`, whose blocking body is engine code on the user's main thread — the only place a
  future Apple main-thread pump can live.

## What the engine owns, and what it does not

The pump owns the scarce process-global resource and the routing from a window's events to the
processor that registered it. Window policy stays with the processor: it supplies the title and
extent, decides what a resize means, decides when to redraw, and decides what closing does. The
raw-window-handle seam is untouched — the present target is still minted from a raw handle by
`GpuContextFullAccess::create_present_target`, which is generic over `HasWindowHandle +
HasDisplayHandle` and never sees winit.

`media-io-layering.md`'s rejection of **Engine-owned windows** is narrowed rather than reversed,
and the narrowing concedes a real part of it: winit event *reception* and *routing* do move into
the engine core. What stays out is window *policy* and input *semantics* — the pump forwards
resize and close and discards the rest uninterpreted, and the engine reads no input and draws
nothing. The engine already linked winit and already exposed `winit::window::Window` in a public
signature (`core/display_info.rs`, re-exported by the SDK) before this change.

Rendering does not move to the pump thread. Each window's owner renders on its own thread, so
two displays are not serialised behind one render loop. Note the limit of that claim: the
displays still share one `VkDevice` and its queues, so whether a vsync-blocked acquire on one
swapchain can hold a lock the other needs is an RHI question this change does not answer. What
is measured is that two windows fed by one source each presented within one frame of the other
over a 30 s run.

## Plan delta

### §Media I/O — camera, display, audio

- MODIFIED: the Windowing DECIDED entry. "The built-in display owns window creation and the
  event pump" becomes: the engine owns the process's one event pump and mints windows on
  request; window-owning processors own window policy and register with it. The
  raw-window-handle seam, engine ownership of every swapchain and acquire detail, and the
  `rt.run()`-blocks-while-the-engine-pumps clause are unchanged.

### §Graphics (RHI / GPU)

- ADDED: nothing. The pump mints no surface, records no Vulkan work, and adds no RHI surface.

## Behavior after this change

Two `rt.add(DisplayWindow)` in one graph each show live frames in their own window. A second
registration is accepted, not degraded. A processor that cannot get a window — no display
server, or a non-winit consumer already took the process's one loop — drains and discards so
upstream still sees a live consumer, which is the failure mode #1734 asked to preserve and
which a failed event loop did not previously reach.

## REMOVED

- REMOVED: DisplayWindowEventLoopHandler
  The display's own `winit::application::ApplicationHandler` — the per-processor event loop
  and the render-inside-`RedrawRequested` cadence that came with it. Its disappearance is the
  proof that no processor builds an event loop of its own: the built-ins crate no longer
  depends on winit or raw-window-handle at all, so there is nothing left there to build one
  with. The engine keeps exactly one construction site, in the pump.

## Out of scope

- Python-facing and third-party window/present-class resource requests — #1731's align. This
  change makes the seam reachable from the engine layer; it exposes nothing new to Python.
- A cross-process pump. Under `[helper-process-placement-only]` a Python window-owning
  processor's window is minted app-process-side; the pump serves native built-ins and the app
  process only.
- The Apple main-thread pump. Post-MVP and undesigned; this change only avoids foreclosing it.
- `packages/display`, which carries its own winit — a pre-pivot consumer that lags by design.
