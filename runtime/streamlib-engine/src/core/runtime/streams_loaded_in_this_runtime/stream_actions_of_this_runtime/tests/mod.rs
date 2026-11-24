// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// What [`Runner::remove_one_stream`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StreamRemoveOutcome {
    /// The stream's id.
    pub stream_id: String,
    /// Whether the stream was loaded and is now unloaded.
    pub unloaded: bool,
    /// Whether the stream was recorded and its record is now gone.
    pub forgotten: bool,
}

impl Runner {
    /// Compile and load `request` on this thread, as a stream action's load
    /// does, with no action queued and a machine shutdown its interrupt.
    fn load_the_project_off_its_queue(
        &self,
        request: ProjectStreamsToReadAndLoad,
    ) -> Result<StreamRunOutcome> {
        self.run_the_project_reporting_to(request, None, &is_the_machines_shutdown_requested)
    }

    /// Forget the record of the stream whose id is `stream_id` — one an earlier
    /// runtime wrote included — then unload it if it is loaded. Refused when the
    /// runtime holds no stream of that id.
    fn remove_one_stream(&self, stream_id: &str) -> Result<StreamRemoveOutcome> {
        let _one_stream_action_at_a_time = self.stream_actions.one_stream_action_at_a_time.lock();
        let loaded = self.loaded_stream_of_the_id_when_loaded(stream_id);
        let forgotten = match self.stream_records() {
            Some(stream_records) => stream_records.remove(stream_id)?,
            None => false,
        };
        if loaded.is_none() && !forgotten {
            return Err(self.a_stream_this_runtime_does_not_hold(stream_id));
        }
        if let Some(loaded) = &loaded {
            unload_a_stream_an_action_took_back(loaded, "its owner removed it");
        }
        Ok(StreamRemoveOutcome {
            stream_id: stream_id.to_string(),
            unloaded: loaded.is_some(),
            forgotten,
        })
    }

    /// Re-load every recorded stream not closed on this thread, as each project's
    /// re-load at the start does.
    fn reload_every_recorded_stream_not_closed_at_once(&self) -> Vec<StreamReloadAtTheStart> {
        let Some(stream_records) = self.stream_records() else {
            return Vec::new();
        };
        self.reload_each_recorded_stream_listed(
            stream_records,
            stream_records.read_every(),
            &is_the_machines_shutdown_requested,
        )
    }
}

use crate::core::test_support::{
    MOCK_REFUSAL_OF_AN_ACCESS_NOBODY_ASKED_FOR, MockInputOnlyNode, MockOutputOnlyNode,
    MockSourceRefusingAnAccessNobodyAskedFor, a_temporary_directory_at_owner_only_mode,
    ensure_test_mocks_registered, write_an_executable_script_from_a_child_process,
};

use serial_test::serial;
use streamlib_runtime_client_contract::local_api_wire_contract::StreamHealthOnTheWire;

use std::collections::BTreeMap;

use std::time::{Duration, Instant};

use crate::core::runtime::STREAM_RECORD_SCHEMA_VERSION;

use streamlib_node_schema::NodeClassImportPath;

use streamlib_runtime_client_contract::directory_at_an_explicit_mode;

const LEND_DIRECTORY_FOR_TEST: &str = "/opt/tatolab/lib/tatolab/lend";

/// A runner handed a lend directory, recording its streams in
/// `streams_directory` when one is given.
fn a_runner_recording_its_streams_in(streams_directory: Option<&Path>) -> Arc<Runner> {
    ensure_test_mocks_registered();
    let runner = Runner::new().expect("the runner builds");
    runner
        .set_node_interpreter_lend_directory(PathBuf::from(LEND_DIRECTORY_FOR_TEST))
        .expect("the lend directory is handed over");
    if let Some(streams_directory) = streams_directory {
        runner
            .record_streams_in_the_state_directory(streams_directory)
            .expect("the runner records its streams there");
    }
    runner
}

/// The graph a stream named `stream_name` holds: one node, `source`, with the output
/// ports `out1` and `out2`.
fn the_graph_of_a_stream_named(stream_name: &str) -> serde_json::Value {
    serde_json::json!({
        "stream": stream_name,
        "nodes": [{
            "name": "source",
            "type": MockOutputOnlyNode::Node::node_class_import_path().as_str(),
        }],
        "links": [],
    })
}

