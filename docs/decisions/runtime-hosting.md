# Runtime hosting: one runtime per machine, owned by one user

Rationale for the `[runtime-hosting]` entries in `docs/plan/ARCHITECTURE.md` (§Product, §Processor
model & scheduling, §Media I/O, §Control plane & observability), decided 2026-10-01 — step 3 of the
one-runtime-per-machine pivot.

## Trigger

Read this before letting two runtimes share a machine, before adding a verb that keeps or loads a
stream, before making a per-process table global again, and before changing how the runtime is
packaged on Apple.

## Decision

- One runtime per machine, owned by whoever installed or started it; another user's runtime is
  refused naming the holder. A server or robot runs it as one service account.
- `run` attached, `run -d` kept and re-loaded on every start, `stop` unloads and forgets,
  `streams` lists, `dev` reloads on edit; no `up`/`down` — `tatolabd` runs in a terminal or a
  container where no installer put it.
- Composition is plain Python over a flat graph.
- The runtime keeps the GPU context, signals, Zenoh session, local API and relay once per machine;
  every other per-process table becomes per stream. Streams on one machine link without exposing;
  surfaces are shared across all of the runtime's processor interpreters.
- On Apple, `tatolabd` is an SMAppService agent inside the signed Tatolab app.

## Rejected alternatives

- **A runtime per user, a second user naming a second machine.** The plan already gives the
  machine one GPU owner, one Zenoh router, one relay and one URL namespace; a second runtime would
  contend for the GPU with no arbiter, collide on cameras and ports, make "machine" mean two things
  on one hostname, and force same-computer streams to expose themselves to each other. Tailscale
  and Docker Desktop also give a machine one owner and refuse the second user. Owner: adding
  multi-user later breaks nobody; shipping it and removing it would.
- **Docker's separate `stop` and `rm`.** A stopped stream keeps nothing worth keeping; re-run it.
- **`up`/`down` verbs.** Naming the program `tatolabd` makes "run the runtime in a terminal" the
  program itself, as `tailscaled` is.
- **A `group` label or nested subgraphs.** No consumer needs to see a fragment as one thing yet.
- **A bare launchd binary on Apple.** Its prompts name the executable file and key the grant by
  path; an agent inside the signed app is credited to the app (TN3179, DTS guidance; #2560).

## Consequences

- A shared lab machine serves one user at a time until a shared, system-wide runtime is decided.
- The per-stream split is the hosting change's bulk: the event topic, registry, interpreter, logs,
  shutdown, watchdog and process-group table all move off process scope.
- A denied microphone is silent at the OS level, so the runtime must check authorization itself.
