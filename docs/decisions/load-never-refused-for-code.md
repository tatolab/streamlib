# A load is never refused for code: a failing stream is a failed stream, and the runtime always makes the venv

Rationale for the `[load-never-refused-for-code]` entry in `docs/plan/ARCHITECTURE.md`
(§Product), decided 2026-10-29.

## Trigger

Read this before making any load, start or create refuse because of what a project's code or
environment does; before rolling a stream back to a previous version when a new one fails; and
before leaving a project's `.venv` for someone else to make.

## Decision

- The runtime is the place people and agents build in, like a game engine's editor: opening a
  project never depends on its code being correct. A load always takes the project in. A stream
  whose venv, compile, describe or start fails is recorded `failed`, listed with its reason, and
  its traceback is in its logs. Fixing the code and saving under `dev`, or loading again, is the
  way forward.
- A replaced stream whose new code fails is unloaded and shows `failed`; the previous version is
  not put back. The record says what the code does now.
- When `stream.py` cannot be imported, the runtime cannot see the project's streams. Every
  stream already recorded from the project is marked `failed`. A project with none gets one
  failed record for `stream.py`, so a brand-new broken project still shows up in the list, and
  the next load that imports `stream.py` replaces it.
- Only the caller's own mistakes are refused: a path that is not there, a folder that is not a
  project, a function name nothing defines, `--name` across several streams, an id another
  location holds, and create over scaffold files. None of these leaves a record worth fixing.
- The runtime always takes care of the `.venv`, as `npm install` would: whenever
  `.venv/bin/python` does not run — missing, deleted, or pointing at a Python that is gone — on
  a load, `start`, the runtime's re-load at start, create and listing. A `.venv` whose
  interpreter runs is never touched. A sync uv cannot finish is a failed stream in uv's words.
- `tatolab run` exits 1 when a stream it loaded is `failed`, so a script or an agent notices,
  while the stream stays recorded and listed.

## Rejected alternatives

- **Refusing a load whose code fails** (the 2026-10-02 rule for a first load) — a person or an
  agent could load only finished code, and a coding agent's first draft would never appear in
  the runtime to be debugged.
- **All or nothing: putting every stream of the load back as it was** — it keeps the runtime
  consistent, but it hides the failing code behind the old version and makes the failure a
  message rather than something listed with logs.
- **Each stream on its own, with a failure refused in the reply** — the streams that loaded run,
  but the failing one leaves no record to see, retry or follow.
- **Refusing a load when the `.venv` cannot be made** — the environment is part of the project
  being built; a missing system library or no network is fixed and retried like a code bug.
- **Refusing a brand-new project whose `stream.py` does not import** — the project the person or
  agent just made would not appear anywhere.
- **Projects as their own records, beside streams** — a new concept and a new record for what a
  single placeholder stream already shows.

## Consequences

- A stream record can hold no graph: a stream whose compile failed has a location and a reason
  only, and `start` compiles it from its location.
- The runtime's re-load at start may run `uv sync`, so a start after a deleted `.venv` takes as
  long as the sync and needs the network.
- A load's reply carries running and failed streams alike; clients read each stream's state
  rather than treating an answer as success.