/// The graph of a stream named `stream_name` holding a described source and a node of a
/// type no module provides.
fn the_graph_of_a_stream_naming_a_type_not_described(stream_name: &str) -> serde_json::Value {
    let mut graph = the_graph_of_a_stream_named(stream_name);
    let source = graph["nodes"][0].clone();
    graph["nodes"] = serde_json::json!([source, {
        "name": "absent",
        "type": "tatolab.absent:AbsentNodeType",
        "input_values": {"strength": 3},
    }]);
    graph
}

/// How a stub `.venv/bin/python` answers the runtime's probe of whether the interpreter
/// runs, `-I -c ""`: it exits 0.
const STUB_VENV_INTERPRETER_ANSWERING_THE_PROBE: &str = "#!/bin/sh\nexit 0\n";

/// The name every test project's `tatolab-project.json` gives it.
const TEST_PROJECT_NAME: &str = "rig";

/// A temporary directory spelled as the runtime keys a project, symlinks resolved:
/// macOS's temporary directory sits under `/var`, a link to `/private/var`.
fn resolved_temporary_directory(temporary_directory: &tempfile::TempDir) -> PathBuf {
    std::fs::canonicalize(temporary_directory.path()).expect("a resolvable temporary directory")
}

/// A project of its own UUID whose streams are the graph documents under `streams/`, and
/// whose `.venv/bin/python` is a shell script that answers the runtime's probe.
struct ProjectWithGraphDocuments {
    project_directory: PathBuf,
    project: Uuid,
    _directory_removed_on_drop: Arc<tempfile::TempDir>,
}

impl ProjectWithGraphDocuments {
    /// A project with a venv holding one graph document, `graph`'s, named after its
    /// `stream`.
    fn holding(graph: serde_json::Value) -> Self {
        let project = Self::with_no_venv().given_a_venv();
        project.write_the_graph_document(graph);
        project
    }

    /// A project with no venv and no graph document.
    fn with_no_venv() -> Self {
        let temporary_directory =
            a_temporary_directory_at_owner_only_mode().expect("a project directory");
        Self::of_the_project_at(
            resolved_temporary_directory(&temporary_directory),
            Uuid::new_v4(),
            Arc::new(temporary_directory),
        )
    }

    fn of_the_project_at(
        project_directory: PathBuf,
        project: Uuid,
        directory_removed_on_drop: Arc<tempfile::TempDir>,
    ) -> Self {
        directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode(
            &project_directory,
            0o700,
        )
        .expect("the project directory");
        std::fs::write(
            project_directory.join("pyproject.toml"),
            "[project]\nname = \"project\"\n",
        )
        .expect("the project's pyproject.toml");
        let project_file = TatolabProjectFile {
            project,
            name: TEST_PROJECT_NAME.to_string(),
            description: None,
            cover: None,
            created_with_runtime: None,
            dependencies: Vec::new(),
            node_type_groups: Default::default(),
        };
        std::fs::write(
            project_directory.join(PROJECT_FILE_NAME),
            project_file
                .canonical_bytes()
                .expect("the project file serializes"),
        )
        .expect("the project's tatolab-project.json");
        Self {
            project_directory,
            project,
            _directory_removed_on_drop: directory_removed_on_drop,
        }
    }

    /// A second checkout of this project — its project UUID — at another directory,
    /// with a venv and no graph document.
    fn another_copy(&self) -> Self {
        let temporary_directory =
            a_temporary_directory_at_owner_only_mode().expect("a project directory");
        Self::of_the_project_at(
            resolved_temporary_directory(&temporary_directory),
            self.project,
            Arc::new(temporary_directory),
        )
        .given_a_venv()
    }

    /// Two projects with no venv whose stream `stream_name` computes to one stream id:
    /// project UUIDs drawn until their hashes' six hex digits collide.
    fn two_with_no_venv_whose_ids_collide(stream_name: &str) -> (Self, Self) {
        let parent =
            Arc::new(a_temporary_directory_at_owner_only_mode().expect("a parent directory"));
        let resolved_parent = resolved_temporary_directory(&parent);
        let mut project_by_stream_id = std::collections::HashMap::new();
        let (first, second) = std::iter::repeat_with(Uuid::new_v4)
            .find_map(|project| {
                let stream_id = StreamLocation {
                    project_directory: PathBuf::new(),
                    project,
                    stream_asset: Self::the_stream_asset_in(project, stream_name),
                }
                .stream_id_named(TEST_PROJECT_NAME, stream_name);
                project_by_stream_id
                    .insert(stream_id, project)
                    .map(|colliding| (colliding, project))
            })
            .expect("two projects whose ids collide");
        let project_at = |project: Uuid| {
            Self::of_the_project_at(
                resolved_parent.join(project.to_string()),
                project,
                Arc::clone(&parent),
            )
        };
        (project_at(first), project_at(second))
    }

