# One monotonic clock

Rationale for the `[one-monotonic-clock]` entry in `docs/plan/ARCHITECTURE.md`
§Media I/O, decided 2026-08-03.

## Trigger

Read this before adding any timestamp source, epoch offset, or clock export in any
language, and before writing code that assumes a timestamp starts near zero.

## Decision

One concept — the machine's monotonic clock — in every language the project speaks, on
the data plane. Scoped by the owner (2026-08-03) to what a processor stamps, reads, or
compares: frames, bags, audio ticks, `ctx.time`. Wall clock survives on exactly three
observability surfaces — log record `host_ts` and `source_ts`, and log file naming —
because correlating with the outside world and
with other hosts' logs is a job monotonic time cannot do. Everything else is monotonic;
a wall-clock value never enters the data plane and is never compared against a media
timestamp. Adding a further wall-clock surface is a plan change, not a judgement call, and
`cargo xtask check-clock-usage` enforces the list mechanically.

> ~~A fourth surface, the control-plane pubsub event timestamp, also keeps wall clock.~~
> — Superseded 2026-08-13 by #1783. The control-plane event bus became an in-process
> registry, so its events no longer cross a wire and carry no timestamp to stamp. The
> surface ceased to exist rather than being retracted.
Timestamps are raw `clock_gettime(CLOCK_MONOTONIC)` on Linux and `mach_absolute_time`
on Apple: the same epoch V4L2 and ALSA stamp their buffers with, and the same value any
other process on the host would read. No process-relative epoch, and exactly one
exported name per language.

`MediaClock` remains the cross-platform seam — Rust's std exposes no raw monotonic
clock, so something has to name it — but it is a naming boundary, not a second epoch.

## Rejected alternatives

- **Process-start-relative epoch** (the Linux `MediaClock` behavior) — destroys
  comparability between a frame's driver stamp and the engine's own, between processes
  on one host (helper-only placement, 2026-08-04, makes this *every* data-plane
  timestamp comparison, not an occasional one — this decision is a hard prerequisite of
  helper placement, not a cleanup), and between nodes. It also diverged
  from its own macOS twin, which was already machine-wide.
- **Two exported clock names** (`media_clock_now_ns` alongside `monotonic_now_ns`) — two
  names for one number invite the reader to assume they differ.

## Consequences

- Call sites that read a timestamp as "seconds since start" are wrong and must subtract
  their own baseline explicitly.
- The wheel's duplicate export is deleted once the epochs match.
- A/V sync, when designed, starts from a single comparable timebase shared with drivers.
