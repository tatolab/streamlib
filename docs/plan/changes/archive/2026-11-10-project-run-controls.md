# project-run-controls

> **Decided by the owner, 2026-11-09.** "Projects, not streams, are started, stopped and paused;
> everything runs", as in Unreal, TouchDesigner and Houdini. A reactive node still wakes only when
> upstream data arrives. Starting or stopping a single stream goes away. Closing a running project's
> window stops it after a prompt (stop and close, or cancel), and running in the background is left
> out for now. The app-side record is tatolab-app `docs/engine-guarantee.md`
> (tatolab/tatolab-app#201).

## Current state (recon, `main` `a4574c2`)

- **A project starts as a whole and stops by the stream.** `run_project` loads and starts every
  stream of a project (`runtime/streamlib-api-server/src/mcp.rs:481-504`). `stop_stream` and
  `start_stream` act on one stream (`mcp.rs:577-623`). `remove_project` unloads every stream and
  forgets them (`mcp.rs:625`).
- **Pause exists inside the engine and no verb reaches it.** `LoadedStream::pause` and `resume`
  pause every processor of a stream and compile the changes logged meanwhile
  (`runtime/streamlib-engine/src/core/runtime/loaded_stream.rs:1508-1560`).
- **Re-asking for a capture device is a stop then a start of the stream** (§Media I/O, the entry
  on access nobody asked for).

## ADDED

- **`stop_project {project_directory}`** queues, on the project's action queue, the unload of every
  stream of the project, recording the project stopped so the runtime does not re-load it at its
  next start. `run_project` starts it again (owner, 2026-11-09).
- **`pause_project {project_directory}`** pauses every loaded stream of the project through
  `LoadedStream::pause`, and **`resume_project {project_directory}`** resumes them. A paused project
  stays loaded, its processors hold their state, and its graph edits compile on resume. Pause is
  live state and is not recorded, so a runtime restart re-loads the project running (session,
  2026-11-09: pause is not recorded because the plan records only what a restart must restore).
- `list_streams` lists a paused stream as `paused`.
- A stream a `run_project` starts while its project is paused starts paused, so a `tatolab dev`
  re-load of a paused project stays paused; the project stays paused until `resume_project`,
  `stop_project` or `remove_project` (session, 2026-11-10). Pause and resume answer at once and
  are not queued, so a pause lands while a long run is still making the venv.
- `tatolab pause [--dir DIR]` and `tatolab resume [--dir DIR]` act on the working directory's
  project, as `tatolab stop` does.

## MODIFIED

- §Product, the stream actions: the queued actions are `run_project`, `create_project`,
  `stop_project` and `remove_project`. The project is the unit a person starts and stops.
- §Media I/O, access nobody asked for: re-asking is a `stop_project` then a `run_project`.
- §Control plane: the server instructions name the project verbs.

## REMOVED

- REMOVED: start_stream
- REMOVED: stop_stream

## Consumers

Tato Studio's toolbar runs, stops and pauses the project from one Play/Stop button with Pause beside
it. Its window calls `stop_project` when a person confirms closing a running project. That is
tatolab-app #202, backlog that never blocks this change. `tatolab stop <stream>` becomes
`tatolab stop` for the working directory's project.
