// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-internal test fixtures shared across `#[cfg(test)]` modules.
//!
//! The TestMock processor types live here so engine tests can drive
//! graph + compiler code without depending on any external package's
//! processors. The `#[processor]` macro never auto-registers; tests
//! register the mocks explicitly via [`ensure_test_mocks_registered`].
//!
//! [`CapturedTracingWarnings`] is the one warning-capture layer every module
//! asserts a log line against.

use std::fmt::Write;
use std::sync::{Arc, Mutex, Once};

use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

use crate as the_engine_under_test;
use crate::core::processors::PROCESSOR_REGISTRY;

#[path = "../../tests/support/links_between_streams_harness.rs"]
pub(crate) mod links_between_streams_harness;

#[path = "../../tests/support/rig_only_capability_skip.rs"]
pub(crate) mod rig_only_capability_skip;

/// Mock processor with two input ports + two output ports.
#[crate::processor(
    execution = manual,
    input("in1", delivery_profile = "newest"),
    input("in2", delivery_profile = "newest"),
    output("out1"),
    output("out2"),
)]
pub(crate) struct MockProcessor;

impl crate::core::ManualProcessor for MockProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock processor with only output ports.
#[crate::processor(
    execution = manual,
    output("out1"),
    output("out2"),
)]
pub(crate) struct MockOutputOnlyProcessor;

impl crate::core::ManualProcessor for MockOutputOnlyProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock processor with only input ports.
#[crate::processor(
    execution = manual,
    input("in1", delivery_profile = "newest"),
    input("in2", delivery_profile = "newest"),
)]
pub(crate) struct MockInputOnlyProcessor;

impl crate::core::ManualProcessor for MockInputOnlyProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock processor whose one input port reads `ordered`, beside the `newest`
/// ports every other input mock declares.
#[crate::processor(
    execution = manual,
    input("in1", delivery_profile = "ordered"),
)]
pub(crate) struct MockOrderedInputOnlyProcessor;

impl crate::core::ManualProcessor for MockOrderedInputOnlyProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock processor with only input ports, waking on upstream writes rather
/// than driving itself — the one execution mode that consumes the link
/// notifications its listener receives.
#[crate::processor(
    execution = reactive,
    input("in1", delivery_profile = "newest"),
    input("in2", delivery_profile = "newest"),
)]
pub(crate) struct MockReactiveInputOnlyProcessor;

impl crate::core::ReactiveProcessor for MockReactiveInputOnlyProcessor::Processor {
    fn process(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock consumer whose audio input port declares a window contract — the
/// destination shape the read-side windowing stage exists for.
#[crate::processor(
    execution = reactive,
    input(
        "audio",
        delivery_profile = "ordered",
        audio_window(
            sample_rate = 16_000,
            channels = 1,
            dtype = "f32",
            window_size = 512,
            hop = 512
        )
    ),
)]
pub(crate) struct MockWindowedAudioConsumerProcessor;

impl crate::core::ReactiveProcessor for MockWindowedAudioConsumerProcessor::Processor {
    fn process(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock consumer whose audio input port declares the `match_device` sentinel.
/// Nothing resolves it on this rung, which is what makes it a wiring error
/// rather than a default.
#[crate::processor(
    execution = reactive,
    input("audio", delivery_profile = "ordered", audio_window = match_device),
)]
pub(crate) struct MockDeviceMatchedAudioConsumerProcessor;

impl crate::core::ReactiveProcessor for MockDeviceMatchedAudioConsumerProcessor::Processor {
    fn process(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock source declaring its one output port in camelCase, `outOne`, which
/// registers under its cast `outone`.
#[crate::processor(
    execution = manual,
    output("outOne"),
)]
pub(crate) struct MockProcessorWithACamelCaseOutputPort;

impl crate::core::ManualProcessor for MockProcessorWithACamelCaseOutputPort::Processor {
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// The one setting [`MockSourceTakingOneSetting`] takes.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct MockSourceTakingOneSettingConfig {
    #[serde(default)]
    pub frame_width: Option<u32>,
}

/// Mock source whose config takes `frame_width` and refuses every other key.
#[crate::processor(
    execution = manual,
    config = crate::core::test_support::MockSourceTakingOneSettingConfig,
    output("video"),
)]
pub(crate) struct MockSourceTakingOneSetting;

