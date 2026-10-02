# Runtime hosting: one runtime per machine, owned by one user

Rationale for the `[runtime-hosting]` entries in `docs/plan/ARCHITECTURE.md` (§Product, §Processor
model & scheduling, §Media I/O, §Control plane & observability), decided 2026-10-01 as step 3 of the
one-runtime-per-machine pivot — step 4 since the reorder the same day (#2582).

## Trigger

Read this before letting two runtimes share a machine, before adding a verb that keeps or loads a
stream, before making a per-process table global again, and before changing how the runtime is
packaged on Apple.

## Decision

- One runtime per machine, owned by whoever installed or started it; another user's runtime is
  refused naming the holder. A server or robot runs it as one service account.
- `run` attached, `run -d` kept and re-loaded on every start, `stop` unloads and remembers,
  `start` resumes, `rm` forgets (amended 2026-10-02), `streams` lists, `dev` reloads on edit; no `up`/`down` — `tatolabd` runs in a terminal or a
  container where no installer put it.
- Composition is plain Python over a flat graph.
- The runtime keeps the GPU context, signals, Zenoh session, local API and relay once per machine;
  every other per-process table becomes per stream. Streams on one machine link without exposing;
  surfaces are shared across all of the runtime's processor interpreters.
- On Apple, `tatolabd` is an SMAppService agent inside the signed Tatolab app.
- Who starts it (2026-10-02): no verb ever does. With no runtime running, a verb fails because it
  cannot reach the socket, as `docker` does with its daemon down. `Tatolab.app` registers the
  login service on Apple (Docker Desktop's shape), a terminal runs `tatolabd` there until the app
  ships, and on Linux the installer registers a systemd user service, Docker Engine's shape.
- Kept streams always come back; `stop` remembers, `start` resumes, `rm` forgets; an attached
  stream ends with its terminal command (2026-10-02).
- Every CLI stream action is also a tool and `graph` returns every stream, because the CLI is a
  pure client of the runtime's tools; surfaces cross between streams on one machine with no
  copy (both confirmed 2026-10-02).
- A machine-name clash on the mesh takes the next unused suffix (`<name>-2`, then `-3`…),
  recorded and kept for good —
  the first claimant keeps the bare name (2026-10-02).

## Rejected alternatives

- **No service until the app, on every floor.** Read from the owner's 2026-10-02 remark that a
  service now "would cause chaos". The owner clarified that the chaos was a verb starting the
  runtime as a side effect, not a service existing. A Linux machine has no app to wait for:
  servers and robots run the runtime as a service account, so the systemd unit is permanent there.
- **A machine-name clash refusing the runtime's start.** The earlier direction, carried over from
  the shipped runtime-name rule. Two fresh machines sharing a hostname on one LAN would leave the
  second unable to run even local streams, against "the mesh never fails a runtime's start".
  The owner's refusal intent was two users on one machine, which stays refused. Bonjour (RFC 6762
  §9: suffix, persist, tell the user) and Tailscale (`<hostname>-1`, kept after the other leaves)
  were checked live 2026-10-02; the suffix being recorded once is what keeps addresses stable,
  the worry "never auto-suffixed" guarded.
- **`run` starting a runtime when none is running.** Hides which build is serving the machine and
  makes the runtime's lifetime a side effect of whichever command ran first.

- **A runtime per user, a second user naming a second machine.** The plan already gives the
  machine one GPU owner, one Zenoh router, one relay and one URL namespace; a second runtime would
  contend for the GPU with no arbiter, collide on cameras and ports, make "machine" mean two things
  on one hostname, and force same-computer streams to expose themselves to each other. Tailscale
  and Docker Desktop also give a machine one owner and refuse the second user. Owner: adding
  multi-user later breaks nobody; shipping it and removing it would.
- > ~~**Docker's separate `stop` and `rm`.** A stopped stream keeps nothing worth keeping; re-run it.~~
  — Superseded 2026-10-02 by the owner: stopping a kept stream must not lose it ("we don't lose
  it"), and an `rm` is owed anyway to uninstall a stream, so `stop` remembers, `start` resumes and
  `rm` forgets.
- **Restart policies.** Docker's `--restart` dial. Owner, 2026-10-02: "introducing restart
  policies would be weird" — a kept stream always comes back; not wanting it back is `stop`.
- **Recording attached streams.** An attached stream lives as long as its terminal command; a
  crash ends it and the command says so, which is what Docker does for a container started
  without a restart policy.
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
