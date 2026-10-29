# A failed load keeps its log, and a `stream.py` with no `@stream` is a failed project

Rationale for the `[failed-load-logs]` clauses of the `[load-never-refused-for-code]` entry in
`docs/plan/ARCHITECTURE.md` (§Product), decided 2026-10-29.

## Trigger

Read this before making `logs` answer only for a loaded stream, before dropping a failed load's
records, before keeping a removed stream's log files or deleting any other, and before refusing a
load because `stream.py` defines no `@stream`.

## Decision

- A failed load is a load of its own, as a running one is. `run_stream` answers its
  `stream_instance`, the name `logs` pages carry, and its records are what the load logged,
  then one error record with its traceback or reason, written to its log file under
  `<project>/.streamlib/logs/` like any other record.
- `logs` serves a failed stream's records from that file until the stream is loaded again,
  across the runtime's restarts. `logs -f` and `dev` print them and wait; the next load is
  another `stream_instance`, followed from its first record.
- `rm` forgets the stream and deletes its log files under `<project>/.streamlib/logs/`, and
  nothing else there.
- A `stream.py` that imports and defines no `@stream` is failed as one that cannot be imported
  is: with no stream recorded from the project, the project's own `<folder>-<hash>` failed
  record, its reason the missing `@stream`; with some, each recorded `failed` as no longer
  defined. It is not a refusal.

## Rejected alternatives

- **A failed stream with no logs, its reason only in the listing** — a one-line reason cut from
  a traceback is not enough to fix the code, and `dev` could not follow a failed stream the way
  it follows a running one.
- **Holding a failed load's records in memory only** — a stream that failed while no one watched
  would be listed failed across a restart with nothing to read but its reason. Docker keeps a
  dead container's logs until `docker rm`; a failed stream's log lasts as long.
- **`rm` leaving the stream's log files in place** — every load writes a file, and every `dev`
  save is a load, so they would pile up with nothing pointing at them. Docker's `rm` and
  Kubernetes' pod deletion take the logs with them; `.streamlib/` is the runtime's own folder
  in the project, not the user's code.
- **Refusing a `stream.py` that defines no `@stream`** — an agent's first draft, or a function
  whose decorator is missing, would never appear in the runtime to be fixed.

## Consequences

- `logs` reads a log file from disk for a stream that is not loaded, so a failed stream's
  record names the file of its last load.
- `run_stream` answers a `stream_instance` for a failed stream as for a running one.
- `rm` deletes files inside the project: the removed stream's log files, under the runtime's
  own `.streamlib/logs/`, and nothing else. Rotating the old load files of a stream that is
  never removed is not decided here.
- A project whose `stream.py` defines no `@stream` is listed, as `<folder>-<hash>`, from its
  first load.
