# stream-clocks

> **Asked for by the owner, 2026-11-04:** "Run the Clocks milestone end to end: how a stream picks
> its clock, what a node reads, how scaled and stepped time reach helpers in their own
> processes, and how timestamps on ports follow the stream clock. Timekeeping stays monotonic
> everywhere." Builds the `[stream-clocks]` entry (§Processor model & scheduling,
> `ARCHITECTURE.md:1348-1355`) and `docs/decisions/stream-clocks.md`.

Today every stream runs on the machine's monotonic clock and nothing else. `ctx.time`, the
implicit write stamp and a continuous node's pacing all read `MediaClock::now()` directly, so a
stream can neither run slower or faster than real time nor wait for its slowest node.

**After this change:**
- **A stream names its clock once, in `stream.py`.** `stream_builder.run_on_clock(...)` takes a
  `RealTimeStreamClock()` (the default, and what every stream runs on today), a
  `ScaledStreamClock(scale=…, step_ns=…)` or a `SteppedStreamClock(step_ns=…, pace=…,
  stop_after_steps=…)`. The choice travels in the graph document as `clock`, so `graph` shows
  it and a loaded graph keeps it.
- **A node reads stream time.** `ctx.time` is the stream clock's now, in every process; in a
  real-time stream that is exactly the `MediaClock` reading it is today. `ctx.stream_clock`
  says which clock the stream runs on. `monotonic_now_ns()` and `start_monotonic_timer` stay the
  machine's clock, for latency and log correlation.
- **Ports follow the stream clock.** A write without `timestamp_ns` is stamped with stream time;
  in a stepped stream every write of step *k* carries the step's time. A continuous node fires
  at absolute deadlines on stream time, never by sleeping.
- **Stepped time is honest across processes.** The clock advances to step *k+1* only once every
  node of the stream, native or in its own helper process, finished step *k* and no message of
  step *k* is still in flight — FMI co-simulation's barrier.
- **Time warp is live.** A scaled stream's scale is changed while it runs, from the control
  plane.
- **What a stream author sees:** one new builder call, three clock classes and one context
  property. A real-time stream behaves byte-for-byte as today.

**Scale gate — this skill plus an ADR.** It changes the processor model's scheduling, the helper
lifecycle protocol, the shared-memory wire (a new `#[repr(C)]` blackboard entry) and the Python
API's public contract. The ADR exists (`docs/decisions/stream-clocks.md`); its "left to the change
that builds this" consequence is settled by Decision B below and recorded there at ship.

**Precondition.** The `[stream-clocks]` entry is DECIDED (owner, 2026-11-03). The one-clock entry
(§Media I/O, `:2366-2393`), the reactive/continuous pacing entry (§Processor model,
`:1356-1374`), the helper-placement entry (`:1375-1395`) and the per-spawn loss-count board are
DECIDED and SHIPPED.

## Current state (recon, 2026-11-04, `main` `3182cd0`)

- **One clock, read directly.** `MediaClock::now()` (`core/media_clock.rs:11-25`, Apple arm
  `apple/media_clock.rs`) is the only time source. A per-stream `TimeContext` already exists —
  built at each stream's start (`core/runtime/loaded_stream.rs:447`) and held as
  `RuntimeContext.time` (`core/context/runtime_context.rs:56`) — but it only wraps
  `MediaClock::now()`, and no processor reads it.
- **What a Python node reads.** `ctx.time` is `monotonic_clock_now_ns()`
  (`sdk/streamlib-python-wheel/src/python_processor_context/runtime_context.rs:234-237`, `:358`),
  declared by `_RuntimeContextShared.time` (`tatolab/stream/_node_context_protocols.py:185`). The
  scaffold paces its `BrightnessMeter` and `HalftoneLensEffect` on it
  (`scaffold_template/nodes/brightness_meter.py:33-48`).
- **Implicit stamps.** Native `write_msgpack` stamps `MediaClock::now()`
  (`iceoryx2/output.rs:669`); a Python write without `timestamp_ns` stamps
  `monotonic_clock_now_ns()` (`python_processor_link_data_access.rs:951`).