impl crate::core::ManualProcessor for MockSourceTakingOneSetting::Processor {
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Register all engine-internal test mock processors with the global
/// `PROCESSOR_REGISTRY`. Idempotent — safe to call from every test
/// fixture that builds a graph against `lookup_registered_ident` or
/// drives the compiler against a `ProcessorSpec`.
pub(crate) fn ensure_test_mocks_registered() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        PROCESSOR_REGISTRY.register::<MockProcessor::Processor>();
        PROCESSOR_REGISTRY.register::<MockOutputOnlyProcessor::Processor>();
        PROCESSOR_REGISTRY.register::<MockWindowedAudioConsumerProcessor::Processor>();
        PROCESSOR_REGISTRY.register::<MockDeviceMatchedAudioConsumerProcessor::Processor>();
        PROCESSOR_REGISTRY.register::<MockInputOnlyProcessor::Processor>();
        PROCESSOR_REGISTRY.register::<MockOrderedInputOnlyProcessor::Processor>();
        PROCESSOR_REGISTRY.register::<MockReactiveInputOnlyProcessor::Processor>();
        PROCESSOR_REGISTRY.register::<MockProcessorWithACamelCaseOutputPort::Processor>();
        PROCESSOR_REGISTRY.register::<MockSourceTakingOneSetting::Processor>();
    });
}

/// Add a [`MockOutputOnlyProcessor`] node to `graph`; its processor id.
pub(crate) fn add_mock_output_only(graph: &mut crate::core::graph::Graph) -> String {
    add_a_mock_node(
        graph,
        MockOutputOnlyProcessor::processor_class_import_path(),
    )
}

/// Add a [`MockInputOnlyProcessor`] node to `graph`; its processor id.
pub(crate) fn add_mock_input_only(graph: &mut crate::core::graph::Graph) -> String {
    add_a_mock_node(graph, MockInputOnlyProcessor::processor_class_import_path())
}

fn add_a_mock_node(
    graph: &mut crate::core::graph::Graph,
    processor_class_import_path: crate::core::descriptors::ProcessorClassImportPath,
) -> String {
    ensure_test_mocks_registered();
    graph
        .traversal_mut()
        .add_v(crate::core::processors::ProcessorSpec::new(
            processor_class_import_path,
            serde_json::Value::Null,
        ))
        .expect("the node is named")
        .first()
        .expect("the mock is in the registry")
        .id
        .to_string()
}

/// Every `WARN`-level tracing event raised while this is the default
/// subscriber's layer, each rendered as its `field=value` pairs.
///
/// Structured fields are kept rather than only the message, because the port a
/// warning names is usually a field.
#[derive(Clone, Default)]
pub(crate) struct CapturedTracingWarnings(Arc<Mutex<Vec<String>>>);

impl CapturedTracingWarnings {
    /// Run `raising_them` with this installed on the calling thread, handing
    /// back what it returned alongside the warnings it raised.
    pub(crate) fn captured_while<T>(raising_them: impl FnOnce() -> T) -> (T, Vec<String>) {
        let captured = Self::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let returned = tracing::subscriber::with_default(subscriber, raising_them);
        let warnings = captured.0.lock().expect("no test panics holding this lock");
        (returned, warnings.clone())
    }
}

/// Renders one event's fields into `name=value` pairs, so a test can assert on
/// a structured field as readily as on the message.
struct EveryFieldOfOneCapturedWarning<'a>(&'a mut String);

impl EveryFieldOfOneCapturedWarning<'_> {
    fn push_one_field(&mut self, name: &str, rendered: std::fmt::Arguments<'_>) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        let _ = write!(self.0, "{name}={rendered}");
    }
}

impl Visit for EveryFieldOfOneCapturedWarning<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.push_one_field(field.name(), format_args!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.push_one_field(field.name(), format_args!("{value}"));
    }
}

impl<S: tracing::Subscriber> Layer<S> for CapturedTracingWarnings {
    fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
        if *event.metadata().level() != tracing::Level::WARN {
            return;
        }
        let mut rendered = String::new();
        event.record(&mut EveryFieldOfOneCapturedWarning(&mut rendered));
        self.0
            .lock()
            .expect("no test panics holding this lock")
            .push(rendered);
    }
}

/// One port a far side was asked to drop. Named rather than a tuple so a
/// swapped port and link id fails the assert instead of passing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReclaimedLink {
    pub(crate) port_direction: crate::core::PortDirection,
    pub(crate) local_port_name: String,
    pub(crate) link_id: String,
}

