// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::tests::unique_service_name;
use super::*;
use crate::iceoryx2::channel_sample_test_support::TestChannelSample;
use streamlib_runtime_client_contract::streamlib_runtime_directory::current_process_uid;

fn names_of_the_live_nodes_in(config: &Config) -> Vec<String> {
    use iceoryx2::node::NodeView;

    let mut names = Vec::new();
    Node::<ipc_threadsafe::Service>::list(config, |node_state| {
        if let NodeState::Alive(view) = node_state
            && let Some(details) = view.details()
        {
            names.push(details.name().as_str().to_string());
        }
        CallbackProgression::Continue
    })
    .expect("the domain's nodes can be listed with its own config");
    names
}

#[test]
fn a_domain_root_past_the_socket_path_budget_is_refused_by_name() {
    let prefix = engine_owned_iceoryx2_prefix_for_this_user();
    let root_at_the_budget = format!(
        "/{}",
        "r".repeat(ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES - prefix.len() - 1)
    );
    let root_one_byte_past_it = format!("{root_at_the_budget}r");

    engine_owned_iceoryx2_config(std::path::Path::new(&root_at_the_budget))
        .expect("a root exactly at the budget is accepted");
    let refusal = match create_iceoryx2_node_in_engine_owned_domain(
        std::path::Path::new(&root_one_byte_past_it),
        "streamlib-test",
    ) {
        Ok(_) => panic!("a root one byte past the budget must be refused before any node"),
        Err(refusal) => refusal.to_string(),
    };

    assert!(refusal.contains(&root_one_byte_past_it), "{refusal}");
    assert!(
        refusal.contains(&format!(
            "{ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES}-byte budget"
        )),
        "{refusal}"
    );
    assert!(
        !std::path::Path::new(&root_one_byte_past_it).exists(),
        "a refused root must never be created"
    );
}

/// A temporary directory short enough that a domain root under it still
/// fits the socket-path budget.
///
/// macOS resolves `TMPDIR` to a per-session `/var/folders/…` path of
/// roughly fifty bytes, which spends the whole budget before a domain name
/// is appended. `/tmp` is where the runtime directory resolves there
/// anyway, so a test domain built under it matches what a real run gets.
fn domain_root_parent_within_the_socket_path_budget() -> tempfile::TempDir {
    #[cfg(target_os = "macos")]
    return crate::core::test_support::at_owner_only_mode(
        tempfile::Builder::new()
            .tempdir_in("/tmp")
            .expect("a temp directory under /tmp"),
    )
    .expect("a temp directory under /tmp");
    #[cfg(not(target_os = "macos"))]
    return crate::core::test_support::a_temporary_directory_at_owner_only_mode()
        .expect("a temp directory");
}

/// Set only in the child process the dead-node test re-runs itself in.
const DEAD_NODE_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_TEST_DEAD_NODE_CHILD_ICEORYX2_DOMAIN_ROOT";