    /// This project, given a venv whose interpreter answers the runtime's probe.
    fn given_a_venv(self) -> Self {
        directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode(
            &self.path().join(".venv").join("bin"),
            0o755,
        )
        .expect("the venv's bin directory");
        write_an_executable_script_from_a_child_process(
            &self.interpreter(),
            STUB_VENV_INTERPRETER_ANSWERING_THE_PROBE,
        );
        crate::core::runtime::projects_and_their_venv::project_venv_made_with_the_runtime_unit_uv::record_the_venv_synced_with_what_the_project_holds_now(
            self.path(),
            &PackagesTheRuntimeUnitCarries::default(),
        )
        .expect("the venv's sync stamp");
        self
    }

    fn path(&self) -> &Path {
        &self.project_directory
    }

    fn interpreter(&self) -> PathBuf {
        self.path().join(".venv").join("bin").join("python")
    }

    fn stream_environment(&self) -> StreamEnvironment {
        StreamEnvironment {
            project_directory: self.path().to_path_buf(),
            interpreter: self.interpreter(),
        }
    }

    /// The stream asset UUID the stream `stream_name` of the project `project` is given.
    fn the_stream_asset_in(project: Uuid, stream_name: &str) -> Uuid {
        Uuid::new_v5(&project, stream_name.as_bytes())
    }

    /// The stream asset UUID of this project's stream `stream_name`.
    fn stream_asset_of(&self, stream_name: &str) -> Uuid {
        Self::the_stream_asset_in(self.project, stream_name)
    }

    /// Where this project's graph document of the stream `stream_name` sits.
    fn graph_document_path_of(&self, stream_name: &str) -> PathBuf {
        self.path()
            .join("streams")
            .join(format!("{stream_name}.graph.json"))
    }

    /// Where this project's stream `stream_name` lives.
    fn location_of(&self, stream_name: &str) -> StreamLocation {
        StreamLocation {
            project_directory: self.path().to_path_buf(),
            project: self.project,
            stream_asset: self.stream_asset_of(stream_name),
        }
    }

    /// The stream id a first run records this project's stream `stream_name` under.
    fn stream_id_of(&self, stream_name: &str) -> String {
        self.location_of(stream_name)
            .stream_id_named(TEST_PROJECT_NAME, stream_name)
    }

    /// Write `graph` as the graph document of the stream its `stream` names, holding that
    /// stream's asset UUID and, as the runtime writes one, a UUID for each node holding
    /// none, the same one for a node of one name in every write.
    fn write_the_graph_document(&self, graph: serde_json::Value) {
        let stream_name = graph
            .get("stream")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("main")
            .to_string();
        let mut document = graph;
        let stream_asset = self.stream_asset_of(&stream_name);
        if let Some(nodes) = document["nodes"].as_array_mut() {
            for node in nodes {
                if node.get("id").is_none() {
                    let node_name = node["name"].as_str().unwrap_or_default().to_string();
                    node["id"] = serde_json::json!(
                        Uuid::new_v5(&stream_asset, node_name.as_bytes()).to_string()
                    );
                }
            }
        }
        document["asset"] = serde_json::json!(stream_asset);
        self.write_the_graph_document_bytes(&stream_name, &document.to_string());
    }

    /// Write the graph document of the stream `stream_name` holding its asset UUID and a
    /// `nodes` that is no list, which does not load as a graph.
    fn write_an_unloadable_graph_document(&self, stream_name: &str) {
        self.write_the_graph_document_bytes(
            stream_name,
            &serde_json::json!({
                "asset": self.stream_asset_of(stream_name),
                "nodes": "not a list",
                "links": [],
            })
            .to_string(),
        );
    }

    fn write_the_graph_document_bytes(&self, stream_name: &str, document: &str) {
        let path = self.graph_document_path_of(stream_name);
        std::fs::create_dir_all(path.parent().expect("a streams directory"))
            .expect("the streams directory");
        std::fs::write(path, document).expect("the graph document");
    }