/// A far side past its setup command that records every link it is handed and
/// every port it is told to drop, holding each handed link's answer cell for the
/// test to answer.
///
/// Clones share what they record, which is how a test reads what the engine
/// asked of a far side it handed to an envelope.
#[derive(Clone, Default)]
pub(crate) struct RecordingOutOfProcessFarSideLinkDelivery {
    /// Every link handed over, with the direction it was wired in.
    pub(crate) late_wired_links:
        Arc<parking_lot::Mutex<Vec<(crate::core::PortDirection, serde_json::Value)>>>,
    /// The answer cells handed over, in the order their links were — how a
    /// test plays a far side that has not answered yet, opened its port, or
    /// refused.
    pub(crate) wire_answers_owed:
        Arc<parking_lot::Mutex<Vec<Arc<crate::core::processors::OutOfProcessLinkWireReply>>>>,
    /// Every port the far side was told to drop.
    pub(crate) reclaimed_links: Arc<parking_lot::Mutex<Vec<ReclaimedLink>>>,
}

impl crate::core::processors::OutOfProcessFarSideLinkDelivery
    for RecordingOutOfProcessFarSideLinkDelivery
{
    fn hand_over_a_link_wired_after_setup(
        &self,
        port_direction: crate::core::PortDirection,
        link_wiring: &serde_json::Value,
        answer_cell: Arc<crate::core::processors::OutOfProcessLinkWireReply>,
    ) -> crate::core::Result<()> {
        self.late_wired_links
            .lock()
            .push((port_direction, link_wiring.clone()));
        self.wire_answers_owed.lock().push(answer_cell);
        Ok(())
    }

    fn tell_the_far_side_a_link_was_unwired(
        &self,
        port_direction: crate::core::PortDirection,
        local_port_name: &str,
        link_id: &str,
    ) -> crate::core::Result<()> {
        self.reclaimed_links.lock().push(ReclaimedLink {
            port_direction,
            local_port_name: local_port_name.to_string(),
            link_id: link_id.to_string(),
        });
        Ok(())
    }

    fn refuse_every_link_still_awaiting_the_far_sides_answer(&self, reason: &str) {
        for answer_cell in self.wire_answers_owed.lock().iter() {
            answer_cell.note_the_far_sides_answer(
                crate::core::processors::OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
                    reason: reason.to_string(),
                },
            );
        }
    }
}

/// A `sleep` parked in a process group of its own, standing in for a helper
/// process.
pub(crate) fn a_process_parked_in_a_process_group_of_its_own() -> std::process::Child {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new("sleep");
    command
        .arg("120")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: `setpgid` is async-signal-safe, the contract for `pre_exec`.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().expect("a parked process starts")
}