/// A node is dead only once the process that opened it is gone, so the node
/// is opened in a child test process that is then killed where it stands —
/// the same self-re-run shape the working-directory test uses.
#[test]
fn a_node_whose_process_was_killed_is_reclaimed_by_the_sweep() {
    if let Some(domain_root) = std::env::var_os(DEAD_NODE_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE) {
        let _node = Iceoryx2Node::new(
            std::path::Path::new(&domain_root),
            "streamlib-test/killed-where-it-stood",
        )
        .expect("a node opens in the engine-owned domain");
        // SAFETY: this process signalling itself, which is what leaves the
        // node registered with no process behind it.
        unsafe { libc::kill(std::process::id() as libc::pid_t, libc::SIGKILL) };
        unreachable!("SIGKILL to self does not return");
    }

    let domain = domain_root_parent_within_the_socket_path_budget();
    let domain_root = domain.path().join("iox2");

    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "iceoryx2::node::domain_and_reclaim_tests::a_node_whose_process_was_killed_is_reclaimed_by_the_sweep",
            "--exact",
            "--test-threads=1",
        ])
        .env(
            DEAD_NODE_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE,
            &domain_root,
        )
        .output()
        .expect("the test binary re-runs this test in a child process");
    assert!(
        !child.status.success(),
        "the child must die where it stood rather than report a result"
    );

    // Swept until it lands rather than once: a process that has just died is
    // not reclaimable the instant it goes — about 10 ms on Linux 7.0, wider
    // under load — which is the same window `open_or_create_service` budgets
    // for. Asserting on one sweep asserts the race, not the reclaim.
    let mut reclaimed = 0u64;
    let started_sweeping = std::time::Instant::now();
    while reclaimed == 0 && started_sweeping.elapsed() < std::time::Duration::from_secs(5) {
        reclaimed += reclaim_dead_iceoryx2_nodes_in_engine_owned_domain(&domain_root)
            .expect("the sweep runs against the engine-owned domain");
        if reclaimed == 0 {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    assert_eq!(
        reclaimed, 1,
        "the killed child's node was left registered with no process behind it"
    );
    assert_eq!(
        reclaim_dead_iceoryx2_nodes_in_engine_owned_domain(&domain_root).unwrap(),
        0,
        "a swept domain has nothing left to reclaim"
    );
}

/// Set only in the child process the stale-holder test re-runs itself in.
const STALE_HOLDER_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_TEST_STALE_HOLDER_CHILD_ICEORYX2_DOMAIN_ROOT";

/// The channel the stale holder leaves behind, shared by both processes.
const STALE_HOLDER_CHANNEL_SERVICE_NAME: &str = "test/stale-holder/frames_to_downstream";

/// The depth the holder creates the channel at, and the deeper one a later
/// destination asks for — `newest`'s old depth against `ordered`'s.
const STALE_HOLDER_SHALLOW_DEPTH: usize = 4;
const STALE_HOLDER_DEEPER_DEPTH: usize = 16;
const STALE_HOLDER_MAX_SUBSCRIBERS: usize = 4;

/// Re-run the stale-holder test as a holder process that creates the
/// shallow channel under `domain_root` and is killed holding it.
fn leave_a_dead_holder_of_the_shallow_stale_holder_channel(domain_root: &std::path::Path) {
    let holder = crate::core::test_support::rerun_this_test_in_a_child_process(
        "iceoryx2::node::domain_and_reclaim_tests::a_deeper_open_survives_a_shallow_service_a_dead_holder_left_behind",
        STALE_HOLDER_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE,
        domain_root.as_os_str(),
    );
    // The signal and not merely a non-zero exit: a holder that panicked
    // before it created its shallow service would also exit non-zero, and
    // a later open would then create a fresh service and prove nothing.
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&holder.status),
        Some(libc::SIGKILL),
        "the holder must die where it stood, holding its service: {}",
        String::from_utf8_lossy(&holder.stderr),
    );
}

/// A long-lived node's deeper open gets past a shallow service whose only
/// holder died, because iceoryx2 reclaims a service's dead holders on open.
///
/// Fail-without-fix: set `cleanup_dead_nodes_on_open` to `false` in
/// [`iceoryx2_config_for_domain`] and the open is refused for depth on every
/// reopen, for as long as the long-lived node lives.
#[test]
fn a_deeper_open_survives_a_shallow_service_a_dead_holder_left_behind() {
    if let Some(domain_root) = std::env::var_os(STALE_HOLDER_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE)
    {
        let node = Iceoryx2Node::new(
            std::path::Path::new(&domain_root),
            "streamlib-test/stale-holder",
        )
        .expect("the holder's node opens in the engine-owned domain");
        let _shallow_channel = node
            .open_or_create_service(
                STALE_HOLDER_CHANNEL_SERVICE_NAME,
                STALE_HOLDER_MAX_SUBSCRIBERS,
                STALE_HOLDER_SHALLOW_DEPTH,
            )
            .expect("the holder creates the channel at its own shallow depth");
        // SAFETY: this process signalling itself, which is what leaves the
        // service held by a node with no process behind it.
        unsafe { libc::kill(std::process::id() as libc::pid_t, libc::SIGKILL) };
        unreachable!("SIGKILL to self does not return");
    }

    let domain = domain_root_parent_within_the_socket_path_budget();
    let domain_root = domain.path().join("iox2");

    // This node is created BEFORE the holder dies and is never replaced, so
    // no node creation sweeps the dead holder out from under it — the app
    // process's own arrangement.
    let long_lived_node = Iceoryx2Node::new(&domain_root, "streamlib-test/long-lived")
        .expect("the long-lived node opens first");

    leave_a_dead_holder_of_the_shallow_stale_holder_channel(&domain_root);
    let shallow_service_the_holder_left =
        <ipc_threadsafe::Service as iceoryx2::service::Service>::details(
            &iceoryx2_service_name_of(STALE_HOLDER_CHANNEL_SERVICE_NAME).unwrap(),
            &long_lived_node.config(),
            iceoryx2::service::messaging_pattern::MessagingPattern::PublishSubscribe,
        )
        .expect("the domain's services can be read without opening one")
        .expect("the dead holder's shallow service is still there");
    assert_eq!(
        shallow_service_the_holder_left
            .static_details
            .publish_subscribe()
            .subscriber_max_buffer_size(),
        STALE_HOLDER_SHALLOW_DEPTH,
    );

    let deeper_channel = long_lived_node
        .open_or_create_service(
            STALE_HOLDER_CHANNEL_SERVICE_NAME,
            STALE_HOLDER_MAX_SUBSCRIBERS,
            STALE_HOLDER_DEEPER_DEPTH,
        )
        .expect("the open must get past a shallow service nothing live holds");
    assert_eq!(
        deeper_channel.channel_service_creation_depth(),
        STALE_HOLDER_DEEPER_DEPTH,
        "the recreated service must carry the depth this open asked for"
    );
}

