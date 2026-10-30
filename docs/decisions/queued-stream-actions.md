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
- A load takes a project and loads every `@stream` in it; it names no function and gives no
  display name. A project defines any number of streams, and each is managed afterwards by its
  own `start`, `stop`, `rm` and `rename`.
- A load refuses nothing. A missing directory and an id another location holds fail the
  action, with the reason in the action and the log. The call errors only on a malformed
  request.
- What a stop, a remove or a start does to the record is written when it is queued, so a
  crash finishes the action rather than undoing it. The queue itself lives in memory.
- Each project has one serial queue, as a workflow engine runs jobs: its actions run one at a
  time in order, and nothing replaces or edits a queued action — `cancel_stream_action` is the
  only way to end one early. Everything but the sync is fast, so a serial queue costs little.
  The queue is per project, so one project's long sync never holds another's stop. The sync
  has no bound of its own.
- `list_project_streams` runs no project code: it answers from the project's last compile,
  and says `preparing` while a load has not compiled yet. Saying it does not know yet is more
  truthful than importing `stream.py` inside the call.
- `rename_stream` and `expose_port` stay immediate: each sets an attribute of the record, and
  the live side of an exposure — cutting readers the level no longer allows — is bounded and
  runs no project code.
- A project has one log, and the project is its only key. A stream is a programmatic concept
  a project defines any number of; the project is a real location on disk, as in any other
  tool, and the storage unit for its log. Every record carries `action`, and `stream` when a
  stream wrote it, as attributes — as OpenTelemetry carries attributes on one log — never as
  where the record lives. `logs` takes the project, narrowed to a stream or an action when
  asked, so it works whether or not the stream is recorded or the action remembered. `dev`
  shows every record under the project's folder name. The log schema keeps its field names and
  gains `action`.

## Rejected alternatives

- **The venv made inside the reply, bounded, with the client's timeouts raised to fit** — what
  shipped first: 240s for the sync, with the load timeout raised to 480s. A CUDA wheel that
  cannot download in the bound never gets a venv, and each larger wheel would raise the timeout
  again.
- **Answering synchronously when the venv already runs, and queuing only a sync** — two modes
  for one call, and the compile and describe still sit inside the reply.
- **A newer load replacing one still queued** — a workflow never replaces a job, only cancels
  it, and replacement drops the loads it does not cover: a re-load at start per record would
  keep only the last stream of a project.
- **`run` naming one stream function, and `--name` at a load** — a project is loaded whole;
  naming a function at a load was a single-stream habit, and the per-stream verbs already
  manage one stream.
- **`list_project_streams` importing `stream.py` inline when the venv runs** — a hung import
  would hold the call, and it is the one place a call would run project code.
- **`tatolab run` following the load until it settles, keeping exit 1 for a failed stream** —
  it brings the synchronous wait back in the client. The action's state and the logs carry the
  outcome.
- **Keeping the refusals that need `stream.py` imported** — the call would have to run project
  code before answering.
- **`rename_stream` and `expose_port` as actions** — a record attribute gains nothing from a
  queue, and a stream not yet loaded takes it when it loads.
- **One log per stream, or per load of a stream, or a log found through a stream's record** —
  the venv's and the compile's records would belong to no stream, a first load's streams are
  unknown until `stream.py` imports, and after `rm` or a restart nothing would map a stream or
  an action back to its log.
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
- A queued action is lost if the runtime crashes before it starts; a project with a record
  neither stopped nor failed is loaded again by the re-load at start, and a first load of a new
  project is lost until it is run again.
- A `dev` session saving rapidly queues one load per save; each runs in turn, and only the
  venv's making is slow.
- Sequencing of the work already planned on the synchronous shape:
  - the multi-stream load proceeds, and this change removes its function-naming load and
    converts its `list_project_streams`;
  - creating a project and the app's switch to it wait for this change, so a 600s create
    timeout and a blocking create reply are never built;
  - the per-load log segments and `rm` deleting a stream's log files are replaced by the
    project's log.
