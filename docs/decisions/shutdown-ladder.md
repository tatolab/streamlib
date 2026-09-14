# Shutdown always ends

Rationale for the `[shutdown-ladder]` entries in `docs/plan/ARCHITECTURE.md` §Processor model,
§Networking and §Language SDKs, decided 2026-09-14.

## Trigger

Read this before adding a timeout, a join, a kill or a signal handler anywhere on the
shutdown path, and before changing how a helper process is stopped or how a processor that
will not stop is handled.

## Decision

- **One ladder, every helper at once.** `stop` and `teardown` are sent together. A Python
  callback still running after one second is interrupted with `KeyboardInterrupt`, and
  `stop()` and `teardown()` still run. `teardown()` has five seconds. Then the helper's
  process group is terminated, killed and reaped.
- **Descendants die with the processor.** A helper's descendants go with it. A helper
  inherits no descriptor beyond its escalate socket and its standard streams, which are
  pipes the engine reads, never the app's own output. At helper exit the engine shuts its
  end of the escalate socket and stops waiting on those pipes.
- **Repeated interrupts escalate.** `rt.run()` owns SIGINT, SIGTERM and SIGHUP through the
  whole teardown. The first interrupt is graceful, the second forces, and the third kills
  every helper group and exits with status 130.
- **Nothing hangs the app.** A native thread that will not return is abandoned and named,
  and a watchdog of about fifteen seconds ends any other hang.

## Rejected alternatives

- **The old five-second reply deadline alone.** It was a naive guard against apps that
  never quit. It marked a helper failed whenever a single callback ran longer than five
  seconds and skipped that processor's `teardown()`. It also did not prevent the hangs it was
  meant to:
  - An unbounded join on a native thread that never returns.
  - Repeat Ctrl-Cs swallowed while teardown ran.
  - Helpers stopped serially at about ten seconds each.
  - Helpers and their children inheriting the app's standard-output descriptors, so anything
    reading the app's output to its end kept waiting after the app died. This is the "won't
    quit even with SIGKILL" shape.
- **`_thread.interrupt_main()` from the helper's command thread.** It does not wake a main
  thread asleep in a blocking call. A real signal does.
- **Killing only the helper's pid.** Fork-based workers and `os.system` children survive
  it, and they keep the helper's sockets open. A process group reaches them.
- **Authorable budgets.** They would add a configuration dial for a guarantee the engine
  should simply keep.
- **Waiting indefinitely for a cooperative teardown.** An app that cannot be stopped is worse
  than a teardown cut short, and the ladder already gives a cooperative processor its full
  budget.

## Consequences

- A `KeyboardInterrupt` can arrive inside a Python callback at shutdown. The bag in flight
  is lost, which `teardown()` can observe.
- An abandoned native thread keeps the engine alive beneath it until the process exits, and
  `run()` raises naming it rather than returning cleanly.
- A descendant that deliberately leaves its process group escapes the kill. It holds none
  of the app's descriptors, and the escalate socket it may have inherited is already shut at
  the engine's end, so it cannot stall the app's output or issue privileged operations.
- A process stuck in uninterruptible sleep inside a GPU driver cannot be killed from user
  space. The watchdog ends the app around it where the kernel allows.