    /// Remove the graph document of the stream `stream_name`.
    fn remove_the_graph_document(&self, stream_name: &str) {
        std::fs::remove_file(self.graph_document_path_of(stream_name))
            .expect("the graph document is removed");
    }

    /// Make the graph documents under `streams/` exactly those of `streams_loading`, each
    /// its stream's graph, and of `streams_unloadable`, each one that does not load.
    fn hold_the_streams(&self, streams_loading: &[&str], streams_unloadable: &[&str]) {
        let _ = std::fs::remove_dir_all(self.path().join("streams"));
        for stream_name in streams_loading {
            self.write_the_graph_document(the_graph_of_a_stream_named(stream_name));
        }
        for stream_name in streams_unloadable {
            self.write_an_unloadable_graph_document(stream_name);
        }
    }

    fn run_request(&self) -> ProjectStreamsToReadAndLoad {
        ProjectStreamsToReadAndLoad {
            project_directory: self.path().to_path_buf(),
            only_the_stream_assets: None,
            playback_after_the_load: ProjectPlaybackAfterItsLoad::Playing,
            keeps_each_loaded_stream_whose_node_code_is_unchanged: false,
        }
    }
}

fn records_in(streams_directory: &Path) -> StreamRecordsInTheStateDirectory {
    StreamRecordsInTheStateDirectory::open(streams_directory).expect("the records open")
}

/// The record of `stream_id` in `streams_directory`, `None` when there is
/// none; one an earlier runtime wrote fails the test.
fn the_current_record_in(streams_directory: &Path, stream_id: &str) -> Option<StreamRecord> {
    match records_in(streams_directory)
        .read(stream_id)
        .expect("the record reads")
    {
        Some(StreamRecordAsRead::Current(record)) => Some(record),
        Some(earlier) => {
            panic!("`{stream_id}` is a record an earlier runtime wrote: {earlier:?}")
        }
        None => None,
    }
}

fn the_record_of(streams_directory: &Path, stream_id: &str) -> StreamRecord {
    the_current_record_in(streams_directory, stream_id).expect("the record is there")
}

/// A record of the stream `stream_name` of `project`.
fn a_record_of(project: &ProjectWithGraphDocuments, stream_name: &str) -> StreamRecord {
    StreamRecord::of_a_running_stream(
        project.stream_id_of(stream_name),
        project.location_of(stream_name),
        &project.stream_environment(),
    )
}

/// Write `record` and load it recorded by the runtime, not started — what
/// a recorded stream is between its load and its start.
fn a_recorded_stream_loaded_without_its_start(
    runner: &Runner,
    streams_directory: &Path,
    record: &StreamRecord,
) -> Arc<LoadedStreamInThisRuntime> {
    records_in(streams_directory)
        .write(record)
        .expect("the record is written");
    a_stream_loaded_from_an_unwritten_record(runner, record)
}

/// Load `record` recorded by the runtime, not started, without writing it —
/// a loaded stream whose record is gone — from a graph of one node.
fn a_stream_loaded_from_an_unwritten_record(
    runner: &Runner,
    record: &StreamRecord,
) -> Arc<LoadedStreamInThisRuntime> {
    let display_name = the_display_name_of_the_stream_at(&record.location, &record.stream_id);
    let graph = GraphSnapshot::from_graph_document(the_graph_of_a_stream_named(&display_name))
        .expect("the graph reads");
    runner
        .load_a_recorded_stream(OneStreamAStreamActionLoads::recorded_by(
            record,
            &display_name,
            &graph,
        ))
        .expect("the recorded stream loads")
}

/// The stream `stream_name` of `project` loaded by a library host under the id a run
/// records it under, not recorded and not started.
fn a_stream_its_host_loaded(
    runner: &Runner,
    project: &ProjectWithGraphDocuments,
    stream_name: &str,
) -> Arc<LoadedStreamInThisRuntime> {
    let graph = GraphSnapshot::from_graph_document(the_graph_of_a_stream_named(stream_name))
        .expect("the graph reads");
    runner
        .load_stream_from_graph_snapshot(
            &graph,
            OptionsForLoadingOneStream::at_stream_location(
                project.stream_id_of(stream_name),
                project.location_of(stream_name),
            ),
        )
        .expect("the host loads its stream")
}

