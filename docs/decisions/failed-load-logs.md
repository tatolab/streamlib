# A failed load keeps its log, and a `stream.py` with no `@stream` is a failed project

Rationale for the `[failed-load-logs]` clauses of the `[load-never-refused-for-code]` entry in
`docs/plan/ARCHITECTURE.md` (§Product), decided 2026-10-29.

## Trigger

Read this before making `logs` answer only for a loaded stream, before dropping a failed load's
records, before keeping a removed stream's log files or deleting any other, and before refusing a
load because `stream.py` defines no `@stream`.

## Decision

- > ~~A failed load is a load of its own, as a running one is. `run_stream` answers its `stream_instance`, the name `logs` pages carry, and its records are what the load logged, then one error record with its traceback or reason. Each carries the stream id in `stream`, as every routed record does, and they are written to the load's own segment under `<project>/.streamlib/logs/`, named as any load's is. Its `stream_instance` is never one a later load of the stream is given, across the runtime's restarts, since a re-load is always another instance.~~ — Superseded 2026-10-30 by `queued-stream-actions.md`: a project has one log, every record carrying its stream action's id and, when a stream wrote it, the stream id.
- > ~~`logs` serves a failed stream's records from that file until the stream is loaded again, across the runtime's restarts. `logs -f` and `dev` print them and wait; the next load is another `stream_instance`, followed from its first record.~~ — Superseded 2026-10-30 by `queued-stream-actions.md`: `logs` pages the project's log by project, stream or action, from disk.
- > ~~`rm` forgets the stream and deletes its log files under `<project>/.streamlib/logs/`, and nothing else there.~~ — Superseded 2026-10-30 by `queued-stream-actions.md`: `rm` deletes no log; a removed stream's records age out with the project log's rotation.
- A `stream.py` that imports and defines no `@stream` is failed as one that cannot be imported
  is: with no stream recorded from the project, the one failed stream record for `stream.py` —
  its location the project directory with no function, its id `<folder>-<hash>` — its reason
  the missing `@stream`; with some, each recorded `failed` as no longer defined. It is not a
  refusal, and no record of a project beside its streams is made.

## Rejected alternatives

- **A failed stream with no logs, its reason only in the listing** — a one-line reason cut from
  a traceback is not enough to fix the code, and `dev` could not follow a failed stream the way
  it follows a running one.
- **Holding a failed load's records in memory only** — a stream that failed while no one watched
  would be listed failed across a restart with nothing to read but its reason. Docker keeps a
  dead container's logs until `docker rm`; a failed stream's log lasts as long.
- **The stream record naming its last load's log file** — a new record field changes the
  record's format for every existing record, and names one file where `rm` must find them all
  (owner, 2026-10-29).
- **`rm` leaving the stream's log files in place** — every load writes a file, and every `dev`
  save is a load, so they would pile up with nothing pointing at them. Docker's `rm` and
  Kubernetes' pod deletion take the logs with them; `.streamlib/` is the runtime's own folder
  in the project, not the user's code.
- **Refusing a `stream.py` that defines no `@stream`** — an agent's first draft, or a function
  whose decorator is missing, would never appear in the runtime to be fixed.

## Consequences

- `logs` reads a log file from disk for a stream that is not loaded. It and `rm` find a
  stream's segments by its stream id in its project's `.streamlib/logs/` — a name ending
  `-<stream id>-<digits>`, confirmed by the `stream` its records carry, since a pinned runtime
  id may hold dashes. The stream record gains no field.
- `run_stream` answers a `stream_instance` for a failed stream as for a running one.
- `rm` deletes files inside the project: the removed stream's log files, under the runtime's
  own `.streamlib/logs/`, and nothing else. Rotating the old load files of a stream that is
  never removed is not decided here.
- A project whose `stream.py` defines no `@stream` is listed, as its `stream.py` record
  `<folder>-<hash>`, from its first load.
- `docs/logging-schema.md`'s one log per loaded stream grows to a failed load's segment, and a
  compile's output reaches a stream's segment once the compile has named the stream; the
  schema document says so when this is built.
