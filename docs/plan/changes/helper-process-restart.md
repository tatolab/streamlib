# helper-process-restart

> **Asked for by the owner, 2026-11-03:** "auto restarting is a missing feature … necessary", as
> its own engine change, sharing one restart design with the local relay's supervisor and the
> shared browser host (`browser-and-rive-source-modules`). The crash-loop limit and the retry path
> were settled by the owner the same day (§Decided below).

Today a Python processor whose helper process dies on its own stays down for the life of the
stream. The engine notices the death, takes the helper's process group down, refuses the node's
pending links and moves the node to `Error`, and nothing ever starts it again. The rest of the
stream keeps running around a hole until the stream is re-run.

**After this change:**
- **A helper that dies on its own is started again**, after a backoff, as the same node with a
  new processor instance. The old instance died with its process; the new process re-imports the
  class, constructs a new instance, runs its `setup()` with the node's current config, wires every
  link the node holds again, and the node runs. No lifecycle call (`stop`, `on_pause`, `setup`)
  ever reaches the dead instance. Its id, config, links and place in the graph never
  change, so nothing upstream or downstream is re-planned.
- **A crash loop ends.** A helper that keeps dying soon after it starts is given up on: the node
  reads `Error`, with the last death's reason, and the rest of the stream keeps running as it does
  today.
- **`graph` says what happened.** A node between a death and its next start reads `Restarting`,
  with how many times it has been restarted and why it last died.
- **One restart policy in the engine.** The local relay's children, the shared browser host and
  helper restart count deaths and wait between starts through one engine type, so there is one
  set of numbers to tune and one place they are tested.
- **What a stream author sees:** a processor whose `setup()` must be safe to run again in a fresh
  process, which it already is (each helper is one fresh interpreter). No node, port, config key
  or Python Protocol changes.

**Scale gate — this skill plus an ADR** (`docs/decisions/helper-process-restart.md`): it changes
the processor model's lifecycle, which is one of the expensive things to reverse.

**Precondition.** The shutdown ladder and helper-death path (§Processor model,
`ARCHITECTURE.md:1386-1400`, `:1440-1452`), the per-spawn loss-count board (`:1114-1137`) and
failure isolation (`:1629-1648`) are DECIDED and SHIPPED.

## Current state (recon, 2026-11-03, `main` `cf18995`)

- **Death is detected by the process, every 100 ms.** The Manual-mode loop in
  `runtime/streamlib-engine/src/core/execution/thread_runner.rs:826-835` calls
  `detect_and_clean_up_after_an_out_of_process_helper_that_died` and then
  `has_failed_unrecoverably`; a dead helper moves the node to `Error` once, through
  `transition_to`, so the node carries **no failure reason** (`fail_with` is never called on this
  path). The loop keeps polling until shutdown, so nothing else is disturbed.
- **The death path takes everything down.**
  `processor_interpreter_spawn_host.rs:1318-1334` → `take_the_helper_process_group_down`
  (`:604-617`) → `close_the_engines_end_of_the_helper_process` (`:626-650`): the ladder's
  terminate/kill rung, the bridge dropped (escalate socket shut), the macOS surface-share
  admission released once reaped, and the dead helper's iceoryx2 nodes reclaimed.
  `give_up_on_the_helper_process` (`:357`) refuses every later link through
  `refuse_every_later_link_because_the_far_side_is_gone`.
- **The links survive the death.** `OutOfProcessLinkWiringEnvelope`
  (`core/processors/__generated_private/generated_processor_impl.rs:123-427`) keeps every link it
  handed over in `input_links` / `output_links`; only `how_a_link_reaches_the_far_side` moves to
  `RefusedBecauseTheFarSideIsGone`. `send_the_setup_command_then_hand_every_later_link_over`
  refuses a second setup command (`:360-368`), so a respawn needs the envelope reopened, never a
  second envelope.