- **Continuous pacing.** The native runner sleeps `interval_ms` between calls
  (`core/execution/thread_runner.rs:115-157`, the sleep at `:154`). A helper paces on a
  `MonotonicTimer` (`_processor_interpreter_bootstrap.py:1453-1471`), and learns its mode from
  the `run` command (`processor_interpreter_spawn_host.rs:362-369`).
- **The stream's spec.** `StreamBuilder._compiled_graph` writes `stream`, `nodes`, `links`,
  `exposed` (`_stream_graph_builder.py:404-420`); `GraphSnapshot` admits exactly those spec keys
  (`core/graph_snapshot.rs:76-81`). `@stream` takes no arguments by design (`:79-98`).
- **Shared state across processes.** The per-spawn loss-count board is an iceoryx2 blackboard
  (`iceoryx2/node.rs:422-475`, `iceoryx2/helper_process_loss_count_board.rs`) carrying
  `OutputPortBagCountBoardEntry.written_bags` per output port and
  `InboundLinkLossCountBoardSlot.dropped_bags` per inbound link
  (`streamlib-ipc-types/src/lib.rs:738-765`). Nothing counts what a link's reader took.
- **Self-paced nodes.** `RelayTrackSource` is `execution = manual`
  (`streamlib-media-builtins/src/relay_track_source.rs:132`); camera, microphone, display and
  audio output are first-party modules in `packages/`, each pacing itself on its device.

## ADDED

- **ADDED (§Processor model & scheduling, DECIDED):** A stream names its clock in its spec. The
  graph document gains a spec key `clock`, absent meaning real time:
  `{"mode": "real"}`, `{"mode": "scaled", "scale": <f64 > 0>, "step_ns": <u64>}` or
  `{"mode": "stepped", "step_ns": <u64 > 0>, "pace": <f64 > 0 | null>, "stop_after_steps":
  <u64 | null>}`. Python spells it `stream_builder.run_on_clock(RealTimeStreamClock() |
  ScaledStreamClock(scale, step_ns=16_666_667) | SteppedStreamClock(step_ns, pace=None,
  stop_after_steps=None))`, called at most once per stream function; the three are frozen
  dataclasses exported from `tatolab.stream`, refusing a non-positive scale or step at
  construction. `graph` renders the key and `GraphSnapshot` loads it, so a recorded graph keeps
  its clock. A stream's mode is fixed for its load; changing mode is a re-run. A nested
  `@stream` takes its own `run_on_clock` when subgraphs land; until then a stream has one
  clock. [stream-clocks]
- **ADDED (§Processor model & scheduling, DECIDED):** The engine's `StreamClock` replaces
  `TimeContext` as `RuntimeContext.stream_clock`, one per loaded stream, built at its start.
  Stream time is in the machine's monotonic epoch and derived only from `MediaClock` readings
  and step counts — never a wall clock, never a sleep:
  - **real:** `now = MediaClock::now()`;
  - **scaled, free-running:** `now = base + scale × (MediaClock::now() − anchor)`, where a scale
    change re-anchors (`base ← now`, `anchor ← MediaClock::now()`) so time never jumps or goes
    backwards;
  - **stepped:** `now = base + k × step_ns` for the open step *k*, where `base` is the
    `MediaClock` reading at the stream's start.
  [stream-clocks]
- **ADDED (§Processor model & scheduling, DECIDED):** Each non-real stream has one **stream clock
  board**, an iceoryx2 blackboard the engine creates at the stream's start and every one of its
  helpers opens read-only by the service name its `setup` command carries, beside the loss-count
  board. One key holds one `#[repr(C)]` `StreamClockBoardEntry { mode, step_index, step_ns,
  base_ns, anchor_ns, scale_f64_bits, pace_f64_bits }` in `streamlib-ipc-types`, with a layout
  regression test, so a reader always copies a consistent clock. A real-time stream creates no
  board and its helpers read `MediaClock` directly, so the default path gains nothing. A step
  opening is announced on one iceoryx2 event service per stream, whose listener fd a helper and
  a native runner wait on. [stream-clocks]
