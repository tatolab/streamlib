# Loading a project loads every stream in it, each its own unit

Rationale for the `[project-loads-every-stream]` entries in `docs/plan/ARCHITECTURE.md`
(§Product), decided 2026-10-28.

## Trigger

Read this before changing what a load naming no stream function loads, before letting one
stream's state decide another's, before making a client find a project's streams itself, and
before making `--name` or a display name stand for more than one stream.

## Decision

- A project is the directory; its `stream.py` may define any number of `@stream`s. A load that
  names no function — `tatolab run`, `tatolab dev`, `run_stream` without `stream_function`,
  `Tatolab.app`'s Add, create — loads all of them. Naming a function loads that one alone.
- Each `@stream` stays its own unit: its own location, id, display name, record, logs and
  lifecycle. Loading together never ties lifecycles together; `stop`, `start`, `rm` and
  `rename` act on one stream.
- The project compiles once, and a failed compile refuses the whole load, leaving every stream
  of the project as it was, as a refused load already does for one stream.
- A display name given at a load names one stream, so `--name` is refused when a load would load
  several.
- A stream whose function was deleted from `stream.py` is recorded `failed` on the project's next
  load, naming the missing function. Only `rm` forgets a stream, so a deletion never silently
  forgets one.
- The runtime, not each client, finds a project's streams: `list_project_streams` returns each
  function, its docstring and, when loaded, its id and state.

## Rejected alternatives

- **A bare load refusing when `stream.py` defines several `@stream`s** (the 2026-10-01 shape) —
  a project is the unit a person adds, and adding a project that starts one of its streams, or
  none, reads as broken.
- **Listing every stream but loading only the one picked** — every client grows a picker, and an
  agent needs a call per stream to bring a project up.
- **Bare `run` loading every stream while the app's Add keeps a picker** — the GUI and the CLI
  would load different things from the same project.
- **A project-level lifecycle (stopping the project stops all its streams)** — the streams are
  independent units; a project-wide verb can be built on per-stream ones when a need appears.
- **Each client parsing `stream.py` for `@stream`** — the runtime's compile is the authority on
  what a project defines; a client regex drifts from it.

## Consequences

- The compile entry reports every stream a project defines, with its graph, in one run of the
  project's interpreter, and `run_stream`'s reply carries a list.
- `Tatolab.app`'s Add drops its stream picker and its `stream.py` reading, and calls
  `list_project_streams` to show what a project holds.
- `dev` interleaves several streams' records, so each record carries the stream's display name.
