// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::fmt::Write;
use std::sync::{Arc, Mutex, Once};

use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

use crate::core::nodes::NODE_REGISTRY;

mod a_setting_read_at_setup_for_a_test;
mod recording_far_side_link_delivery;
pub(crate) use a_setting_read_at_setup_for_a_test::make_in1_a_setting_read_at_setup;
pub(crate) use recording_far_side_link_delivery::{
    ReclaimedLink, RecordingOutOfProcessFarSideLinkDelivery,
};

#[path = "../../../tests/support/rig_only_capability_skip.rs"]
pub(crate) mod rig_only_capability_skip;

#[crate::node(
    execution = manual,
    input("in1", delivery_profile = "newest", pin_type = String, flow = Stream),
    input("in2", delivery_profile = "newest", pin_type = String, flow = Stream),
    output("out1", pin_type = String, flow = Stream),
    output("out2", pin_type = String, flow = Stream),
)]
pub(crate) struct MockNode;

impl crate::core::ManualNode for MockNode::Node {
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

#[crate::node(
    execution = manual,
    output("out1", pin_type = String, flow = Stream),
    output("out2", pin_type = String, flow = Stream),
)]
pub(crate) struct MockOutputOnlyNode;

impl crate::core::ManualNode for MockOutputOnlyNode::Node {
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

/// Mock source with one Channels output, written to by a test.
#[crate::node(execution = manual, output("audio", pin_type = Audio))]
pub(crate) struct MockChannelsSourceNode;

impl crate::core::ManualNode for MockChannelsSourceNode::Node {
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

#[crate::node(
    execution = manual,
    input("in1", delivery_profile = "newest", pin_type = String, flow = Stream),
    input("in2", delivery_profile = "newest", pin_type = String, flow = Stream),
)]
pub(crate) struct MockInputOnlyNode;

impl crate::core::ManualNode for MockInputOnlyNode::Node {
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

/// A node reading the Trigger input `in1`, which reads by its pin's default delivery.
#[crate::node(
    execution = manual,
    input("in1", pin_type = Trigger),
)]
pub(crate) struct MockTriggerReaderNode;

impl crate::core::ManualNode for MockTriggerReaderNode::Node {
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

#[crate::node(
    execution = manual,
    input("in1", delivery_profile = "ordered", pin_type = String, flow = Stream),
)]
pub(crate) struct MockOrderedInputOnlyNode;

impl crate::core::ManualNode for MockOrderedInputOnlyNode::Node {
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

#[crate::node(
    execution = reactive,
    input("in1", delivery_profile = "newest", pin_type = String, flow = Stream),
    input("in2", delivery_profile = "newest", pin_type = String, flow = Stream),
)]
pub(crate) struct MockReactiveInputOnlyNode;

impl crate::core::ReactiveNode for MockReactiveInputOnlyNode::Node {
    fn process(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock consumer whose audio input port declares a window contract — the
/// destination shape the read-side windowing stage exists for.
#[crate::node(
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
        ),
        pin_type = Audio,
    ),
)]
pub(crate) struct MockWindowedAudioConsumerNode;

impl crate::core::ReactiveNode for MockWindowedAudioConsumerNode::Node {
    fn process(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock consumer whose input port declares a borrow quota past the default.
#[crate::node(
    execution = reactive,
    input("frames", delivery_profile = "ordered", borrow_quota = 5, pin_type = Texture),
)]
pub(crate) struct MockBorrowingConsumerNode;

impl crate::core::ReactiveNode for MockBorrowingConsumerNode::Node {
    fn process(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock node whose ports are declared by pin type rather than by family.
#[crate::node(
    execution = reactive,
    input("gain", delivery_profile = "newest", pin_type = Float),
    output("frames", pin_type = Texture, formats = ["rgba8_unorm"]),
)]
pub(crate) struct MockPinTypedNode;

impl crate::core::ReactiveNode for MockPinTypedNode::Node {
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
#[crate::node(
    execution = reactive,
    input("audio", delivery_profile = "ordered", audio_window = match_device, pin_type = Audio),
)]
pub(crate) struct MockDeviceMatchedAudioConsumerNode;

impl crate::core::ReactiveNode for MockDeviceMatchedAudioConsumerNode::Node {
    fn process(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock source declaring its one output port in camelCase, `outOne`, which
/// registers under its cast `outone`.
#[crate::node(
    execution = manual,
    output("outOne", pin_type = String, flow = Stream),
)]
pub(crate) struct MockNodeWithACamelCaseOutputPort;