- **A spawn is already self-contained.** `start_the_helper_process_and_await_its_registration`
  (`processor_interpreter_spawn_host.rs:693-858`) creates a fresh loss-count board per spawn,
  builds the command, spawns, admits the pid to surface-share, registers the process group with
  its stream tag, starts the standard-stream readers, opens the bridge and sends the setup command
  carrying the envelope's ports. `start` (`:1246-1262`) sends `run`. Nothing in it assumes it runs
  once, apart from the envelope's guard above.
- **The relay already restarts, without a limit.** `keep_the_child_running` in
  `runtime/streamlib-relay-adapter-local/src/local_relay_child_supervision.rs:183-249` waits
  `FIRST_RESTART_BACKOFF` (1 s), doubling to `LONGEST_RESTART_BACKOFF` (30 s), reset after a run of
  `RUN_THAT_RESETS_THE_BACKOFF` (60 s) (`:37-44`). It retries forever, which is right for
  infrastructure the runtime ships and wrong for user code.
- **No respawn machinery exists anywhere else.** The reload-on-save entry (`ARCHITECTURE.md:1469`)
  describes the same processor-granular respawn and is unbuilt.

## ADDED

- **ADDED (§Processor model & scheduling, DECIDED):** A helper that dies on its own is restarted
  by its node's host. "On its own" is every exit the engine did not ask for — a crash, a signal, an
  exit of any status — and never one on the shutdown ladder, at node removal, at stream stop, or
  by the stream's watchdog. The death path runs as today (process group down, escalate socket shut,
  surface-share admission and iceoryx2 nodes reclaimed, pending links refused), then the node waits
  its backoff and starts again as the same node with a new processor instance — the old one died
  with its process: a fresh interpreter re-imports the class and constructs the instance, `setup()`
  runs on it with the node's current config, the setup command carries every link the node holds, `run`
  follows. A link's `graph` state is not re-stamped across a restart: a link `wired` before the
  death reads `wired` throughout, and one still `pending` at the death is refused on the death path,
  reads `error`, and is left off the new helper's setup command, so an `error` link never carries
  a bag; it is disconnected and connected again like any refused link. A frame or
  message in flight at the death is lost and never replayed; a Python processor's in-memory state
  is gone, as it is at every start. A restart gets a fresh loss-count board, as every spawn does.
  Budgets are engine-chosen and not authorable. [helper-process-restart]
- **ADDED (§Processor model & scheduling, DECIDED):** One restart policy for every child process
  the runtime keeps alive. An engine type, `ChildProcessRestartBackoff`, holds the wait before each
  start — 1 s, doubling to 30 s, back to 1 s after a run of 60 s — and a crash-loop limit
  counting deaths in a row that each came within 60 s of their start. The local relay's children
  run it with no limit; a helper and the shared browser host run it with a limit of 5 — the fifth
  such death in a row is the last. Each owner keeps its own loop (the relay's supervision thread,
  the helper's thread-runner poll, the browser host's supervisor) and asks the type when to start
  and whether to give up. [helper-process-restart]
- **ADDED (§Processor model & scheduling, DECIDED):** A node past its crash-loop limit reads `Error`
  with the last death's reason and stays down; the rest of its stream keeps running. Re-running the
  stream (`tatolab run`, or `tatolab start` after a stop) starts it with a fresh count. A restart
  whose `setup()` refuses, or whose helper refuses the engine's build id, counts as one death in a
  row, and its refusal is the reason the node carries. [helper-process-restart]
- **ADDED (§Control plane & observability):** A helper-placed node's `components.state` in `graph`
  reads `Restarting` between a death and its next `ready`, and its
  `components.helper_process_restarts` carries `restart_count` (restarts since the stream loaded)
  and `last_exit_reason` (the exit status or signal, its pid, and the tail of the helper's standard
  error, as the setup refusal already quotes it). A node past its limit reads `Error`; readiness
  waits through `Restarting`, so its error names the crash loop and the last death's reason. The MCP instructions name
  `Restarting` and the restart record. [helper-process-restart]

## MODIFIED

- **MODIFIED (§Processor model, the shutdown-ladder entry, `:1386-1400`):** "a crash the engine
  detects by the process itself" keeps its whole death path and is followed by the restart entry
  above; the ladder itself is unchanged, and a helper on the ladder is never restarted.
