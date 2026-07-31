# Media I/O: packages over engine primitives

Rationale for the `[media-io-layering]` entries in `docs/plan/ARCHITECTURE.md`
§Media I/O, decided 2026-07-30.

## Trigger

Read this before adding a media processor (camera, display, audio) to the engine tree,
before duplicating a hardware primitive inside a package, before scheduling media
package upgrades against engine milestones, or before starting any non-V4L2 capture
work.

## Decision

The engine owns hardware primitives; media processors are packages. The primitives —
DMA-BUF / OPAQUE_FD import and export, the present target, the audio clock, color
resolution, codec sessions — live behind `GpuContext`, the RHI, and the ABI vtables,
and packages reach them only across the plugin ABI. No media processor is
engine-internal.

First-party media packages lag by design while the engine moves, then upgrade to the
current engine exactly once, as the final MVP step — the Next.js model: prove the
runtime solid first, then bring consumers forward before release.

Capture is V4L2-only. Apple capture (AVFoundation) stays undesigned until a milestone
traces to it; only the TCC permission shims exist.

Windowing splits at the raw window handle: the package creates and owns the window;
the engine mints the present target from the raw handle and keeps every swapchain and
acquire detail host-side, plus the platform main-thread event loop where the OS
demands it. Camera-to-GPU transport is zero-copy DMA-BUF import when the device
exports it, with a transparent CPU-upload fallback chosen automatically — no
configuration dial.

Audio backend stays open with a stated intent: PipeWire-native on Linux, the current
CPAL-over-ALSA path interim, pending a research memo. The engine's decided audio
surface is the clock primitive.

## Rejected alternatives

- **Engine-internal media processors** — breaks engine purity (the engine's own
  manifest declares no domain packages), couples driver churn to engine releases, and
  demotes the plugin ABI to a second-class path the moment first-party code bypasses
  it.
- **Continuous lockstep upgrades of media packages** — churns consumers on every
  engine change before the design has settled; a single end-of-milestone upgrade
  proves the design once, against a stable runtime.
- **Scaffold-generated media processors (no first-party packages)** — turns every
  scaffolded app into a fork of driver code nobody upgrades; capture/display fixes
  would never reach existing apps.
- **Deciding AVFoundation capture now** — no MVP trace (the floor is Linux + NVIDIA);
  designing it without a driving consumer violates plan-first.
- **Engine-owned windows** — drags winit event handling, input, and window policy into
  the engine for one consumer's convenience; the raw-window-handle seam already gives
  the engine everything the swapchain needs.
- **Zero-copy required (no CPU fallback)** — kills virtual and test devices (vivid,
  v4l2loopback) and any driver without DMA-BUF export; a user-visible transport dial
  would break the zero-ceremony bar.
- **Committing to PipeWire (or CPAL) today** — audio has no MVP trace and the
  realtime claims are unmeasured; committing without a research memo is the kind of
  inference-from-vibes the plan exists to prevent.

## Consequences

- MVP completion includes a final consumer-upgrade pass over the media packages and
  the scaffold's effect chain — that pass is MVP work, not optional backlog.
- Engine changes may break media packages mid-development; that is expected, not a
  defect, and files no tickets.
- The plugin ABI must carry every capability media processors need — there is no
  side door, so a missing primitive is engine work, never an ABI bypass.
- Non-Linux capture waits, including for contributors on Apple hardware.