/// Set only in the child process the bounded-reclaim-record test re-runs
/// itself in, naming the domain root it opens.
const BOUNDED_RECLAIM_RECORD_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_TEST_BOUNDED_RECLAIM_RECORD_CHILD_ICEORYX2_DOMAIN_ROOT";

/// The record iceoryx2 0.10 writes as an open reclaims a service's dead
/// holder — a `from self` record whose origin is the open's whole service
/// builder — reaches the JSONL with that origin cut at its cap.
///
/// Runs in a child process: iceoryx2 takes one logger per process, for
/// good, so the bridge is installed there before anything touches
/// iceoryx2, rather than racing every other test in this binary for it.
#[test]
fn an_open_reclaiming_a_dead_holder_logs_its_builder_bounded() {
    use crate::core::logging::iceoryx2_log_bridge::{
        ICEORYX2_LOG_RECORD_MESSAGE_CAP_BYTES, ICEORYX2_LOG_RECORD_ORIGIN_CAP_BYTES,
        iceoryx2_log_record_truncation_marker,
    };
    use crate::core::logging::one_stream_log_file_written_on_a_test_thread::{
        OneStreamLogFileWrittenOnThisTestThread, read_every_record_of_a_jsonl_log,
    };
    use crate::core::logging::{
        LoggingTunables, StreamlibLoggingConfig,
        install_iceoryx2_log_bridge_at_the_engines_configured_level,
    };
    use streamlib_runtime_client_contract::runtime_log_event::RuntimeLogEvent;

    let Some(domain_root) =
        std::env::var_os(BOUNDED_RECLAIM_RECORD_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE)
    else {
        let domain = domain_root_parent_within_the_socket_path_budget();
        let domain_root = domain.path().join("iox2");
        let observer =
            crate::core::test_support::rerun_this_test_in_a_child_process_with_environment(
                "iceoryx2::node::domain_and_reclaim_tests::an_open_reclaiming_a_dead_holder_logs_its_builder_bounded",
                &[
                    (
                        BOUNDED_RECLAIM_RECORD_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE,
                        domain_root.as_os_str(),
                    ),
                    ("RUST_LOG", std::ffi::OsStr::new("info")),
                ],
            );
        assert!(
            observer.status.success(),
            "the observer process failed: {}{}",
            String::from_utf8_lossy(&observer.stdout),
            String::from_utf8_lossy(&observer.stderr),
        );
        return;
    };
    let domain_root = std::path::PathBuf::from(domain_root);

    let project_directory =
        crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
    let logging = OneStreamLogFileWrittenOnThisTestThread::install(
        StreamlibLoggingConfig {
            service_name: "test".into(),
            pretty_log_mirror_stream: None,
            intercept_stdio: false,
            tunables: LoggingTunables::default(),
        },
        "RtestBoundedReclaimRecord",
        project_directory.path(),
    );
    install_iceoryx2_log_bridge_at_the_engines_configured_level();

    let long_lived_node = Iceoryx2Node::new(&domain_root, "streamlib-test/long-lived")
        .expect("the long-lived node opens first");
    leave_a_dead_holder_of_the_shallow_stale_holder_channel(&domain_root);
    long_lived_node
        .open_or_create_service(
            STALE_HOLDER_CHANNEL_SERVICE_NAME,
            STALE_HOLDER_MAX_SUBSCRIBERS,
            STALE_HOLDER_DEEPER_DEPTH,
        )
        .expect("the open gets past the service its dead holder left");

    let path = logging.jsonl_log_path();
    logging.finish();
    let iceoryx2_records: Vec<_> = read_every_record_of_a_jsonl_log(&path)
        .into_iter()
        .filter(|record| record.target == "iceoryx2")
        .collect();

    fn origin_of(record: &RuntimeLogEvent) -> &str {
        record
            .attrs
            .get("origin")
            .and_then(|origin| origin.as_str())
            .unwrap_or_default()
    }
    let origin_truncation_marker =
        iceoryx2_log_record_truncation_marker(ICEORYX2_LOG_RECORD_ORIGIN_CAP_BYTES);
    let most_forwarded_origin_bytes =
        ICEORYX2_LOG_RECORD_ORIGIN_CAP_BYTES + origin_truncation_marker.len();
    let most_forwarded_message_bytes = ICEORYX2_LOG_RECORD_MESSAGE_CAP_BYTES
        + iceoryx2_log_record_truncation_marker(ICEORYX2_LOG_RECORD_MESSAGE_CAP_BYTES).len();

    let reclaim_record = iceoryx2_records
        .iter()
        .find(|record| record.message.starts_with("Detected dead node"))
        .expect("the open logs the dead holder it reclaims");
    assert!(
        origin_of(reclaim_record).ends_with(&origin_truncation_marker),
        "the reclaim record's origin is the open's whole builder, so it is cut: {:?}",
        origin_of(reclaim_record),
    );
    for record in &iceoryx2_records {
        let origin = origin_of(record);
        assert!(
            origin.len() <= most_forwarded_origin_bytes,
            "an iceoryx2 origin of {} bytes reached the log",
            origin.len()
        );
        assert!(
            record.message.len() <= most_forwarded_message_bytes,
            "an iceoryx2 message of {} bytes reached the log",
            record.message.len()
        );
    }
}

