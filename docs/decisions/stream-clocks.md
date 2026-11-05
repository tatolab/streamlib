# Stream clocks

Rationale for the `[stream-clocks]` entry in `docs/plan/ARCHITECTURE.md` §Processor model &
scheduling, decided 2026-11-03. Builds on `one-monotonic-clock.md`, which still holds: every
stamp is in the machine's monotonic epoch.

## Trigger

Read this before adding a time mode to a single node, before pacing a simulation with sleeps,
or before rendering a stream faster or slower than real time.

## Decision

Each stream runs on one stream clock over `MediaClock`: **real** time (the default), **scaled**
(`base + scale × elapsed`), or **stepped**. Stepped and faster-than-real-time run behind a
cross-process step barrier: the clock advances only once every node finished the step. A
nested `@stream` may carry its own clock.

> ~~Device sources and live output stay on real time; a stepped stream renders to a file or a sim
> view.~~ — Superseded 2026-11-05 by the shipped change: a node declaring `paced_by_real_time`
> loads in any stream, paces on the machine clock and stands outside the step barrier.

## Why

- **Uses that need it:** Kerbal-style time warp (scaled), FMU co-simulation across nodes
  (stepped, the FMI barrier), deterministic graph tests in CI, faster-than-real-time training
  and evaluation, inference over recordings with no dropped frames, offline-quality renders and
  frame-exact export, and templated or personalised video at scale.
- **What works without it does not reach these.** A fixed timestep paced to real time with sim
  time in the payload lets one part rewind while the rest stays live, but nothing can run
  faster than real time or wait for the slowest node.
- **Per stream, not per node.** One clock per stream keeps a graph's nodes agreeing on time;
  a nested `@stream` gives local time where it is wanted, as TouchDesigner's components do.

## Rejected alternatives

- **A clock per node** — nodes in one stream disagree on time, and every link has to translate.
- **Layering modes onto the real-time clock by convention** — no barrier, so stepped and
  faster-than-real-time cannot be honest.

## Consequences

- Rendering a stream to a file also needs a file source and an MP4 file sink.
> ~~How a stepped stream that holds a live device behaves is left to the change that builds
> this.~~ — Superseded 2026-11-05 by the shipped change: a device node loads in any stream, its
> writes carry stream time, the step barrier neither waits on it nor balances its links, and what
> a step reads from it follows the reading port's delivery setting.
- A scaled clock above 1× runs behind the step barrier, paced at its scale; at or under 1× it
  runs free.
