# One monotonic clock

Rationale for the `[one-monotonic-clock]` entry in `docs/plan/ARCHITECTURE.md`
§Media I/O, decided 2026-08-03.

## Trigger

Read this before adding any timestamp source, epoch offset, or clock export in any
language, and before writing code that assumes a timestamp starts near zero.

## Decision

One concept — the machine's monotonic clock — in every language the project speaks.
Timestamps are raw `clock_gettime(CLOCK_MONOTONIC)` on Linux and `mach_absolute_time`
on Apple: the same epoch V4L2 and ALSA stamp their buffers with, and the same value any
other process on the host would read. No process-relative epoch, and exactly one
exported name per language.

`MediaClock` remains the cross-platform seam — Rust's std exposes no raw monotonic
clock, so something has to name it — but it is a naming boundary, not a second epoch.

## Rejected alternatives

- **Process-start-relative epoch** (the Linux `MediaClock` behavior) — destroys
  comparability between a frame's driver stamp and the engine's own, between processes
  on one host (helper placement makes this routine), and between nodes. It also diverged
  from its own macOS twin, which was already machine-wide.
- **Two exported clock names** (`media_clock_now_ns` alongside `monotonic_now_ns`) — two
  names for one number invite the reader to assume they differ.

## Consequences

- Call sites that read a timestamp as "seconds since start" are wrong and must subtract
  their own baseline explicitly.
- The wheel's duplicate export is deleted once the epochs match.
- A/V sync, when designed, starts from a single comparable timebase shared with drivers.