#[test]
fn the_sweep_reads_the_engine_owned_domain_and_never_the_ambient_one() {
    // The root-and-prefix budget belongs to `engine_owned_iceoryx2_config`,
    // so a refusal by name here is the proof the sweep is built from that
    // configuration rather than from iceoryx2's own lookup path.
    let prefix = engine_owned_iceoryx2_prefix_for_this_user();
    let root_one_byte_past_the_budget = format!(
        "/{}",
        "r".repeat(ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES - prefix.len())
    );

    let refusal = reclaim_dead_iceoryx2_nodes_in_engine_owned_domain(std::path::Path::new(
        &root_one_byte_past_the_budget,
    ))
    .expect_err("a root past the budget is refused before any listing")
    .to_string();

    assert!(
        refusal.contains(&root_one_byte_past_the_budget),
        "{refusal}"
    );
}

const HIJACKED_MAX_SUBSCRIBERS: usize = 3;

/// Set only in the child process the working-directory test re-runs itself in.
const CWD_CONFIG_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_TEST_CWD_CONFIG_CHILD_ICEORYX2_DOMAIN_ROOT";

/// The working directory is process-wide, so the node opens in a child test
/// process started inside the directory holding the config file; changing
/// this process's directory would move it under every test running beside it.
#[test]
fn an_iceoryx2_toml_in_the_working_directory_has_no_effect_on_a_node() {
    if let Some(domain_root) = std::env::var_os(CWD_CONFIG_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE) {
        open_a_node_beside_a_working_directory_iceoryx2_toml(std::path::Path::new(&domain_root));
        return;
    }

    let working_directory =
        crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
    let hijacked_root = working_directory.path().join("hijacked");
    streamlib_runtime_client_contract::directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode(
            &working_directory.path().join("config"),
            streamlib_runtime_client_contract::directory_at_an_explicit_mode::OWNER_ONLY_DIRECTORY_MODE,
        )
        .unwrap();
    std::fs::write(
        working_directory
            .path()
            .join("config")
            .join("iceoryx2.toml"),
        format!(
            "[global]\nroot-path = \"{}\"\nprefix = \"hijack_\"\n\n\
                 [defaults.publish-subscribe]\nmax-subscribers = {HIJACKED_MAX_SUBSCRIBERS}\n",
            hijacked_root.display()
        ),
    )
    .unwrap();
    let domain = domain_root_parent_within_the_socket_path_budget();
    let domain_root = domain.path().join("iox2");

    let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "iceoryx2::node::domain_and_reclaim_tests::an_iceoryx2_toml_in_the_working_directory_has_no_effect_on_a_node",
                "--exact",
                "--test-threads=1",
            ])
            .env(CWD_CONFIG_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE, &domain_root)
            .current_dir(working_directory.path())
            .output()
            .expect("the test binary re-runs this test in a child process");
    let child_stdout = String::from_utf8_lossy(&child.stdout);
    assert!(
        child.status.success() && child_stdout.contains("1 passed"),
        "the child test process must run this test and pass\nstdout:\n{child_stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(
        !hijacked_root.exists(),
        "nothing may be written where the working directory's config points"
    );
}

