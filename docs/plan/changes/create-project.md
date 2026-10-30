# create-project

> **Approved by the owner, 2026-10-29:** "Approved, merge the pr" (PR #2969).

Built from #2951 and the aligns merged in #2968 and #2970. It builds three §Product entries:
- "Creating a project" (ADR `docs/decisions/create-project.md`);
- "Loading a project loads every stream in it" (ADR `docs/decisions/project-loads-every-stream.md`);
- "A load is never refused for what a project's code or environment does" (ADR
  `docs/decisions/load-never-refused-for-code.md`).

Today `tatolab new <dir>` scaffolds in the CLI and loads nothing; a load needs a `.venv`
someone else made and takes only the sole `@stream`; a first load whose code fails is refused,
and a failed replace puts the previous version back. Tatolab.app works around the first three.

**After this change:**
- **Create.** The runtime creates a project in one call: it scaffolds, makes the `.venv` with
  the uv it carries, then loads. `tatolab new`, a new `create_project` tool and the app share
  that call. With no directory it creates `~/streams/<adjective-noun>`.
- **Load.** The runtime makes the `.venv` whenever `.venv/bin/python` does not run. A load
  naming no function loads every `@stream` in `stream.py`, each as its own stream.
- **Failure.** A stream whose code or environment fails is recorded `failed` with its traceback,
  never refused and never rolled back.
- **List.** `list_project_streams` lists a project's streams.

**Scale gate — this skill. The three ADRs already exist from the aligns.**
- **Contracts changed:** the local API (`run_stream`'s result, two new tools); the CLI's `new`,
  `run` and `dev`; the compile document; the runtime unit's contents on both floors.
- **Not touched:** the RHI, the iceoryx2 wire, the processor model, `@stream` and the rest of
  the authoring API, the stream id, and the stream record's schema version (its `graph` becomes
  nullable, which a schema-3 file already satisfies).

**Precondition.** Every touched section is DECIDED: §Product, §Distribution & versioning (the
Linux installer and `Tatolab.app`) and §Control plane (the tool inventory). Each carries
`amended by create-project`, `amended by project-loads-every-stream` or `amended by
load-never-refused-for-code`.

## Current state (recon, 2026-10-28 and 2026-10-29, `main` `96df35cfc`)

Paths are under `runtime/` unless marked otherwise.

- **`tatolab new`** (`tatolab-cli/src/main.rs:112-115`, dispatched at `:289-291`) takes a
  required directory. `scaffold_new_stream_project.rs`:
  - embeds the seven templates from `sdk/tatolab-stream/scaffold_template/` (`:14-27`);
  - names the distribution after the folder (`:90-131`);
  - refuses before writing when any scaffolded path exists (`:146-160`);
  - prints "Created a Tatolab stream project … uv sync / tatolab dev" (`:182-186`).
- **The load.** `Runner::run_stream` (`streamlib-engine/src/core/runtime/stream_actions_of_this_runtime.rs:351-470`):
  - It runs one compile (`:379-383`), which yields one location, id, graph and set of warnings.
  - It takes the one-action lock after the compile (`:390`).
  - It replaces a recorded stream (`:412-427`, `replace_a_recorded_stream` `:1127-1173`, which
    restores the previous record on failure) or loads a new one (`:432-468`). The display name
    is the request's, else the record's, else the graph's `"stream"`.
  - `RunStreamRequest` is at `:63-75` and `StreamRunOutcome`, one stream, at `:78-95`. The
    budget const-asserts are at `:274-313`.
- **The missing venv.** The compile op refuses "{project} has no .venv/bin/python; run `uv sync`
  in {project}" (`compiler_ops/stream_function_compile_in_the_projects_interpreter.rs:157-162`).
  A start or re-load from a record refuses "its interpreter {} is gone; run `uv sync` in {}"
  (`stream_actions_of_this_runtime.rs:1177-1190`). No runtime code runs `uv`.
- **The record.** `StreamRecord` (`streamlib-engine/src/core/runtime/machine_state_directory.rs:23-56`):
  - schema 3, `graph: serde_json::Value` (`:45`), `failed_because: Option<String>` (`:50`);
  - `location: StreamLocation`, with `stream_function: String`
    (`stream_location_and_id.rs:27-31`). A function that casts to nothing is already left out
    of the id (`:35-55`).
  - `start_stream` (`stream_actions_of_this_runtime.rs:554-590`) re-loads from the recorded
    graph, and a refused retry stays failed (`:586`).
  - The re-load at start, `reload_each_recorded_stream_listed` (`:838-1000`), takes the
    one-action lock per record (`:853`) and marks a record that will not re-load `failed`
    (`:938`, `:996`).
- **The compile.** `<project>/.venv/bin/python -I -m tatolab.stream._project_stream_compile_entry
  --verb run [fn]` (`:107-138`, bound 60s `:39`). It writes one document,
  `{stream_graph, project_directory, stream_function}` (`:71-76`; Python
  `sdk/tatolab-stream/tatolab/stream/_project_stream_compile_entry.py:185-191,257-263`).
  - `select_stream_function` (`_project_stream_entry_execution.py:131-182`) returns the sole
    `@stream`, refuses none (`:146`), and refuses several, listing them (`:148-152`).
  - `stream_functions_defined_in` (`:83-100`) already lists every `@stream` the entry defines,
    in order.
  - `@stream` stamps `__streamlib_stream_description__ = inspect.getdoc(fn)`
    (`_stream_graph_builder.py:101-112`). No description reaches the runtime.
  - The process pattern a `uv` spawn mirrors is at `:172-201`: spawn outside the iceoryx2 binds,
    the stdout/stderr readers, a bounded wait, and a process-group kill.
- **The tools.**
  - `RunStreamToolArguments{project_directory, stream_function, name}` (`streamlib-api-server/src/mcp.rs:313-329`).
    Its descriptions (`:318,322`) and the server instructions (`:90`) say "the sole `@stream`" and
    "run `uv sync` in it first".
  - Tools are `#[tool]` methods in `impl LocalApiMcpServerHandler` (`:446-889`), calling the
    engine trait `OperationsOnTheStreamsLoadedInThisRuntime`
    (`streamlib-engine/src/core/runtime/operations_on_the_streams_loaded_in_this_runtime.rs:38`;
    stub `streamlib-api-server/src/control_plane_stub_support.rs:240`).
  - `CONTROL_TOOL_NAMES` (`mcp.rs:1152-1171`) is pinned to `tools/list` (`:1569`).
  - `RunStreamToolResult{stream, display_name, stream_instance, location, node_count,
    replaced_a_recorded_stream, compile_warnings}`
    (`streamlib-runtime-client-contract/src/local_api_wire_contract.rs:84-101`) describes one
    stream. `STREAM_LOAD_TOOL_CALL_TIMEOUT` is 360s (`:27`).
- **The CLI's `run` and `dev`.**
  - `run` prints one stream (`tatolab-cli/src/stream_actions_on_the_runtime.rs:272-288`).
  - `dev` (`stream_followed_and_reloaded_on_every_save.rs:78-159,307-520`) tracks one
    `FollowedStreamPosition` and polls `logs` for one stream.
- **The runtime unit.** `cargo xtask build-runtime` (`xtask/src/build_runtime.rs:184`) copies
  `tatolabd`, `tatolab` and `moq-relay` into `bin/` (`:402-428`).
  - `moq-relay` is built with `cargo install` at a pinned version (`xtask/src/stage_local_relay.rs:17-87`).
  - `tatolabd` finds it beside its own executable
    (`tatolabd/src/local_relay_adapter_of_this_runtime.rs:56-73`;
    `streamlib-relay-adapter-local/src/lib.rs:535-541`). Inside the bundle that is
    `Contents/MacOS/moq-relay`.
  - The pinned-download precedent: `scripts/stage_{macos,linux}_bundled_vulkan_driver.sh` pin a
    URL and its sha256.
  - `bin/moq-relay` is named in three places: the portability gate's `RUNTIME_UNIT_EXECUTABLE_NAMES`
    (`sdk/streamlib-python-wheel/tests_against_the_runtime_unit_lend/test_wheel_portability.py:112`),
    `installer/install.sh:109-110,130-138`, and the macOS codesign check
    (`.github/workflows/reusable-macos-runtime-unit.yml:103-108`).
- **No home helper and no name generator.** The only HOME resolver is the state directory
  (`streamlib-runtime-client-contract/src/tatolab_state_directory.rs:134-160`, with a test-root
  override at `:136-141`). The only randomness dependency is `uuid` v4
  (`streamlib-engine/Cargo.toml:111`).
- **Tatolab.app** (tatolab-app `main`): it scaffolds with `tatolab new`, syncs with its own uv
  (`StreamProjectPythonEnvironments`), parses `stream.py` with a regex
  (`stream_project_folder_inspection.ts:27`) and loads one picked function. Its follow-up is
  tatolab-app #100.

## ADDED

- **The runtime unit's uv.** `xtask/src/stage_runtime_unit_uv.rs`, called by `build-runtime`,
  downloads uv's release archive for the target, checks its sha256 and copies the `uv`
  executable into `bin/`.
  - The pin is uv `0.13.1`, as tatolab-app ships today, with one URL and sha256 per target:
    `x86_64-unknown-linux-gnu` and `aarch64-apple-darwin`. If the gnu build fails the glibc 2.28
    floor, the musl build is used.
  - `tatolabd` finds it beside its own executable through `runtime_unit_executable_beside_this_one`,
    the lookup it finds the relay through. Inside the bundle that is `Contents/MacOS/uv`.
  - A unit without it refuses every venv-making call: "the runtime unit carries no uv; rebuild it
    with `cargo xtask build-runtime`".
- **Making the venv.** `streamlib-engine/src/core/runtime/project_venv_made_with_the_runtime_unit_uv.rs`:
  `make_the_projects_venv_unless_its_interpreter_runs(project_directory)`.
  - It first runs `.venv/bin/python -I -c ""` under a 10s bound. If that exits 0 it returns,
    leaving the venv untouched.
  - Otherwise it removes `.venv` and runs `<uv> sync` in the project directory.
  - It strips `VIRTUAL_ENV`, `PYTHON*` and `UV_PROJECT_ENVIRONMENT`, so uv takes
    `.python-version` and fetches a missing Python. uv's own cache and Python store stay at their
    defaults, shared with any uv the user runs.
  - It is bounded by `PROJECT_VENV_SYNC_BOUND` (240s), and uses the compile's spawn, reader,
    wait and kill pattern.
  - A non-zero exit removes the half-made `.venv` and returns uv's stderr tail verbatim as the
    reason the caller records.
- **Every `@stream` in one compile.** The compile entry gains `--every-stream`. With it the
  document is `{project_directory, streams: [...]}`, in definition order.
  - Each entry is `{stream_function, description, stream_graph}`, or
    `{stream_function, description, failed_because}` when that function raised. In the second
    case `failed_because` is the traceback with the launcher frames stripped, as the entry strips
    them today (`:105-118`).
  - An entry that cannot be imported writes `{project_directory, import_failed_because}`, its
    traceback, and exits 0, as a single-function compile does; exit 1 is the caller's own
    refusal alone (session, 2026-10-29).
  - The runtime's parse refuses a document without `streams` as an older `tatolab-stream`, as it
    does today for one without `stream_function` (`:265-272`).
- **Listing a project's streams.** The compile entry gains `--list-streams`, which imports the
  entry and writes `{project_directory, streams: [{stream_function, description}]}` without
  building any graph.
  - The tool `list_project_streams {project_directory}` makes the venv first, then runs it. It
    returns each stream's `stream_function`, its `description`, and its `stream` id and `state`
    when the runtime holds it.
  - If the entry cannot be imported, it returns `{project_directory, import_failed_because}`,
    the traceback, in place of `streams`.
  - Types: `ListProjectStreamsToolResult` and `ListProjectStreamsToolResultStream` in the wire
    contract.
- **The project name.** `streamlib-engine/src/core/runtime/new_project_name_drawing.rs`
  embeds two lowercase ASCII word lists, adjectives and nouns, of a few hundred words each,
  each word already a fixed point of the exposed-name cast.
  - `draw_new_project_directory_under(streams_root)` takes its randomness from `uuid::Uuid::new_v4()`
    bytes, so it adds no dependency.
  - It creates the directory with `fs::create_dir`, so a directory that already exists, or a
    concurrent create, draws again. After 8 draws it refuses, naming the root.
  - `streams_root` is `$HOME/streams`, created if missing, or a directory under the test root
    behind the `machine-directories-under-a-test-root` feature.
- **A failed stream with no graph.**
  - `StreamRecord.graph` becomes `Option<serde_json::Value>`. A schema-3 file's present value
    reads as `Some`, so the schema version stays 3.
  - `None` is a stream whose compile failed. `start_stream` and the re-load at start compile a
    graphless record from its location instead of loading a graph.
- **The project placeholder.** When the project's venv cannot be made or its entry cannot be
  imported, and no record holds the project, the load writes one `failed` record.
  - Its location is the project directory with `stream_function` empty, so its id is
    `<folder>-<hash>`. Its display name is the folder's name, and its `failed_because` is the
    traceback or uv's tail.
  - A later load of the project that compiles replaces it with the project's streams and
    forgets it. `start_stream` on it is a load of the project.
- **The scaffold, in the runtime.** `streamlib-engine/src/core/runtime/new_project_scaffold.rs`
  holds `render_scaffold_template_files` and `python_distribution_name_for`. These move whole
  from the CLI, with their tests and their refusal of a directory that already holds a
  scaffolded file.
- **Creating a project.** `Runner::create_project(CreateProjectRequest{project_directory: Option<PathBuf>})`
  on `OperationsOnTheStreamsLoadedInThisRuntime`. It runs, in order:
  1. resolve or draw the directory;
  2. refuse existing scaffold files;
  3. write the scaffold;
  4. `run_stream` with no function and the folder's name as the display name, which makes the
     venv as every load does.
  - After step 3 nothing refuses: a failed venv or compile is step 4's placeholder or failed
    records. The reply is the run's result plus `project_directory`.
  - The tool is `create_project {project_directory?}`; a relative directory is refused, as
    `run_stream` refuses one. `CreateProjectToolResult` goes in the wire contract.
  - The budget, `PROJECT_VENV_SYNC_BOUND` plus the compile and load bounds, sits under a new
    `PROJECT_CREATE_TOOL_CALL_TIMEOUT` (600s), held by the const-asserts beside `:274-313`.

## MODIFIED

- **`run_stream` loads a project and never refuses for code.** `Runner::run_stream` (`:351-470`):
  - It runs `make_the_projects_venv_unless_its_interpreter_runs`, then the compile. With no
    function named, the compile is `--every-stream`. The load budget grows by
    `PROJECT_VENV_SYNC_BOUND`, and the compile op's missing-interpreter refusal goes.
  - It holds one acquisition of the one-action lock for every stream the document lists. Each
    stream gets its own id, record and logs:
    - a stream with a graph is loaded and started, replacing its recorded stream in place;
    - a stream whose function raised, or whose describe or start fails, is recorded `failed`
      with the reason, and its previous running version is unloaded.
  - `replace_a_recorded_stream`'s restore of the previous record on failure (`:1162-1169`) goes.
  - When the venv cannot be made or the entry cannot be imported:
    - every record whose location is this project is unloaded and recorded `failed` with the
      reason;
    - with none, the placeholder is written.
  - A recorded stream whose `project_directory` is this one and whose function's module is the
    entry module, but which the document no longer holds, is unloaded and recorded `failed`:
    "`<module:function>` is no longer defined in stream.py".
  - Refused, with nothing changed (the §Product list):
    - a directory that is missing or relative;
    - a folder with neither `stream.py` nor `pyproject.toml`;
    - a named function the document does not hold and no record holds;
    - a display name when the document holds more than one stream;
    - an id held by another location.
  - A named function no longer defined but recorded becomes `failed`, as above.
- **`start_stream`, and the re-load at start** (`:554-590`, `:838-1000`):
  - Each first runs `make_the_projects_venv_unless_its_interpreter_runs`. A sync failure records
    the stream `failed` in uv's words.
  - A graphless record, or the placeholder, is compiled from its location.
  - `load_a_recorded_stream_from_its_record`'s "is gone; run `uv sync`" refusal (`:1177-1190`)
    goes. The re-load at start syncs inline, one record at a time under the lock, as it loads
    today.
- **`run_stream`'s result.** `RunStreamToolResult` becomes `{project_directory, streams:
  [RunStreamToolResultStream], compile_warnings}`.
  - Each stream carries today's per-stream fields (`stream`, `display_name`, `stream_instance`,
    `location`, `node_count`, `replaced_a_recorded_stream`) plus `state`, `running` or `failed`,
    and `failed_because`. A failed stream has no `stream_instance` or `node_count`.
  - `StreamRunOutcome` follows.
  - The descriptions of `run_stream`, its arguments and the server instructions
    (`mcp.rs:90,318,322,612`) drop "sole" and "`uv sync`", and say that a failed stream is
    returned, not refused.
- **`tatolab new [DIRECTORY]`** (`main.rs:112-115,289-291`): the directory becomes optional and
  resolves against the working directory. The verb is a `create_project` call, and like every
  verb it fails with no runtime. It prints the `run` line for the stream, then
  `follow it: cd <dir> && tatolab dev`. The CLI no longer embeds the templates.
- **`tatolab run`** prints one line per stream loaded, a failed one as `<id> failed: <reason>`,
  then `follow it: tatolab logs -f` for one stream, or `tatolab logs -f <stream id>` per
  stream for several, since a bare `logs -f` refuses there.
  - It exits 0 when every stream is running, and 1 when any is `failed` or the load is refused
    (`main.rs:118-123` long_about).
  - A `--name` with no target in a project of several streams is refused by the runtime.
- **`tatolab dev`** follows every stream its load returned. `FollowedStreamPosition` becomes one
  per stream id, each with its own `after`, and each record prints prefixed `[<display name>] `.
  On save it re-loads the project with one `run_stream`, then follows the streams that load
  returned, failed ones included, so a traceback shows in the followed records. A stream that
  vanished from `stream.py` prints its reason once and is dropped from following.
- **A bare `tatolab logs -f`** in a several-stream directory keeps today's refusal
  (`stream_argument_resolution_through_the_listing.rs:20-60`).
- **The scaffold's `stream.py`** module docstring (`scaffold_template/stream.py:6-8`) says
  `tatolab dev` loads every `@stream` below. `test_scaffold_template.py:126-138` still pins one
  stream, `main`.
- **The runtime unit's executable lists** gain `uv`:
  - `RUNTIME_UNIT_EXECUTABLE_NAMES` (`test_wheel_portability.py:112`);
  - `install.sh`'s unpacked check and completeness check (`:109-110,130-138`). `uv` is not linked
    into `~/.local/bin`;
  - `reusable-macos-runtime-unit.yml:103-108`.
- **`README.md`**'s `uv sync` before `tatolab dev` and its missing-venv line describe the new
  path.
- **Plan text** when this ships: every `amended by` pointer of the three decisions folds in, and the
  §Control plane inventory names `create_project` and `list_project_streams` as stream actions.

## REMOVED

- REMOVED: runtime/tatolab-cli/src/scaffold_new_stream_project.rs
- REMOVED: scaffold_new_stream_project
  The scaffold moves to the engine as `new_project_scaffold`.
- REMOVED: Tatolab stream project
  The CLI's next-steps text and its test assertion.
- REMOVED: has no .venv/bin/python
  The compile op's refusal, its tests, the CLI test at `tatolab_dev_and_logs_follow.rs:374`,
  and `README.md`.
- REMOVED: is gone; run
  The record re-load's missing-interpreter refusal (`stream_actions_of_this_runtime.rs:1183`).
- REMOVED: @stream functions:
  The several-streams refusal in `select_stream_function`.
- REMOVED: test_several_streams_are_refused_listing_each_with_its_description
- REMOVED: test_the_several_streams_suggestion_quotes_the_dir_it_was_given
- REMOVED: test_the_several_streams_suggestion_names_no_dir_when_none_was_given

## Test plan

- **Stream suite** (`sdk/tatolab-stream/tests/test_project_stream_compile_entry.py`):
  - `--every-stream` writes every `@stream` in definition order, with its `stream_graph`,
    `stream_function` and `description`;
  - a raising function becomes its own `failed_because` entry while the others compile;
  - an unimportable entry writes its traceback as `import_failed_because`;
  - imported and aliased streams count as today (`:544`, `:748`);
  - `--list-streams` builds no graph.
- **Runtime suite, engine** (`ProjectWithAStubCompile`, `stream_actions_of_this_runtime.rs:1480-1630`):
  - Loading:
    - three stubs load as three streams, records and ids;
    - one raising stub is `failed` while two run;
    - a stub whose describe fails is `failed`, and its old version is unloaded, not restored;
    - a re-load with one function gone records it `failed`, naming it;
    - `--name` across several streams, a relative directory and an unknown, unrecorded function
      are refused with nothing changed.
  - Import failure:
    - an unimportable entry marks every recorded stream of the project `failed`;
    - with none recorded, it writes the `<folder>-<hash>` placeholder, which the next good load
      replaces.
  - The venv:
    - a missing `.venv`, or one whose `bin/python` does not run, runs a stub `uv` first;
    - one that runs is untouched;
    - a stub uv failure records `failed` in its words and removes the half-made `.venv`;
    - `start_stream` and the re-load at start do the same, and compile a graphless record.
  - The record: a schema-3 file reads with `graph` as `Some`, and `None` round-trips.
- **Runtime suite, name drawing:**
  - every word is a cast fixed point;
  - a taken directory draws again;
  - 8 taken draws refuse;
  - two racing creates get two directories.
- **Runtime suite, api-server** (stub runtime):
  - `create_project`: with a directory; with none (under the test root); over a scaffold file
    (refused, nothing written); with a relative path (refused); with a failing stub compile
    (a failed stream returned, not a refusal);
  - `list_project_streams`: loaded and unloaded streams, no venv, and import failure;
  - `run_stream`'s result with running and failed streams;
  - the `CONTROL_TOOL_NAMES` and schema pins.
- **CLI tests** (`tests/tatolab_new_and_help.rs`, `tatolab_stream_actions.rs`,
  `tatolab_dev_and_logs_follow.rs`):
  - `new` with and without a directory;
  - `run` printing several streams, and exiting 1 with one failed;
  - `dev` following two prefixed streams, showing a failed stream's traceback, and dropping one
    deleted from `stream.py`.
- **Runtime unit:** the portability gate, `install.sh` and the macOS codesign step each refuse a
  unit without `bin/uv`.
- **Integration suite** (`tests/stream-on-runtime/`, GPU-free half, both lanes, no uv on `PATH`):
  1. over `tatolab mcp`, `create_project` with no directory;
  2. reconnect; `list_streams` shows it running under its folder's name;
  3. delete `.venv` and `start_stream`: it is made again;
  4. add a second `@stream` that raises and `run_stream`: one `running`, one `failed` with the
     traceback in `logs`.

## Out of scope

- Tatolab.app's move onto `create_project` and `list_project_streams`, and the retirement of its
  own uv, Python and Python picker: tatolab-app #100. Its `run_stream {stream_function}` calls
  keep working, but its read of `RunStreamToolResult` breaks on the new shape; that is #100's
  work.
- A call that proposes the default name before creating (the gap #100 notes).
