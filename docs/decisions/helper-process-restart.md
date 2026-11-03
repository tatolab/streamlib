# A crashed helper comes back

Rationale for the `[helper-process-restart]` entries in `docs/plan/ARCHITECTURE.md` §Processor
model & scheduling, §Control plane & observability and §Networking, proposed 2026-11-03.

## Trigger

Read this before changing what happens when a helper process dies on its own, before adding a
retry, a backoff or a supervisor loop around any child process the runtime keeps alive, and before
making a restart budget authorable.

## Decision

- **A helper that dies on its own is restarted as the same node, with a new processor instance.**
  The processor instance lives inside its helper process, so it dies with it; nothing is left to
  pause, stop or set up again, and the restart constructs a new one in a new process. The death path runs in full
  first, so a restart never inherits the dead helper's process group, escalate socket, surface-share
  admission or iceoryx2 nodes. The node's id, config and links are kept; nothing around it is
  re-planned. Each helper is already one fresh interpreter, so a restart asks nothing new of a
  processor's `setup()`.
- **Crash loops end.** User code can fail deterministically — an import error, a `setup()` that
  always raises — and a restart that can never succeed only hides the failure. A death within a
  minute of its start counts toward a limit; a healthy run resets it.
- **One restart policy, many loops.** The local relay already restarts its children with a
  backoff; the shared browser host needs the same; helper restart is the third. Sharing the numbers
  and the counting keeps them from drifting. The loops stay separate because each owner learns of a
  death differently — the relay reaps its child on a thread, a helper is polled by its thread
  runner — and forcing one loop would put a thread per helper or a poll on the relay.
- **Not authorable.** Restart budgets join the shutdown ladder's budgets as engine-chosen. A stream
  author has nothing to tune, and a per-node knob would be a restart policy the plan rules out at
  the stream level.

## Rejected alternatives

- **Restart the whole stream when a helper dies.** It stops every healthy node to fix one, and the
  stream-level crash rules exist for the runtime's own crashes, not a child's.
- **Replay the message in flight.** A message that crashed a helper would crash the next one; the
  plan already rules out silent replay.
- **One generic supervisor thread for every child process.** See above; the policy is what must be
  shared, not the loop.

## Consequences

- A processor's `setup()` must tolerate running again in a fresh process after a crash, which a
  helper's fresh interpreter already guarantees for in-memory state; a processor holding an
  external resource (a device, a file lock) must let it go when its process dies.
- A message in flight at a death is lost, and a Python processor's in-memory state is gone.
- A helper that crashes rarely but forever keeps coming back; only a crash soon after a start
  counts toward the limit.