fn open_a_node_beside_a_working_directory_iceoryx2_toml(domain_root: &std::path::Path) {
    assert!(
        std::path::Path::new("config/iceoryx2.toml").is_file(),
        "the child must start in the directory holding the config file"
    );

    let node = Iceoryx2Node::new(domain_root, "streamlib-test/cwd-config-ignored")
        .expect("a node opens in the engine-owned domain");

    let config = node.config();
    assert_eq!(
        config.global.root_path().as_bytes_const(),
        domain_root.as_os_str().as_encoded_bytes()
    );
    assert_eq!(
        config.global.prefix.as_bytes_const(),
        engine_owned_iceoryx2_prefix_for_this_user().as_bytes()
    );
    assert_ne!(
        Config::default().defaults.publish_subscribe.max_subscribers,
        HIJACKED_MAX_SUBSCRIBERS
    );
    assert_eq!(
        config.defaults.publish_subscribe.max_subscribers,
        Config::default().defaults.publish_subscribe.max_subscribers,
        "a value the engine never sets must still be the library default, not the file's"
    );
    assert!(
        names_of_the_live_nodes_in(&engine_owned_iceoryx2_config(domain_root).unwrap())
            .contains(&"streamlib-test/cwd-config-ignored".to_string()),
        "the named node must be listed in the engine-owned domain"
    );
}

/// A test-domain prefix naming a child process held alive until this is
/// dropped, then killed, so the first sweep after it reclaims the shared
/// memory a test leaves under it.
///
/// Alive, because every test process runs a one-time sweep at its first
/// iceoryx2 use that deletes each `/dev/shm` segment whose prefix names a
/// process that is gone. Nextest starts sibling test processes throughout a
/// test's run, so a prefix naming an exited pid has its shared memory
/// deleted underneath it mid-test (`HangsInCreation`,
/// `ServiceInCorruptedState`).
struct TestDomainPrefixNamingALiveChildProcess {
    prefix: String,
    child_process_the_prefix_names: std::process::Child,
}

impl TestDomainPrefixNamingALiveChildProcess {
    fn new() -> Self {
        use crate::iceoryx2::iceoryx2_domain_for_this_test_process::test_domain_prefix;

        let child_process_the_prefix_names = std::process::Command::new("sleep")
            .arg("3600")
            .spawn()
            .expect("a child process for a test domain's prefix to name");
        let prefix = test_domain_prefix(current_process_uid(), child_process_the_prefix_names.id());
        Self {
            prefix,
            child_process_the_prefix_names,
        }
    }

    fn prefix(&self) -> &str {
        &self.prefix
    }
}

impl Drop for TestDomainPrefixNamingALiveChildProcess {
    fn drop(&mut self) {
        let _ = self.child_process_the_prefix_names.kill();
        let _ = self.child_process_the_prefix_names.wait();
    }
}