/// Whether every member of `process_group_id` is gone inside `budget`.
///
/// Signal 0 rather than a wait: a group's members are not necessarily this
/// process's children.
pub(crate) fn a_process_group_is_gone_within(
    process_group_id: libc::pid_t,
    budget: std::time::Duration,
) -> bool {
    let deadline = std::time::Instant::now() + budget;
    // SAFETY: signal 0 delivers nothing; it only asks whether a member is left.
    while unsafe { libc::killpg(process_group_id, 0) } == 0 {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    true
}

/// Re-run the test at `test_path` in a child process of this test binary with
/// `environment_variable` set to `value`, and wait for it — for a test whose
/// passing ends the process it runs in.
pub(crate) fn rerun_this_test_in_a_child_process(
    test_path: &str,
    environment_variable: &str,
    value: &std::ffi::OsStr,
) -> std::process::Output {
    rerun_this_test_in_a_child_process_with_environment(test_path, &[(environment_variable, value)])
}

/// [`rerun_this_test_in_a_child_process`] with every variable in
/// `environment` set in the child.
pub(crate) fn rerun_this_test_in_a_child_process_with_environment(
    test_path: &str,
    environment: &[(&str, &std::ffi::OsStr)],
) -> std::process::Output {
    let child_process_output = crate::iceoryx2::spawn_outside_every_iceoryx2_listener_bind(
        std::process::Command::new(std::env::current_exe().expect("the test binary's own path"))
            .args([test_path, "--exact", "--test-threads=1", "--nocapture"])
            .envs(environment.iter().copied())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .and_then(std::process::Child::wait_with_output)
    .expect("the test binary re-runs this test in a child process");
    // `--exact` on a name that matches nothing runs no test and exits 0, which
    // reads as a pass for a test that was renamed away.
    let child_standard_output = String::from_utf8_lossy(&child_process_output.stdout);
    assert!(
        child_standard_output.contains("running 1 test"),
        "the child process ran no test named `{test_path}`:\n{child_standard_output}"
    );
    child_process_output
}

/// Write `script` to `path` as an executable, from a short-lived shell.
///
/// A file this process writes and then execs can fail with ETXTBSY: another
/// test's thread that forks in between hands its child the still-open write
/// descriptor until that child execs. Written from a child process, the
/// descriptor never enters this process's table.
pub(crate) fn write_an_executable_script_from_a_child_process(
    path: &std::path::Path,
    script: &str,
) {
    use std::io::Write;
    let mut script_writer = std::process::Command::new("/bin/sh")
        .args(["-c", r#"cat > "$1" && chmod 755 "$1""#, "sh"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("a shell writes the script");
    script_writer
        .stdin
        .take()
        .expect("the shell's standard input is piped")
        .write_all(script.as_bytes())
        .expect("the script reaches the shell");
    let status = script_writer.wait().expect("the shell exits");
    assert!(
        status.success(),
        "the shell could not write `{}`",
        path.display()
    );
}

/// A temporary directory at exactly owner-only mode, even when it was made
/// while another test's thread was binding an iceoryx2 listener under its
/// process-wide umask.
pub(crate) fn a_temporary_directory_at_owner_only_mode() -> std::io::Result<tempfile::TempDir> {
    at_owner_only_mode(tempfile::tempdir()?)
}

/// Give `temporary_directory` exactly owner-only mode, whatever umask it was
/// made under.
pub(crate) fn at_owner_only_mode(
    temporary_directory: tempfile::TempDir,
) -> std::io::Result<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        temporary_directory.path(),
        std::fs::Permissions::from_mode(
            streamlib_runtime_client_contract::directory_at_an_explicit_mode::OWNER_ONLY_DIRECTORY_MODE,
        ),
    )?;
    Ok(temporary_directory)
}

/// A live surface-share service wired into a GPU context as the engine's own
/// store, so what the engine registers reaches a real service.
#[cfg(target_os = "linux")]
pub(crate) struct LiveSurfaceShareServiceForATest {
    pub(crate) state: crate::linux::surface_share::SurfaceShareState,
    service: crate::linux::surface_share::UnixSocketSurfaceService,
    _socket_dir: tempfile::TempDir,
}

#[cfg(target_os = "linux")]
impl LiveSurfaceShareServiceForATest {
    /// A started service, its store installed as `gpu`'s engine store.
    pub(crate) fn wired_into(gpu: &crate::core::context::GpuContext) -> Self {
        use std::sync::Arc;

        use crate::core::context::SurfaceStore;
        use crate::linux::surface_share::{SurfaceShareState, UnixSocketSurfaceService};

        let socket_dir =
            a_temporary_directory_at_owner_only_mode().expect("temp dir for the test socket");
        let socket_path = socket_dir.path().join("surface-share.sock");
        let state = SurfaceShareState::new();
        let mut service = UnixSocketSurfaceService::new(state.clone(), socket_path.clone());
        service.start().expect("service start");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !socket_path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let store = SurfaceStore::new_reading_check_out_leases(
            socket_path.to_string_lossy().into_owned(),
            "R-live-surface-share-service-for-a-test".to_string(),
            Arc::clone(state.check_out_leases()),
        );
        store.connect().expect("the store connects");
        gpu.install_the_engines_surface_store(store)
            .expect("the engine's surface store installs once");
        Self {
            state,
            service,
            _socket_dir: socket_dir,
        }
    }

    /// The service's lease table.
    pub(crate) fn check_out_leases(&self) -> &crate::core::context::SurfaceCheckOutLeaseRegistry {
        self.state.check_out_leases()
    }

    /// Whether the service holds a registration under `surface_id`.
    pub(crate) fn is_registered(&self, surface_id: &str) -> bool {
        self.state.get_surface_planes(surface_id).is_some()
    }

    /// Stop the service.
    pub(crate) fn stop(mut self) {
        self.service.stop();
    }
}

#[cfg(target_os = "macos")]
pub(crate) struct LiveSurfaceShareServiceForATest {
    pub(crate) state: crate::apple::surface_share::IOSurfaceShareState,
    service: crate::apple::surface_share::MachSurfaceShareService,
}

#[cfg(target_os = "macos")]
impl LiveSurfaceShareServiceForATest {
    /// A started service, its store installed as `gpu`'s engine store.
    pub(crate) fn wired_into(gpu: &crate::core::context::GpuContext) -> Self {
        use std::sync::Arc;

        use crate::apple::surface_share::{IOSurfaceShareState, MachSurfaceShareService};
        use crate::core::context::SurfaceStore;

        let state = IOSurfaceShareState::new();
        let mut service = MachSurfaceShareService::new(
            state.clone(),
            format!(
                "com.tatolab.streamlib.escalate-tensor-test.{}.{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ),
        );
        service
            .start()
            .expect("the Mach surface-share service starts");
        let store = SurfaceStore::new_sharing_the_mach_services_tables(
            service.service_name().to_string(),
            "R-live-surface-share-service-for-a-test".to_string(),
            Arc::clone(state.check_out_leases()),
            Arc::clone(state.cross_process_timeline_pairs()),
        );
        store.connect().expect("the store connects");
        gpu.install_the_engines_surface_store(store)
            .expect("the engine's surface store installs once");
        Self { state, service }
    }

    /// The service's lease table.
    pub(crate) fn check_out_leases(&self) -> &crate::core::context::SurfaceCheckOutLeaseRegistry {
        self.state.check_out_leases()
    }

    /// Whether the service holds a registration under `surface_id`.
    pub(crate) fn is_registered(&self, surface_id: &str) -> bool {
        self.state.registration_of(surface_id).is_some()
    }

    /// Stop the service.
    pub(crate) fn stop(mut self) {
        self.service.stop();
    }
}

/// Block until every GPU submission `gpu` made so far has completed, so a
/// freeing decision in a test turns on holders and time alone.
pub(crate) fn wait_for_every_gpu_submission_made_so_far(gpu: &crate::core::context::GpuContext) {
    let made_so_far = gpu.device().snapshot_of_queue_submissions_made_so_far();
    assert!(
        gpu.device()
            .wait_for_every_queue_submission_in_snapshot(
                &made_so_far,
                std::time::Duration::from_secs(5)
            )
            .expect("the GPU's queue completion reads"),
        "the GPU work made before the test did not complete"
    );
}

#[cfg(test)]
mod rig_only_capability_skip_closed_list_tests {
    use super::rig_only_capability_skip::{
        HARDWARE_TEST_SKIP_LINE_PREFIX, RigOnlyCapability, skip_unless_this_machine_has,
    };
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn repository_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn every_closed_list_reason_the_helper_names() -> BTreeSet<String> {
        RigOnlyCapability::EVERY_RIG_ONLY_CAPABILITY
            .iter()
            .map(|capability| capability.closed_list_reason().to_owned())
            .collect()
    }

    /// The quoted strings of the Python set literal assigned to `set_name`.
    fn python_set_literal_strings(python_source: &str, set_name: &str) -> BTreeSet<String> {
        let after_name = python_source
            .split_once(&format!("{set_name} = {{"))
            .unwrap_or_else(|| panic!("{set_name} is assigned a set literal"))
            .1;
        let set_body = after_name
            .split_once('}')
            .expect("the set literal closes")
            .0;
        set_body
            .lines()
            .filter_map(|line| line.trim().strip_prefix('"'))
            .filter_map(|line| line.split_once('"').map(|(reason, _)| reason.to_owned()))
            .collect()
    }

    /// The value the workflow gives `variable_name`, from its `name: "value"` line.
    fn workflow_environment_value(workflow_source: &str, variable_name: &str) -> String {
        workflow_source
            .lines()
            .find_map(|line| line.trim().strip_prefix(&format!("{variable_name}: ")))
            .unwrap_or_else(|| panic!("the workflow sets {variable_name}"))
            .trim_matches('"')
            .to_owned()
    }

    #[test]
    fn the_helpers_reasons_are_the_python_rig_only_closed_list() {
        let platform_markers_test = std::fs::read_to_string(
            repository_root().join("sdk/tatolab-stream/tests/test_platform_markers.py"),
        )
        .expect("the stream suite's platform-marker test reads");
        assert_eq!(
            every_closed_list_reason_the_helper_names(),
            python_set_literal_strings(
                &platform_markers_test,
                "RIG_ONLY_CAPABILITY_CLOSED_LIST_REASONS"
            ),
        );
    }

    #[test]
    fn the_lanes_skip_line_check_accepts_exactly_the_helpers_line_and_reasons() {
        let workflow =
            std::fs::read_to_string(repository_root().join(".github/workflows/python-wheel.yml"))
                .expect("the wheel workflow reads");
        assert_eq!(
            workflow_environment_value(&workflow, "HARDWARE_TEST_SKIP_LINE_PREFIX"),
            HARDWARE_TEST_SKIP_LINE_PREFIX,
        );
        let reasons_the_lane_accepts: BTreeSet<String> =
            workflow_environment_value(&workflow, "HARDWARE_TEST_SKIP_LINE_REASONS")
                .split('|')
                .map(str::to_owned)
                .collect();
        assert_eq!(
            reasons_the_lane_accepts,
            every_closed_list_reason_the_helper_names()
        );
    }

    #[test]
    fn a_capability_this_machine_has_is_not_skipped() {
        println!("Skipping — a deliberately unnamed skip the lane must refuse");
        assert!(!skip_unless_this_machine_has(
            RigOnlyCapability::Camera,
            true
        ));
    }
}
