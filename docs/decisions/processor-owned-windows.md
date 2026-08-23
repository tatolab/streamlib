# Processor-owned windows

Rationale for the `[processor-owned-windows]` entries in `docs/plan/ARCHITECTURE.md`
§Media I/O, decided by the owner 2026-08-23. Carries forward the owner intent recorded
during discovery: processors are little programs with resource-request autonomy.

## Trigger

Read this before exposing any window or present-class capability to a processor, before
adding a second present-loop mechanism beside the engine's, before proposing that Python
drive presentation per frame, or when someone asks why a pip-installed processor can
bring its own debug window.

## Decision

1. **Window ownership is a processor capability.** Any processor may request a window
   from the engine and own its policy — title, extent, which frame it shows, what close
   means. Before this, the only possible window owner was the native built-in display;
   a pip-installed processor wanting a debug view needed the app author to wire the
   built-in downstream by hand, which is the opposite of the autonomy intent.
2. **The engine runs the present loop for owners that cannot.** The engine mints the
   window (pump registration + present target) and, for an owner whose code cannot sit
   in the app process, runs that window's native vsync present loop itself. The owner's
   per-frame involvement is naming a published surface id — a camera-class-cadence
   message that fits the helper hop. The loop presents latest-wins: several ids between
   two vsyncs and the newest shows; none, and the last frame stays up. The standing
   §Media I/O constraint is untouched: the vsync deadline never crosses a process
   boundary — a deadline argument, not a trust argument.
3. **The compositor stays engine-internal.** `create_present_compositor` keeps no
   cross-process spelling and no Python name; it is plumbing inside the engine's own
   present loop. At this capability surface, "present-class" means windows.
4. **One present-loop machinery.** The built-in display and every processor-owned window
   drive the same engine-owned loop shape through different feeders — the built-in fed
   by its input port, a processor-owned window fed by ids named across the escalate
   path. No second present system.
5. **One request seam, one cross-language delta.** Every processor uses the same window
   request; the only difference between languages is where the loop runs. A native owner
   may instead drive its own render thread against its present target — the deadline
   constraint does not bind app-process code, and `DisplayWindow` is the existence
   proof. This is the Python-parity disposition for windows: nothing is capability-gated,
   the loop placement is.
6. **Setup-only minting, one accepting verb.** The window is requested in `setup()`
   where the typestate is Full — the same grammar as kernels: constructed in setup,
   driven in process — and released at teardown or with its processor. The per-frame
   verb accepts anything that names a published surface: the cast object (whose claim
   guarantees the id un-recycled), a kernel-output handle, or a bare surface id as the
   escape hatch.
7. **Events are polled, coalesced state; defaults are benign.** The pump forwards
   exactly two events (resize, close-request), already coalesced; the owner reads them
   off the window object in `process()`. Unread resize just works — the engine owns
   every swapchain detail. An unread close-request closes the window; afterwards the
   per-frame verb is a no-op and the window reports closed. A user gesture never takes
   down a pipeline; loud refusal is reserved for programming errors.
8. **A refused request raises at `setup()`.** No display server, a dead pump, a refused
   registration — the request raises, and an author who considers the window optional
   writes the `try/except`. The built-in's drain-and-discard fallback exists to keep
   upstream seeing a live consumer; a processor-owned window has no port of its own, so
   silent degradation would only hide the refusal.
9. **Invisible to topology.** The window is a processor resource, not a graph node:
   nothing new in `graph` or `tap`. Whether health/metrics ever names it is a later
   question, deliberately not taken here.

## Rejected alternatives

- **Built-ins-only (no new capability).** A processor wanting a debug view adds a
  `DisplayWindow` downstream. Cheapest, but it retracts the resource-request autonomy
  recorded on #1709 — the window belongs to the graph author, not the processor, and a
  pip-installed processor cannot bring its own view without asking the app to rewire.
- **Raw present-target reach for Python.** Exposing `create_present_target` across the
  hop and letting Python drive present per frame puts the vsync loop behind the helper
  hop, against the standing §Media I/O DECIDED that present loops stay native, always.
  Not offerable.
- **Exposing the compositor.** No consumer needs to name it: under the engine-run loop
  the compositor is an implementation detail of that loop, and its only live caller
  today is the built-in display. A cross-process compositor spelling would be surface
  area with no capability behind it.
- **Event callbacks across the hop.** A callback surface would put engine-driven entry
  points into user Python mid-`process()` and a delivery contract across the process
  boundary, for two events whose useful shape is "the latest state". Polled coalesced
  state matches how the pump already hands events to native owners.
- **A raise (or error state) on `show()` after user close.** Closing a debug window is
  a user gesture, not a programming error; raising would make every window owner wrap
  its per-frame verb defensively or crash the pipeline on a click.
- **A silently degraded window on a refused request.** Returning a never-shows window
  keeps headless apps running but hides the refusal from the author who wanted the
  window; the author who considers it optional can state that in one `try/except`.

## What a cross-process owner gives up

Only custom native rendering inside the present loop — per-vsync compositing logic. It
names frames; the engine blits them. That is the deadline constraint applied to the
loop, not a capability tier: the frames it names can be anything it produced, kernel
outputs included.

## Consequences

- The escalate surface grows a present-class pair: the window request and the per-frame
  naming. The compositor gains no wire spelling.
- The engine hosts a native present loop per cross-process-owned window; the built-in
  display's loop machinery becomes a shared engine seam rather than processor-private
  code.
- The wheel gains a window object: setup-time request, per-frame naming verb, polled
  coalesced events, a closed indicator.
- Cost accepted: a cross-process owner cannot custom-composite per vsync — it names
  whole frames, and anything fancier is GPU work it does upstream of naming.
