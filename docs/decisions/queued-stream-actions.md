# Every load and unload is a queued stream action, and a project has one log

Rationale for the `[queued-stream-actions]` entries in `docs/plan/ARCHITECTURE.md` (§Product,
with the tool inventory and `logs` in §Control plane), decided 2026-10-30.

## Trigger

Read this before making a stream tool wait for a venv, a compile, a load or a teardown; before
raising a client timeout so a reply fits around project work; before adding a refusal to a load;
before making `tatolab run` wait for a stream to settle; and before writing a log per stream.

## Decision

- Making a project's venv is an asynchronous side effect that can take minutes: one large wheel
  can outlast any bound. So no call that promises its final answer contains it. Every load and
  unload is a **stream action**, queued and answered at once with an **action id**:
  `run_stream`, `start_stream`, `create_project`, `stop_stream`, `remove_stream` and the
  runtime's re-load at start. The venv, the compile, the describe, the load, the start and the
  teardown run in the background, as a workflow engine runs a job.
  `describe_stream_action {action}` reads an action's state, step, streams and reason.
- There is no synchronous mode, for the CLI either. `tatolab run`, `new`, `start`, `stop` and
  `rm` exit 0 once the action is queued, as a backend acknowledges a request. The stream shows
  `preparing` or `stopping` with its current step, and everything the action does is in the
  logs.
- A load refuses nothing. A missing directory, a typo'd function, `--name` across several
  streams and an id another location holds fail the action, with the reason in the action and
  the log. The call errors only on a malformed request.
- What an action does is recorded when it is queued (`stopped`, forgotten, or the new
  location's record), so a crash finishes the action rather than undoing it. The queue itself
  lives in memory.
- A project's actions run one at a time in order. A newer load of a project replaces one still
  queued. Everything that needs the project's venv waits on its one in-flight `uv sync` rather
  than starting another. The sync has no bound of its own.
- `rename_stream` and `expose_port` stay immediate: each sets an attribute of the record, and
  the live side of an exposure — cutting readers the level no longer allows — is bounded and
  runs no project code.
- A project has one log. A project defines any number of streams, and its venv and compile
  belong to none of them. Every record carries `action`, and `stream` when a stream wrote it, as
  OpenTelemetry carries attributes on one log, and `logs` filters by project, stream or
  action. The log schema keeps its field names and gains `action`.

## Rejected alternatives

- **The venv made inside the reply, bounded, with the client's timeouts raised to fit** — what
  shipped first: 240s for the sync, with the load timeout raised to 480s. A CUDA wheel that
  cannot download in the bound never gets a venv, and each larger wheel would raise the timeout
  again.
- **Answering synchronously when the venv already runs, and queuing only a sync** — two modes
  for one call, and the compile and describe still sit inside the reply.
- **`tatolab run` following the load until it settles, keeping exit 1 for a failed stream** —
  it brings the synchronous wait back in the client. The action's state and the logs carry the
  outcome.
- **Keeping the refusals that need `stream.py` imported** — the call would have to run project
  code before answering.
- **`rename_stream` and `expose_port` as actions** — a record attribute gains nothing from a
  queue, and a stream not yet loaded takes it when it loads.
- **One log per stream, or per load of a stream** — the venv's and the compile's records would
  belong to no stream, or be copied into each, and a first load's streams are unknown until
  `stream.py` imports.
- **OpenTelemetry's log field names** — a breaking rewrite of the durable log contract, for no
  reader that needs it yet.
- **A client-supplied idempotency key** — the action id is the handle, and a client on the local
  socket has no retry to deduplicate.

## Consequences

- No stream tool call waits on the one-action lock or on project code, so the load timeout and
  its const-asserts retire, and every stream tool answers within the ordinary timeout.
- Clients read outcomes, never replies. `Tatolab.app` and agents poll `describe_stream_action` or
  `list_streams`; scripts that relied on `run`'s exit 1 for a failed stream read the action.
- The compile's cross-floor warnings reach the person through the project's log, not a reply.
- `rm` deletes no log file. A removed stream's records stay in the project's log until rotation
  ages them out.
- A queued action is lost if the runtime crashes before it starts; a load of a location already
  on the record is made again by the re-load at start.
- Sequencing of the work already planned on the synchronous shape:
  - the multi-stream load proceeds, and this change converts its `list_project_streams`;
  - creating a project and the app's switch to it wait for this change, so a 600s create
    timeout and a blocking create reply are never built;
  - the per-load log segments and `rm` deleting a stream's log files are replaced by the
    project's log.