- **ADDED (§Processor model & scheduling, DECIDED):** The step barrier. In a stepped stream the
  engine's step coordinator, one thread per stream, opens step *k* by writing the board and
  notifying. A continuous node runs `process()` once in step *k* when its next deadline
  `first + j·interval` on stream time is ≤ *t_k*; a reactive node runs on input as always. Every
  node publishes, on its own entry of a **step ledger** blackboard per stream,
  `{last_step_finished, callbacks_in_flight, bags_taken per inbound link}`, written after each
  callback returns, and the ledger's link section is the loss-count board's own entries extended
  by `taken_bags`. The coordinator closes step *k* when two consecutive reads of the ledger
  agree and show every node idle, every continuous node finished *k*, and every link's
  `taken_bags + dropped_bags` equal to its producer's `written_bags`. It then opens *k+1*
  immediately (`pace = null`, as fast as the graph allows) or no earlier than `anchor + (k+1) ×
  step_ns / pace` on `MediaClock`, waiting on an absolute deadline. A stream with
  `stop_after_steps = n` stops itself after closing step *n*, which is how a CI graph test
  gets a deterministic end. A helper that dies mid-step leaves its node not idle; the step stays
  open until the restart's `setup` re-registers the node, and a node given up on fails the
  stream with the step it held. A manual node, which paces itself, is refused at load in a
  stepped stream, naming the node. [stream-clocks]
- **ADDED (§Processor model & scheduling, DECIDED):** A node that paces itself on real time
  declares it — `paced_by_real_time=True` on `@node`, `paced_by_real_time` in the
  `#[processor]` attribute — and the node catalog shows it. `RelayTrackSource` and the camera,
  microphone, audio-output and relay-publish nodes in `packages/` declare it; a display does not,
  being a simulation view that presents what arrives. Where such a node may run is Decision B.
  [stream-clocks]
- **ADDED (Python surface, `tatolab.stream`):** `RealTimeStreamClock`, `ScaledStreamClock`,
  `SteppedStreamClock`, `StreamBuilder.run_on_clock`, and a `StreamClockReading` Protocol on
  `_RuntimeContextShared.stream_clock` with `mode: Literal["real", "scaled", "stepped"]`,
  `scale: float` (1.0 when real), `step_ns: int | None` and `step_index: int | None`; its
  native class is held to it by the conformance gate. [stream-clocks]
- **ADDED (§Control plane & observability, DECIDED):** One control verb,
  `set_stream_clock_scale(stream, scale)`, on the MCP tools, the HTTP route table and `tatolab
  clock <stream> --scale <f64>`. It re-anchors a scaled stream's clock and refuses a real or
  stepped stream by name. A node never changes its stream's clock. [stream-clocks]

## MODIFIED

- **MODIFIED (§Processor model, the `[stream-clocks]` entry, `:1348-1355`):** folds the
  ADDED entries above and Decisions A and B; "every stamp is still read from the machine's
  monotonic clock" reads "every stamp is in the machine's monotonic epoch, derived from
  `MediaClock` readings and step counts alone".
- **MODIFIED (§Processor model, reactive and continuous pacing, `:1356-1374`):** a continuous
  node fires at `first + k·interval` on **stream time**. The native runner waits on that
  absolute deadline — converted to a `MediaClock` deadline in a real or scaled stream, the step
  event in a stepped one — instead of sleeping. A helper's continuous loop does the same through
  `MonotonicTimer`, re-armed when the scale changes, or the step event's fd. `MonotonicTimer`
  itself stays on the machine clock.
- **MODIFIED (§Media I/O, the one-clock entry, `:2366-2393`):** "every timestamp a processor
  stamps, reads, or compares — frames, messages, audio ticks, `ctx.time` — is the machine's
  monotonic clock" reads "is in the machine's monotonic epoch: a device stamp is the machine
  clock itself, and `ctx.time` and the implicit write stamp are the stream clock, which equals it
  in a real-time stream." `monotonic_now_ns` stays the one name for the machine's clock.
- **MODIFIED (§Processor model, the helper lifecycle):** the `setup` command carries
  `stream_clock: {mode, board_service_name, step_event_service_name}` (`null` names absent when
  real); `run` keeps `execution` and `interval_ms`. A helper-protocol change, made safe by the
  build id the helper already checks.
- **MODIFIED (§Product, the scaffold entry, `:95-115`):** the `BrightnessMeter` is "paced on
  `ctx.time`, the stream's clock"; its comment and the `HalftoneLensEffect`'s stop calling
  `ctx.time` "the monotonic clock every node shares".
