# Media I/O: packages over engine primitives

Rationale for the `[media-io-layering]` entries in `docs/plan/ARCHITECTURE.md`
§Media I/O, decided 2026-07-30.

## Trigger

Read this before adding a media processor (camera, display, audio) to the engine tree,
before duplicating a hardware primitive inside a package, before scheduling media
package upgrades against engine milestones, or before starting any non-V4L2 capture
work.

## Decision

> ~~The engine owns hardware primitives; media processors are packages. The primitives
> live behind `GpuContext`, the RHI, and the ABI vtables, and packages reach them only
> across the plugin ABI. No media processor is engine-internal.~~ — Superseded
> 2026-08-02 by `importable-python-library.md`. The plugin ABI is deleted; first-party
> camera, display, and audio are native built-in processors in the engine tree,
> statically linked into the wheel. The *layering half* survives as internal
> discipline: built-ins are written against the same handle-shaped primitives
> (DMA-BUF / OPAQUE_FD, present target, audio clock, color resolution, codec sessions)
> third parties get — never against private engine guts.
> — Narrowed again 2026-09-04 by `extension-model.md`: "first-party media is a built-in"
> holds for the twelve that shipped and is no longer a rule; the layering half is now the
> contract extension wheels build on too.

> ~~First-party media packages lag by design while the engine moves, then upgrade to
> the current engine exactly once, as the final MVP step.~~ — Superseded 2026-08-02 by
> `importable-python-library.md`. Built-ins ship inside the wheel, current by
> construction; lag-by-design ends for media.

Capture is V4L2-only. Apple capture (AVFoundation) stays undesigned until a milestone
traces to it; only the TCC permission shims exist.

Windowing splits at the raw window handle: ~~the package~~ ~~the built-in display
processor (since 2026-08-02, per `importable-python-library.md`) creates and owns the
window~~ — Superseded 2026-08-23 by `shared-window-event-pump.md` (#1734): winit permits
one event loop per process, so a second window-owning processor could never build one.
The engine owns the process's one event pump and mints windows on request; the
registering processor keeps every window policy decision. The engine mints the present
target from the raw handle and keeps every
swapchain and acquire detail host-side, plus the platform main-thread event loop where
the OS demands it. Camera-to-GPU transport is zero-copy DMA-BUF import when the device
exports it, with a transparent CPU-upload fallback chosen automatically — no
configuration dial.

~~Audio backend stays open with a stated intent: PipeWire-native on Linux, the current
CPAL-over-ALSA path interim, pending a research memo. The engine's decided audio
surface is the clock primitive.~~ — Superseded 2026-08-26 by `audio-subsystem.md`: the
memo was written and the OPEN closed — PipeWire-native via runtime dlopen, dlopen'd
ALSA fallback, null backend; CPAL is gone; the audio surface grew past the clock
primitive to the full `[audio-subsystem]` entry set in §Media I/O.

## Rejected alternatives

- ~~**Engine-internal media processors** — breaks engine purity, couples driver churn
  to engine releases, and demotes the plugin ABI to a second-class path the moment
  first-party code bypasses it.~~ — Superseded 2026-08-02 by
  `importable-python-library.md`: this is now the chosen shape. The rejection rested
  on the plugin ABI existing to be demoted; with the ABI deleted and media shipping
  inside the wheel, the purity concern is met by the internal layering wall instead.
- **Continuous lockstep upgrades of media packages** — churns consumers on every
  engine change before the design has settled; a single end-of-milestone upgrade
  proves the design once, against a stable runtime.
- **Scaffold-generated media processors (no first-party packages)** — turns every
  scaffolded app into a fork of driver code nobody upgrades; capture/display fixes
  would never reach existing apps.
- **Deciding AVFoundation capture now** — no MVP trace (the floor is Linux + NVIDIA);
  designing it without a driving consumer violates plan-first.
- **Engine-owned windows** — drags winit event handling, input, and window policy into
  the engine *core* for one consumer's convenience; the raw-window-handle seam already
  gives the engine everything the swapchain needs. — Scope narrowed 2026-08-02 by
  `importable-python-library.md`: the built-in display block (engine tree, wheel) now
  owns window creation and the event pump, but the raw-window-handle seam between
  windowing code and the present target stands; the engine *core* still never owns
  window policy. — Narrowed again 2026-08-23 by `shared-window-event-pump.md` (#1734):
  the *event pump* moves to the engine, because winit's one-loop-per-process guard
  makes per-processor ownership work only for a graph with exactly one window, and a
  pump below the SDK is unreachable from third-party processors, from the escalate path
  that would serve a Python window request, and from `rt.run()` where an Apple
  main-thread pump must live. Be exact about the price, because part of
  this alternative's objection is now conceded: winit event *reception* and *routing*
  do move into the engine core — the `ApplicationHandler` and the `WindowEvent` match
  live there, and every window's events arrive at the engine first. What stays out is
  what the rejection was really protecting: window *policy* and input *semantics*. The
  pump forwards resize and close to the window's owner and discards the rest without
  interpreting any of it; title, extent, resize meaning, redraw cadence and close
  behaviour are all decided by the registering processor, and rendering stays on that
  processor's own thread. The engine reads no input and draws nothing. The engine already linked winit and already exposed `winit::window::Window` in
  a public signature (`core/display_info.rs`) before this change.
- **Zero-copy required (no CPU fallback)** — kills virtual and test devices (vivid,
  v4l2loopback) and any driver without DMA-BUF export; a user-visible transport dial
  would break the zero-ceremony bar.
- **Committing to PipeWire (or CPAL) today** — audio has no MVP trace and the
  realtime claims are unmeasured; committing without a research memo is the kind of
  inference-from-vibes the plan exists to prevent.

## Consequences

- ~~MVP completion includes a final consumer-upgrade pass over the media packages and
  the scaffold's effect chain.~~ — Superseded 2026-08-02: built-ins live in the engine
  tree; there is no upgrade pass because there is no lag.
- ~~Engine changes may break media packages mid-development; that is expected, not a
  defect, and files no tickets.~~ — Superseded 2026-08-02: media built-ins move with
  the engine in the same tree and the same PRs.
- ~~The plugin ABI must carry every capability media processors need.~~ — Superseded
  2026-08-02: the ABI is deleted; the equivalent discipline is that the handle-shaped
  primitive surface must carry every capability *third-party* media bindings need.
- Non-Linux capture waits, including for contributors on Apple hardware.