fn refusal_of<T: std::fmt::Debug>(outcome: Result<T>) -> String {
    match outcome {
        Ok(unexpected) => panic!("the action was refused, and it returned {unexpected:?}"),
        Err(refusal) => refusal.to_string(),
    }
}

/// The one stream `run` took in, which runs, with its load's tag and its
/// node count.
fn the_one_running_stream_of(
    run: &StreamRunOutcome,
) -> (&StreamLoadedOrFailedByARun, LoadedStreamTag, usize) {
    match run.streams.as_slice() {
        [
            stream @ StreamLoadedOrFailedByARun {
                state:
                    StreamStateAfterItsRun::Running {
                        stream_tag,
                        node_count,
                    },
                ..
            },
        ] => (stream, *stream_tag, *node_count),
        streams => panic!("the run took in one running stream, and took in {streams:?}"),
    }
}

/// The one stream `run` took in, which is failed, and why.
fn the_one_failed_stream_of(run: &StreamRunOutcome) -> (&StreamLoadedOrFailedByARun, &str) {
    match run.streams.as_slice() {
        [
            stream @ StreamLoadedOrFailedByARun {
                state: StreamStateAfterItsRun::Failed { failed_because },
                ..
            },
        ] => (stream, failed_because),
        streams => panic!("the run took in one failed stream, and took in {streams:?}"),
    }
}

/// Makes `streams_directory` refuse every new file until it drops, so a
/// record write fails.
struct StreamsDirectoryRefusingWrites<'a> {
    streams_directory: &'a Path,
}

impl<'a> StreamsDirectoryRefusingWrites<'a> {
    fn from_now_on(streams_directory: &'a Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(streams_directory, std::fs::Permissions::from_mode(0o500))
            .expect("the streams directory is made read-only");
        Self { streams_directory }
    }
}

impl Drop for StreamsDirectoryRefusingWrites<'_> {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            self.streams_directory,
            std::fs::Permissions::from_mode(0o700),
        );
    }
}

/// A previous run's crash pinned on each of `streams`, `SIGSEGV` on each.
fn a_crash_pinned_on(streams: &[&str]) -> HowThePreviousRuntimeRunEnded {
    HowThePreviousRuntimeRunEnded::Crashed(CrashOfThePreviousRuntimeRun {
        causes_by_implicated_stream: streams
            .iter()
            .map(|stream| (stream.to_string(), "SIGSEGV".to_string()))
            .collect(),
        causes_on_threads_no_stream_owns: Vec::new(),
    })
}

/// A stub of the runtime unit's uv, outside every project, that counts its
/// runs.
struct StubRuntimeUnitUv {
    uv: PathBuf,
    uv_runs: PathBuf,
    directory: tempfile::TempDir,
}

impl StubRuntimeUnitUv {
    fn running(body: &str) -> Self {
        let directory = a_temporary_directory_at_owner_only_mode().expect("the uv's directory");
        let uv = directory.path().join("uv");
        let uv_runs = directory.path().join("uv-runs");
        write_an_executable_script_from_a_child_process(
            &uv,
            &format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\n{body}\n",
                uv_runs.display()
            ),
        );
        Self {
            uv,
            uv_runs,
            directory,
        }
    }

    /// A uv whose sync puts back `project`'s `.venv`, which is moved aside
    /// here: the project is left with no venv for the runtime to make.
    fn making_the_venv_moved_aside_from(project: &ProjectWithGraphDocuments) -> Self {
        let stub_uv = Self::running("");
        let venv_uv_makes = stub_uv.directory.path().join("venv-uv-makes");
        std::fs::rename(project.path().join(".venv"), &venv_uv_makes)
            .expect("the venv is moved aside");
        write_an_executable_script_from_a_child_process(
            &stub_uv.uv,
            &format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\ncp -R '{}' .venv\n",
                stub_uv.uv_runs.display(),
                venv_uv_makes.display()
            ),
        );
        stub_uv
    }

    /// A uv whose sync half-makes a `.venv`, then fails writing
    /// `standard_error`.
    fn failing_writing(standard_error: &str) -> Self {
        Self::running(&format!(
            "mkdir -p .venv/bin\necho '{standard_error}' >&2\nexit 1"
        ))
    }

    /// The runtime handed this uv.
    fn handed_to(&self, runner: &Runner) {
        runner
            .set_runtime_unit_uv_executable(self.uv.clone())
            .expect("the runtime is handed its uv");
    }

    /// Each run, by its arguments.
    fn runs(&self) -> Vec<String> {
        std::fs::read_to_string(&self.uv_runs)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

const UV_RESOLUTION_FAILURE: &str =
    "error: No solution found when resolving dependencies: tatolab-stream==99 is unavailable";

/// A uv whose sync waits for its gate file before it puts back `project`'s
/// `.venv`, moved aside here, and the gate that releases it.
fn a_uv_making_the_venv_once_released(
    project: &ProjectWithGraphDocuments,
) -> (StubRuntimeUnitUv, PathBuf) {
    let stub_uv = StubRuntimeUnitUv::running("");
    let venv_uv_makes = stub_uv.directory.path().join("venv-uv-makes");
    let gate = stub_uv.directory.path().join("released");
    std::fs::rename(project.path().join(".venv"), &venv_uv_makes).expect("the venv is moved aside");
    write_an_executable_script_from_a_child_process(
        &stub_uv.uv,
        &format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\nwhile [ ! -e '{}' ]; do sleep 0.02; done\ncp -R \
             '{}' .venv\n",
            stub_uv.uv_runs.display(),
            gate.display(),
            venv_uv_makes.display()
        ),
    );
    (stub_uv, gate)
}

