# create-project

Built from #2951 and the align merged in #2968. It builds two §Product entries:
- "Creating a project" (ADR `docs/decisions/create-project.md`);
- "Loading a project loads every stream in it" (ADR `docs/decisions/project-loads-every-stream.md`).

Today `tatolab new <dir>` writes the scaffold in the CLI and loads nothing. A load needs a
`.venv` someone else made. A load naming no function takes the sole `@stream` and refuses
when there are several. Tatolab.app works around all three in its own code.

**After this change:**
- **Create.** The runtime creates a project in one call: it scaffolds, makes the `.venv` with
  the uv it carries, then loads. `tatolab new`, a new `create_project` tool and the app share
  that call. With no directory it creates `~/streams/<adjective-noun>`.
- **Load.** Every load of a project with no `.venv/bin/python` makes one first. A load naming
  no function loads every `@stream` in `stream.py`, each as its own stream.
- **List.** `list_project_streams` lists a project's streams.

**Scale gate — this skill. Both ADRs already exist from the align.**
- **Contracts changed:**
  - the local API: `run_stream`'s result, and two new tools;
  - the CLI's `new`, `run` and `dev`;
  - the compile document the runtime reads from the project's interpreter;
  - the runtime unit's contents, on both floors.
- **Not touched:** the RHI, the iceoryx2 wire, the processor model, `@stream` and the rest of
  the authoring API, the stream record's schema and the stream id.

**Precondition.** Every touched section is DECIDED: §Product, §Distribution & versioning (the
Linux installer and `Tatolab.app`) and §Control plane (the tool inventory). Each carries
`amended by create-project` or `amended by project-loads-every-stream`.

## Current state (recon, 2026-10-28, `main` `96030eb5b`)

Paths are under `runtime/` unless marked otherwise.

- **`tatolab new`** (`tatolab-cli/src/main.rs:112-115`, dispatched at `:289-291`) takes a
  required directory. `scaffold_new_stream_project.rs`:
  - embeds the seven templates from `sdk/tatolab-stream/scaffold_template/` (`:14-27`);
  - names the distribution after the folder (`:90-131`);
  - refuses before writing when any scaffolded path exists (`:146-160`);
  - prints "Created a Tatolab stream project … uv sync / tatolab dev" (`:182-186`).
  - The templates are embedded in the CLI binary only.
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
  - `tatolabd` finds it beside its own executable through `the_uv_executable_beside_this_one`,
    which mirrors the relay's lookup. Inside the bundle that is `Contents/MacOS/uv`.
  - A unit without it refuses every venv-making call: "the runtime unit carries no uv; rebuild it
    with `cargo xtask build-runtime`".
- **Making the venv.** `streamlib-engine/src/core/runtime/project_venv_made_with_the_runtime_unit_uv.rs`:
  `make_the_projects_venv(project_directory)` runs `<uv> sync` in the project directory.
  - It strips `VIRTUAL_ENV`, `PYTHON*` and `UV_PROJECT_ENVIRONMENT`, so uv takes
    `.python-version` and fetches a missing Python. uv's own cache and Python store stay at their
    defaults, shared with any uv the user runs.
  - It is bounded by `PROJECT_VENV_SYNC_BOUND` (240s), and uses the compile's spawn, reader,
    wait and kill pattern.
  - A non-zero exit removes `.venv` only if this call created it, and is refused with uv's
    stderr tail verbatim.
- **Every `@stream` in one compile.** The compile entry gains `--every-stream`. With it the
  document is `{project_directory, streams: [{stream_graph, stream_function, description}]}`,
  in definition order. A failure of any one stream's function refuses the whole compile, naming
  it. The runtime's parse refuses a document without `streams` as an older `tatolab-stream`,
  as it does today for one without `stream_function` (`:265-272`).