#[test]
fn two_test_process_domains_share_neither_files_nor_shared_memory() {
    let first_domain_root_parent = domain_root_parent_within_the_socket_path_budget();
    let second_domain_root_parent = domain_root_parent_within_the_socket_path_budget();
    let first_root = first_domain_root_parent.path().join("iox2");
    let second_root = second_domain_root_parent.path().join("iox2");
    let first_domain_prefix = TestDomainPrefixNamingALiveChildProcess::new();
    let second_domain_prefix = TestDomainPrefixNamingALiveChildProcess::new();
    let first_prefix = first_domain_prefix.prefix();
    let second_prefix = second_domain_prefix.prefix();
    let service_name = ServiceName::new(&unique_service_name("disjoint")).unwrap();

    let first_node =
        create_iceoryx2_node_in_domain(&first_root, first_prefix, "streamlib-test/first").unwrap();
    let _service_in_the_first_domain = first_node
        .service_builder(&service_name)
        .publish_subscribe::<[u8]>()
        .create()
        .expect("the first domain creates the service");

    let second_node =
        create_iceoryx2_node_in_domain(&second_root, second_prefix, "streamlib-test/second")
            .unwrap();
    assert!(
        second_node
            .service_builder(&service_name)
            .publish_subscribe::<[u8]>()
            .open()
            .is_err(),
        "a service created in one domain must not be visible from another"
    );
    let _service_in_the_second_domain = second_node
        .service_builder(&service_name)
        .publish_subscribe::<[u8]>()
        .create()
        .expect("the same service name creates afresh in a domain with its own shared memory");
    assert!(
        !names_of_the_live_nodes_in(
            &iceoryx2_config_for_domain(&second_root, second_prefix).unwrap()
        )
        .contains(&"streamlib-test/first".to_string())
    );

    let another_node_in_the_first_domain =
        create_iceoryx2_node_in_domain(&first_root, first_prefix, "streamlib-test/first-again")
            .unwrap();
    another_node_in_the_first_domain
        .service_builder(&service_name)
        .publish_subscribe::<[u8]>()
        .open()
        .expect("the same root and prefix are the same domain");
}

/// Two domains that differ only by root, sharing one prefix as every
/// engine-owned node does, never share a channel — which is what lets a
/// runtime stand up in a domain of its own with nothing but a root.
///
/// Fail-without-fix: iceoryx2 0.9.3 named a service's dynamic config from
/// the prefix and the service name alone, so creating the channel in the
/// second domain replaced the first domain's, and a subscriber the first
/// domain opened afterwards joined the second domain's channel: it heard
/// the second domain's publisher and never its own.
#[test]
fn two_domains_sharing_a_prefix_never_share_a_channel() {
    use crate::iceoryx2::DeliveryProfile;

    let first_domain_root_parent = domain_root_parent_within_the_socket_path_budget();
    let second_domain_root_parent = domain_root_parent_within_the_socket_path_budget();
    let first_root = first_domain_root_parent.path().join("iox2");
    let second_root = second_domain_root_parent.path().join("iox2");
    let shared_domain_prefix = TestDomainPrefixNamingALiveChildProcess::new();
    let shared_prefix = shared_domain_prefix.prefix();
    let channel_service_name = unique_service_name("shared_prefix");
    // One past the first domain's lone subscriber, so a refusal for room
    // can never stand in for the delivery this test asserts on.
    let max_subscribers = 2;
    let open_the_channel_in = |root: &std::path::Path, node_name: &str| {
        Iceoryx2Node::wrapping(
            create_iceoryx2_node_in_domain(root, shared_prefix, node_name).unwrap_or_else(
                |refusal| panic!("{node_name} opens a node in its domain: {refusal:?}"),
            ),
        )
        .open_or_create_service(
            &channel_service_name,
            max_subscribers,
            DeliveryProfile::ORDERED_DEPTH,
        )
        .unwrap_or_else(|refusal| panic!("{node_name} opens the channel: {refusal:?}"))
    };
    let publish_one_bag_from = |publisher: &ChannelDataServicePublisher| {
        TestChannelSample::carrying(0, &[0u8; 8]).send_unnumbered_on(publisher)
    };

    // Each source opens and publishes, and only then does a subscriber in
    // the first domain open the channel.
    let first_domains_publisher = open_the_channel_in(&first_root, "streamlib-test/first")
        .create_publisher(64)
        .expect("the first domain's publisher");
    let second_domains_publisher = open_the_channel_in(&second_root, "streamlib-test/second")
        .create_publisher(64)
        .expect("the second domain's publisher");
    let first_domains_subscriber =
        open_the_channel_in(&first_root, "streamlib-test/first-subscriber")
            .create_subscriber(DeliveryProfile::ORDERED_DEPTH)
            .expect("the first domain's subscriber");

    assert_eq!(
        publish_one_bag_from(&second_domains_publisher),
        0,
        "the second domain's publisher reached a subscriber, and the only one on this \
             channel name lives in the first domain"
    );
    assert!(
        first_domains_subscriber
            .receive()
            .expect("a receive")
            .is_none(),
        "the first domain's subscriber heard a bag published in the second domain"
    );
    assert_eq!(
        publish_one_bag_from(&first_domains_publisher),
        1,
        "the first domain's publisher must reach the subscriber its own domain opened"
    );
    assert!(
        first_domains_subscriber
            .receive()
            .expect("a receive")
            .is_some(),
        "the first domain's subscriber must hear its own domain's publisher"
    );
}