/// How long a test waits for a stream action to reach what it asserts.
const STREAM_ACTION_TEST_DEADLINE: Duration = Duration::from_secs(30);

/// The stream action `action_id` once `reached` holds of it.
fn the_stream_action_once(
    runner: &Runner,
    action_id: &str,
    reached: impl Fn(&StreamAction) -> bool,
) -> StreamAction {
    let deadline = Instant::now() + STREAM_ACTION_TEST_DEADLINE;
    loop {
        let action = runner
            .describe_stream_action(action_id)
            .expect("the action is held");
        if reached(&action) {
            return action;
        }
        assert!(
            Instant::now() < deadline,
            "the action never got there: {action:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The stream action `action_id` once it has finished.
fn the_stream_action_once_finished(runner: &Runner, action_id: &str) -> StreamAction {
    the_stream_action_once(runner, action_id, |action| action.state.is_finished())
}

fn project_open_or_reload_request_of(project_directory: &Path) -> ProjectOpenOrReloadRequest {
    ProjectOpenOrReloadRequest {
        project_directory: project_directory.to_path_buf(),
    }
}

/// `project`, opened on `runner` and held stopped once its open is done.
fn a_project_opened_on(runner: &Runner, project: &ProjectWithGraphDocuments) {
    let (stub_uv, gate) = a_uv_making_the_venv_once_released(project);
    stub_uv.handed_to(runner);
    std::fs::write(&gate, "").unwrap();
    let queued = runner
        .open_project(project_open_or_reload_request_of(project.path()))
        .expect("an open of a project is queued");
    let opened = the_stream_action_once_finished(runner, &queued.action_id);
    assert_eq!(opened.state, StreamActionState::Done, "{opened:?}");
}

/// Reload the open `project` on `runner` and answer the action once it is done.
fn the_project_reloaded(runner: &Runner, project: &ProjectWithGraphDocuments) -> StreamAction {
    let queued = runner
        .reload_project(project_open_or_reload_request_of(project.path()))
        .expect("a reload of an open project is queued");
    let reloaded = the_stream_action_once_finished(runner, &queued.action_id);
    assert_eq!(reloaded.state, StreamActionState::Done, "{reloaded:?}");
    reloaded
}

/// The pending actions on the record of `stream_id`, as `(action, kind)`.
fn the_pending_stream_actions_on(
    streams_directory: &Path,
    stream_id: &str,
) -> Vec<(String, PendingStreamActionKind)> {
    the_record_of(streams_directory, stream_id)
        .pending_stream_actions
        .into_iter()
        .map(|pending| (pending.action_id, pending.kind))
        .collect()
}

mod graph_documents;
mod node_types_their_describe_refuses;
mod playback;
mod project_lifecycle;
mod project_removal;
mod project_venv;
mod recorded_stream_reload;
mod run_refusals;
mod stream_action_queue;
mod stream_records;
mod streams_held_from_their_documents;
mod views_that_do_not_open;