- **Listing a project's streams.** The compile entry gains `--list-streams`, which imports the
  entry and writes `{project_directory, streams: [{stream_function, description}]}` without
  building any graph.
  - The tool `list_project_streams {project_directory}` runs it and returns each stream's
    `stream_function`, its `description`, and its `stream` id and `state` when the runtime
    holds it.
  - Types: `ListProjectStreamsToolResult` and `ListProjectStreamsToolResultStream` in the wire
    contract.
  - [NEEDS DECISION — `list_project_streams` on a project with no `.venv`. The import needs the
    project's interpreter.]:
    - **A.** Refuse it, naming the project and that a load makes the venv. Listing stays a read
      with no side effect. *(recommended — the app's Add loads every stream anyway, so it needs
      no listing first)*
    - **B.** Make the venv, then list. One call always answers, but a read tool writes to disk
      and can take minutes.
- **The project name.** `streamlib-engine/src/core/runtime/new_project_name_drawing.rs`
  embeds two lowercase ASCII word lists, adjectives and nouns, of a few hundred words each,
  each word already a fixed point of the exposed-name cast.
  - `draw_new_project_directory_under(streams_root)` takes its randomness from `uuid::Uuid::new_v4()`
    bytes, so it adds no dependency.
  - It creates the directory with `fs::create_dir`, so a directory that already exists, or a
    concurrent create, draws again. After 8 draws it refuses, naming the root.
  - `streams_root` is `$HOME/streams`, created if missing, or a directory under the test root
    behind the `machine-directories-under-a-test-root` feature.
- **The scaffold, in the runtime.** `streamlib-engine/src/core/runtime/new_project_scaffold.rs`
  holds `render_scaffold_template_files` and `python_distribution_name_for`. These move whole
  from the CLI, with their tests and their refusal of a directory that already holds a
  scaffolded file.
- **Creating a project.** `Runner::create_project(CreateProjectRequest{project_directory: Option<PathBuf>})`
  on `OperationsOnTheStreamsLoadedInThisRuntime`. It runs, in order:
  1. resolve or draw the directory;
  2. refuse existing scaffold files;
  3. write the scaffold;
  4. `make_the_projects_venv`;
  5. `run_stream` with no function and the folder's name as the display name.
  - A refusal after step 3 leaves the written project in place, unrecorded, and names it so a
    later `run_stream` can retry. The reply is the run's result plus `project_directory`.
  - The tool is `create_project {project_directory?}`; a relative directory is refused, as
    `run_stream` refuses one. `CreateProjectToolResult` goes in the wire contract.
  - The budget, `PROJECT_VENV_SYNC_BOUND` plus the compile and load bounds, sits under a new
    `PROJECT_CREATE_TOOL_CALL_TIMEOUT` (600s), held by the const-asserts beside `:274-313`.
- **Tests.** Every item in "Test plan" is added work.

## MODIFIED

- **`run_stream` loads a project.** `Runner::run_stream` (`:351-470`):
  - With no function, it compiles with `--every-stream` and loads each stream the document
    holds under one acquisition of the one-action lock.
  - Each stream gets its own id, record and logs, and replaces its recorded stream in place, as
    today.
  - A recorded stream whose `project_directory` is this one and whose function's module is the
    entry module, but which the document no longer holds, is unloaded and recorded `failed`:
    "`<module:function>` is no longer defined in stream.py".
  - A display name given with no function is refused when the document holds more than one
    stream.
  - With a function named, the load behaves as today.
  - `.venv/bin/python` absent → `make_the_projects_venv` first, then the compile, inside the load
    budget, which grows by `PROJECT_VENV_SYNC_BOUND`. The compile op's missing-interpreter
    refusal goes.
  - [NEEDS DECISION — a project load whose compile succeeds but one stream's load fails
    (a node type that will not describe, a processor interpreter that will not start)]:
    - **A.** All or nothing: every stream the call touched returns to its prior state, as one
      stream's refused replace does today. *(recommended — "a refused load leaves every stream
      as it was" stays true at the project level)*
    - **B.** Each stream on its own: the loads that succeeded stay and the failed one is refused
      in the reply.
- **`run_stream`'s result.** `RunStreamToolResult` becomes `{project_directory, streams:
  [RunStreamToolResultStream], compile_warnings}`. Each stream carries today's per-stream
  fields: `stream`, `display_name`, `stream_instance`, `location`, `node_count`,
  `replaced_a_recorded_stream`. `StreamRunOutcome` follows. The descriptions of `run_stream`,
  its arguments and the server instructions (`mcp.rs:90,318,322,612`) drop "sole" and
  "`uv sync`".
- **A start or re-load from a record with no interpreter** stays `failed`, as §Product decides.
  The reason (`stream_actions_of_this_runtime.rs:1183`) points at `tatolab run` in the project,
  which now makes the venv, instead of `uv sync`.
- **`tatolab new [DIRECTORY]`** (`main.rs:112-115,289-291`): the directory becomes optional and
  resolves against the working directory. The verb is a `create_project` call, and like every
  verb it fails with no runtime. It prints the `run` line for the stream, then
  `follow it: cd <dir> && tatolab dev`. The CLI no longer embeds the templates.
- **`tatolab run`** prints one line per stream loaded, then `follow it: tatolab logs -f`. A
  `--name` with no target in a project of several streams is refused by the runtime.
- **`tatolab dev`** follows every stream its load returned. `FollowedStreamPosition` becomes one
  per stream id, each with its own `after`, and each record prints prefixed `[<display name>] `.
  On save it re-loads the project with one `run_stream`, then follows the streams that load
  returned. A stream the re-load recorded `failed` prints its reason once and is dropped from
  following.
- **`tatolab logs -f` without a stream argument**, in a directory holding several streams, keeps
  today's refusal listing them
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
- **Plan text** when this ships: the pointers on the six amended entries fold in, and the
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
- REMOVED: @stream functions:
  The several-streams refusal in `select_stream_function`. A load naming no function takes
  every stream.
- REMOVED: test_several_streams_are_refused_listing_each_with_its_description
- REMOVED: test_the_several_streams_suggestion_quotes_the_dir_it_was_given
- REMOVED: test_the_several_streams_suggestion_names_no_dir_when_none_was_given

## Test plan

- **Stream suite** (`sdk/tatolab-stream/tests/test_project_stream_compile_entry.py`):
  - `--every-stream` writes every `@stream` in definition order, each with its graph, location
    and docstring;
  - one failing function refuses the whole document, naming it;
  - an imported or aliased stream counts as today (`:544`, `:748`);
  - `--list-streams` builds no graph;
  - an entry with no stream is still refused with the sample (`:766`).
- **Runtime suite, engine** (`ProjectWithAStubCompile`, `stream_actions_of_this_runtime.rs:1480-1630`):
  - a project of three stubs loads three streams, three records and three ids;
  - a re-load with one function gone leaves it `failed`, naming the function, and replaces the
    others;
  - `--name` across several streams is refused;
  - a failed compile leaves every stream as it was;
  - a missing `.venv` runs a stub `uv` before the compile;
  - a stub uv's non-zero exit is refused verbatim, and the `.venv` it made is removed while a
    pre-existing one is kept;
  - the decided case of the NEEDS DECISION block.
- **Runtime suite, name drawing:**
  - every word is a cast fixed point;
  - an existing directory draws again;
  - 8 taken draws refuse;
  - a racing pair of creates gets two directories.
- **Runtime suite, api-server** (stub runtime):
  - `create_project` with a directory, with none (under the test root), with a scaffold file
    present (refused, nothing written), and with a relative path (refused);
  - `list_project_streams` with loaded and unloaded streams;
  - `run_stream`'s new result shape;
  - `CONTROL_TOOL_NAMES` and the schema property pins.
- **CLI tests** (`tests/tatolab_new_and_help.rs`, `tatolab_stream_actions.rs`,
  `tatolab_dev_and_logs_follow.rs`):
  - `new` with and without a directory against a stub runtime;
  - `run` printing several streams;
  - `dev` following two streams with prefixed records, and dropping one deleted from
    `stream.py` on save.
- **Runtime unit:** the portability gate, `install.sh`'s check and the macOS codesign step each
  refuse a unit without `bin/uv`.
- **Integration suite** (`tests/stream-on-runtime/`, GPU-free half, both lanes):
  1. over `tatolab mcp`, `create_project` with no directory;
  2. close the connection and reconnect;
  3. `list_streams` shows it running under its folder's name, and `list_project_streams` shows
     `main`;
  4. add a second `@stream` and `run_stream` the project;
  5. two streams are listed.
  The runtime unit's own uv makes the venv, and the CI runner has no uv on `PATH` for this test.

## Out of scope

- Tatolab.app's move onto `create_project` and `list_project_streams`, and the retirement of its
  own uv, Python and Python picker: tatolab-app #100. After this change the app's
  `run_stream {stream_function}` calls still work. Its read of `RunStreamToolResult` breaks on
  the new shape, which is that issue's work.
- A call that proposes the default name before creating, which #100 notes as a gap.