impl crate::core::ManualNode for MockNodeWithACamelCaseOutputPort::Node {
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
pub(crate) struct MockSourceTakingOneSettingConfig {
    #[serde(default)]
    pub frame_width: Option<u32>,
}

/// Mock source whose config takes `frame_width` and refuses every other key.
#[crate::node(
    execution = manual,
    output("video", pin_type = Texture, formats = ["rgba8_unorm"]),
)]
pub(crate) struct MockSourceTakingOneSetting;

impl crate::core::ManualNode for MockSourceTakingOneSetting::Node {
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// The two settings [`MockSourceTakingAFrameSize`] takes.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub(crate) struct MockSourceTakingAFrameSizeConfig {
    #[serde(default)]
    pub frame_width: Option<u32>,
    #[serde(default)]
    pub frame_height: Option<u32>,
}

/// Mock source whose config takes `frame_width` and `frame_height`.
#[crate::node(
    execution = manual,
    output("video", pin_type = Texture, formats = ["rgba8_unorm"]),
)]
pub(crate) struct MockSourceTakingAFrameSize;

impl crate::core::ManualNode for MockSourceTakingAFrameSize::Node {
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// The one setting [`MockSourceReadingAnImageAsset`] takes: the image, by asset reference.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub(crate) struct MockSourceReadingAnImageAssetConfig {
    #[serde(default)]
    pub image: Option<serde_json::Value>,
}

/// Mock source whose config names an image asset `{"asset": "<uuid>"}`.
#[crate::node(
    execution = manual,
    output("video", pin_type = Texture, formats = ["rgba8_unorm"]),
)]
pub(crate) struct MockSourceReadingAnImageAsset;

impl crate::core::ManualNode for MockSourceReadingAnImageAsset::Node {
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Mock source whose `setup()` refuses, as a capture device does, when its
/// stream may not ask the user for an access nobody has answered.
#[crate::node(
    execution = manual,
    output("out1", pin_type = String, flow = Stream),
)]
pub(crate) struct MockSourceRefusingAnAccessNobodyAskedFor;

/// What [`MockSourceRefusingAnAccessNobodyAskedFor`] refuses with.
pub(crate) const MOCK_REFUSAL_OF_AN_ACCESS_NOBODY_ASKED_FOR: &str =
    "mock camera access has not been asked for yet: restart this stream to be asked";

impl crate::core::ManualNode for MockSourceRefusingAnAccessNobodyAskedFor::Node {
    fn setup(
        &mut self,
        ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        match ctx.capture_device_access_prompting() {
            crate::core::context::CaptureDeviceAccessPrompting::MayAskTheUser => Ok(()),
            crate::core::context::CaptureDeviceAccessPrompting::RefuseWhatWasNeverAskedFor => Err(
                crate::core::error::Error::CaptureDeviceAccessNeverAskedFor {
                    device: crate::core::error::PrivacyGatedCaptureDevice::Camera,
                    refusal: MOCK_REFUSAL_OF_AN_ACCESS_NOBODY_ASKED_FOR.to_string(),
                },
            ),
        }
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

pub(crate) fn ensure_test_mocks_registered() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        NODE_REGISTRY.register::<MockNode::Node>();
        NODE_REGISTRY.register::<MockOutputOnlyNode::Node>();
        NODE_REGISTRY.register::<MockChannelsSourceNode::Node>();
        NODE_REGISTRY.register::<MockWindowedAudioConsumerNode::Node>();
        NODE_REGISTRY.register::<MockDeviceMatchedAudioConsumerNode::Node>();
        NODE_REGISTRY.register::<MockBorrowingConsumerNode::Node>();
        NODE_REGISTRY.register::<MockPinTypedNode::Node>();
        NODE_REGISTRY.register::<MockInputOnlyNode::Node>();
        NODE_REGISTRY.register::<MockOrderedInputOnlyNode::Node>();
        NODE_REGISTRY.register::<MockReactiveInputOnlyNode::Node>();
        NODE_REGISTRY.register::<MockNodeWithACamelCaseOutputPort::Node>();
        NODE_REGISTRY.register_taking_a_config_for_a_test::<MockSourceTakingOneSetting::Node, MockSourceTakingOneSettingConfig>();
        NODE_REGISTRY.register_taking_a_config_for_a_test::<MockSourceTakingAFrameSize::Node, MockSourceTakingAFrameSizeConfig>();
        NODE_REGISTRY.register_taking_a_config_for_a_test::<MockSourceReadingAnImageAsset::Node, MockSourceReadingAnImageAssetConfig>();
        NODE_REGISTRY.register::<MockSourceRefusingAnAccessNobodyAskedFor::Node>();
    });
}