- **MODIFIED (§Processor model, the link-confirmation entry, `:1440-1452`):** a helper that dies
  with a link unconfirmed still takes the link down on the death path; it reads `error` and the
  restarted helper's setup command leaves it off.
- **MODIFIED (§Processor model, the loss-count board entry, `:1114-1137`):** a restarted helper
  gets a fresh board, and each wiring's counts stay cumulative across the restart: the parent
  keeps what the dead helper's board last held for every wiring still in place and adds it to what
  the new board reads, so a link's losses never go backwards in `graph`; a reconnect still starts
  from zero.
- **MODIFIED (§Processor model, failure isolation, `:1629-1648`):** "Python crashes and hangs stay
  in their own process" gains "and a crashed helper is restarted within its crash-loop limit".
- **MODIFIED (§Product, "There are no restart policies", `:209`):** reads "There are no stream
  restart policies", so it is not read as ruling out helper restart; the stream-level rules
  (two runtime crashes in a row, `failed` at load) are unchanged.
- **MODIFIED (§Networking, the local relay entry, `:3212`):** "started and supervised by
  `tatolabd`" names the shared restart policy with no limit.
- **MODIFIED (§Graphics, the `VirtualCameraSink` OPEN, `:2167-2174`):** "the runtime's restart
  policy" now exists for helpers only; whether a latched device refusal in a native built-in
  retries stays open.
- **MODIFIED (engine code):**
  - `thread_runner.rs:826-835` — the Manual loop asks the host to restart instead of latching
    `Error`, and moves the state to `Restarting`, `Running` and `Error` with reasons; the backoff
    is a monotonic deadline checked in the existing 100 ms poll, never a sleep, so a shutdown
    during the wait is seen within one poll. A helper whose escalate socket closed is given 5 s
    to exit before it is taken to have stopped answering, so a dying helper is restarted rather
    than failed.
  - `ProcessorInterpreterSpawnHostProcessor` gains the restart: reset `child_is_gone`,
    `shutdown_was_already_asked_of_this_helper` and the bridge, reopen the envelope, then run
    `start_the_helper_process_and_await_its_registration` and `start` again.
  - `OutOfProcessLinkWiringEnvelope` gains reopening for a new far side: from
    `RefusedBecauseTheFarSideIsGone` back to `RidesTheSetupCommand`, keeping every recorded link
    but one handed over past the setup command that the dead helper never opened.
  - `ProcessorState` gains `Restarting`; the state component keeps a reason on every `Error`.
  - `local_relay_child_supervision.rs` reads its waits from `ChildProcessRestartBackoff`.

## REMOVED

- REMOVED: FIRST_RESTART_BACKOFF
- REMOVED: LONGEST_RESTART_BACKOFF
- REMOVED: RUN_THAT_RESETS_THE_BACKOFF

  The relay's three constants move into `ChildProcessRestartBackoff`, so no second copy of the
  numbers survives.

## Decided — how a crash loop ends (owner, 2026-11-03)

A helper is given up on after 5 deaths in a row, each within 60 s of its start; a run of 60 s
resets the count, so a helper that crashes rarely keeps coming back. A `failed` node is retried by
re-running the stream; no verb restarts one node.
Rejected: never giving up, as the relay does — a node whose `setup()` always raises would spin
every 30 s and hide its failure (owner, 2026-11-03).

## Tickets (expected)

One tracer bullet: the policy type with the relay moved onto it, the envelope reopening, the
spawn host's restart, the thread-runner states and the `graph` fields, proven by an engine test
whose fixture helper `os._exit`s after its first message and is seen `restarting`, then `wired`
and running again, and a second fixture that dies in `setup()` and is seen `failed` after five
tries. The browser host's supervisor adopts the type when it is built, under its own change.

As built, the fixtures die in `process()`: one beside a source-sink pair, proven in CI without a
GPU (`test_helper_placement.py`), and one that dies on every start and is left `Error` after five
(`test_helper_process_restart.py`, GPU rig only). No fixture yet dies in a restart's `setup()`.