- **MODIFIED (engine code):**
  - `RuntimeContext.time: Arc<TimeContext>` → `RuntimeContext.stream_clock: Arc<StreamClock>`;
    `loaded_stream.rs:447` builds it from the snapshot's `clock`.
  - `OutputWriterInner` holds the stream's `Arc<StreamClock>` from wiring, and `write_msgpack`
    and every implicit-stamp write read it (`output.rs:669`).
  - The wheel's `ctx.time` and implicit stamp read a `HelperStreamClockReader` opened from the
    `setup` command; `monotonic_clock_now_ns` stays for `monotonic_now_ns()`, logging and the
    timer.
  - `GraphSnapshot` and its key table gain `clock`; `StreamBuilder` gains `run_on_clock`.

## REMOVED

- REMOVED: TimeContext
- REMOVED: time_context.rs
- REMOVED: sleep_duration

  `TimeContext` is replaced by `StreamClock`, not kept beside it. `sleep_duration` is the native
  continuous runner's sleep (`thread_runner.rs:121-154`), replaced by the deadline wait. The
  scaffold's and README's "the monotonic clock every node shares" is reworded in the engine tree;
  `examples/raytracing-showcase` keeps the phrase, being a consumer outside the migration.

## [NEEDS DECISION] A — how a scaled stream runs faster than 1×

The plan says scaled time is `base + scale × elapsed` and that faster than real time runs behind
the step barrier. Those meet at a scale above 1, and the scale changes live.

- **A1 (recommended) — barrier above 1×, free at or below.** At or below 1× a scaled stream runs
  free, as above. Above 1× it runs as a stepped stream paced at `scale`, from the stream time it
  had, in steps of the clock's `step_ns`; crossing 1× live switches between the two at the
  current stream time. Nothing drops above 1×; the warp slows when the graph cannot keep up.
  This is the plan as written.
- **A2 — barrier at every scale.** A scaled stream is always a stepped stream paced at `scale`.
  One rule, but slow motion moves in visible steps unless `step_ns` is small.
- **A3 — free at every scale.** Scaled never waits and may drop above 1×; lockstep faster than real
  time is a `SteppedStreamClock` with a `pace`. Simplest, but amends the plan line.

## [NEEDS DECISION] B — a node paced by real time in a scaled or stepped stream

The ADR leaves this to the change that builds it.

- **B1 (recommended) — refused at load.** A scaled or stepped stream refuses a node declaring
  `paced_by_real_time`, naming it. A device then lives in its own real-time stream, and a
  scaled or stepped stream renders to a file or a simulation view, as the plan already says.
  One rule, no mixed epochs inside a stream.
- **B2 — refused only when stepped.** A scaled stream admits it; its stamps stay machine time
  while the rest of the stream runs scaled, and a node joining the two must translate.

## Tickets (expected)

Three tracer bullets, in order, each provable without a GPU:
1. **Stream clock declared and read, real and scaled.** Spec key, builder call, `StreamClock`,
   the clock board, `ctx.time` and `ctx.stream_clock` in both processes, implicit stamps, the
   `graph` render. Proven by an engine test whose Python helper writes under a 2× clock and whose
   native sink sees stamps advancing twice as fast as `MediaClock`; scaled above 1× is refused
   until ticket 3 (under A1).
2. **Continuous pacing on stream time and live time warp.** The deadline wait in both runners,
   `set_stream_clock_scale` on every control surface. Proven by a continuous Python node firing
   at twice the rate after a live scale change, and by the REMOVED check on `sleep_duration`.
3. **The step barrier.** Step board, ledger, `taken_bags`, the coordinator, both runners,
   `pace`, `stop_after_steps`, the manual-node and `paced_by_real_time` refusals, and scaled
   above 1× per Decision A. Proven by a stepped stream — a Python continuous source, a slow
   native reactive middle, a Python sink — that runs exactly `n` steps faster than real time
   with every stamp `base + k × step_ns` and nothing dropped.

Out of scope: the file source and MP4 file sink a stepped render needs (its own change, per the
ADR), nested `@stream` clocks (with subgraphs), and a `--clock` override at load.