pub(crate) fn add_mock_output_only(graph: &mut crate::core::graph::Graph) -> String {
    add_a_mock_node(graph, MockOutputOnlyNode::node_class_import_path())
}

pub(crate) fn add_mock_input_only(graph: &mut crate::core::graph::Graph) -> String {
    add_a_mock_node(graph, MockInputOnlyNode::node_class_import_path())
}

fn add_a_mock_node(
    graph: &mut crate::core::graph::Graph,
    node_class_import_path: crate::core::descriptors::NodeClassImportPath,
) -> String {
    ensure_test_mocks_registered();
    graph
        .traversal_mut()
        .add_v(crate::core::nodes::NodeSpec::new(
            node_class_import_path,
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

/// A view of a project, of the type `view_type`, reading records on `in1`, as the project's
/// `views/` folder would bring it: a node type that runs in the app process and that the
/// stream's function node pool does not evaluate.
pub(crate) fn a_view_of_a_project_reading_records(
    view_type: &str,
) -> streamlib_project_repository::ProjectViewFolderRead {
    let manifest = a_view_manifest_reading_records();
    streamlib_project_repository::ProjectViewFolderRead {
        view_type: view_type.to_owned(),
        brought_by: streamlib_project_repository::ProjectBringingAView {
            project: uuid::Uuid::nil(),
            project_name: "a-project-bringing-a-view".to_owned(),
            import_folder: None,
        },
        folder: std::path::PathBuf::from("/a-project-bringing-a-view/views/program"),
        manifest: streamlib_project_repository::read_tatolab_view_manifest(&manifest),
        manifest_as_written: Some(manifest),
    }
}

/// Write into `project_directory`'s `views/` the view `view_folder_name`, of `manifest`.
pub(crate) fn a_view_in(
    project_directory: &std::path::Path,
    view_folder_name: &str,
    manifest: &serde_json::Value,
) {
    let folder = project_directory
        .join(streamlib_project_repository::PROJECT_VIEWS_DIRECTORY)
        .join(view_folder_name);
    streamlib_runtime_client_contract::directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode(
        &folder,
        streamlib_runtime_client_contract::directory_at_an_explicit_mode::OWNER_ONLY_DIRECTORY_MODE,
    )
    .expect("the view's folder");
    std::fs::write(
        folder.join(streamlib_project_repository::TATOLAB_VIEW_MANIFEST_FILE_NAME),
        serde_json::to_vec(manifest).expect("the manifest serializes"),
    )
    .expect("the view's manifest");
}

/// Write into `project_directory`'s `views/` the view `view_folder_name`, reading records on
/// `in1`, which the project then brings as a node type that runs in the app process.
pub(crate) fn a_view_reading_records_in(
    project_directory: &std::path::Path,
    view_folder_name: &str,
) {
    a_view_in(
        project_directory,
        view_folder_name,
        &a_view_manifest_reading_records(),
    );
}

/// The phrase the refusal of a view whose port is declared by the older `family` field holds.
pub(crate) const THE_REFUSAL_OF_A_PORT_DECLARED_BY_FAMILY: &str = "older `family` field";

/// Write into `project_directory`'s `views/` the view `view_folder_name`, whose `video` input is
/// still declared by the older `family` field, so it does not open.
pub(crate) fn a_view_that_does_not_open_in(
    project_directory: &std::path::Path,
    view_folder_name: &str,
) {
    a_view_in(
        project_directory,
        view_folder_name,
        &serde_json::json!({
            "contract": "1.1", "title": "Stage", "entry": "index.html", "placement": "node",
            "inputs": [{ "name": "video", "family": "image" }]
        }),
    );
}

fn a_view_manifest_reading_records() -> serde_json::Value {
    serde_json::json!({
        "contract": "1.1", "title": "Program", "description": "What plays out.",
        "entry": "index.html", "placement": "node",
        "inputs": [{ "name": "in1", "pin_type": { "type": "string" }, "description": "What it shows." }],
        "outputs": []
    })
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
        let workflow = std::fs::read_to_string(repository_root().join(".github/workflows/ci.yml"))
            .expect("the CI workflow reads");
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
        assert!(!skip_unless_this_machine_has(
            RigOnlyCapability::Camera,
            true
        ));
    }
}
