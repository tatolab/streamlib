// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Two streams sharing one engine: their devices, pools, surfaces, topics and logs.

use super::*;

/// Unloading a stream stops it and frees its name for the next load; the
/// other stream stays loaded.
#[test]
#[serial]
fn an_unloaded_stream_frees_its_name_and_leaves_the_other_loaded() {
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let unloaded = an_empty_stream_loaded_into(&runner, project_directory.path(), "unloaded");
    let kept = an_empty_stream_loaded_into(&runner, project_directory.path(), "kept");

    runner
        .unload_stream("unloaded")
        .expect("the stream unloads cleanly");

    assert_eq!(
        unloaded.how_this_stream_ended(),
        Some(HowALoadedStreamEnded::Stopped)
    );
    assert_eq!(runner.ids_of_the_loaded_streams(), ["kept"]);
    assert!(!kept.has_ended());
    let reloaded = an_empty_stream_loaded_into(&runner, project_directory.path(), "unloaded");
    assert!(!Arc::ptr_eq(&reloaded, &unloaded));
}

/// Counts the ticks of [`TickCountingInTheFirstStream`].
static TICKS_IN_THE_FIRST_STREAM: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Counts the ticks of [`TickCountingInTheSecondStream`].
static TICKS_IN_THE_SECOND_STREAM: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// What [`TickCountingInTheFirstStream`] logs on its first tick.
const TOKEN_OF_THE_FIRST_STREAMS_PROCESSOR_THREAD: &str =
    "token-of-the-first-streams-processor-thread";

/// What [`TickCountingInTheSecondStream`] logs on its first tick.
const TOKEN_OF_THE_SECOND_STREAMS_PROCESSOR_THREAD: &str =
    "token-of-the-second-streams-processor-thread";

/// A source that counts its own ticks, for the first stream.
#[crate::processor(execution = continuous(interval_ms = 5))]
pub(crate) struct TickCountingInTheFirstStream;

impl crate::core::ContinuousProcessor for TickCountingInTheFirstStream::Processor {
    fn process(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
    ) -> Result<()> {
        if TICKS_IN_THE_FIRST_STREAM.fetch_add(1, Ordering::SeqCst) == 0 {
            tracing::info!("{TOKEN_OF_THE_FIRST_STREAMS_PROCESSOR_THREAD}");
        }
        Ok(())
    }
}

/// A source that counts its own ticks, for the second stream.
#[crate::processor(execution = continuous(interval_ms = 5))]
pub(crate) struct TickCountingInTheSecondStream;

impl crate::core::ContinuousProcessor for TickCountingInTheSecondStream::Processor {
    fn process(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
    ) -> Result<()> {
        if TICKS_IN_THE_SECOND_STREAM.fetch_add(1, Ordering::SeqCst) == 0 {
            tracing::info!("{TOKEN_OF_THE_SECOND_STREAMS_PROCESSOR_THREAD}");
        }
        Ok(())
    }
}

/// Two started streams hold the engine's one Vulkan device, each one's
/// processor thread logs only to its own stream's file, and one's
/// shutdown leaves the other started and processing, its graph changes
/// still committing on its own compiler.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[serial]
fn two_started_streams_share_one_vulkan_device_and_one_shutdown_leaves_the_other_processing() {
    use crate::core::processors::{PROCESSOR_REGISTRY, ProcessorSpec};

    PROCESSOR_REGISTRY.register::<TickCountingInTheFirstStream::Processor>();
    PROCESSOR_REGISTRY.register::<TickCountingInTheSecondStream::Processor>();
    let project_directory =
        crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
    let runner = Runner::new().expect("Runner::new");
    let load_named = |stream_id: &str| {
        runner
            .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
                project_directory.path(),
                stream_id,
            ))
            .expect("an empty stream loads")
    };
    let first = load_named("first");
    let second = load_named("second");
    let project_log = first.jsonl_log_path().expect("first logs").to_path_buf();
    assert_eq!(second.jsonl_log_path(), Some(project_log.as_path()));
    first
        .add_processor(ProcessorSpec::new(
            TickCountingInTheFirstStream::processor_class_import_path(),
            serde_json::json!({}),
        ))
        .expect("the first stream's source is added");
    second
        .add_processor(ProcessorSpec::new(
            TickCountingInTheSecondStream::processor_class_import_path(),
            serde_json::json!({}),
        ))
        .expect("the second stream's source is added");
    first.start().expect("the first stream starts");
    second.start().expect("the second stream starts");
    for stream in [&first, &second] {
        stream
            .wait_until_every_processor_is_running(Duration::from_secs(30))
            .expect("the stream's source runs");
    }

    let device_of = |stream: &LoadedStreamInThisRuntime| {
        Arc::clone(
            stream
                .runtime_context_while_started()
                .expect("a started stream has its runtime context")
                .gpu
                .device(),
        )
    };
    assert!(
        Arc::ptr_eq(&device_of(&first), &device_of(&second)),
        "two streams in one engine must hold one Vulkan device"
    );

    first.ask_for_this_streams_shutdown("the test shuts the first stream down");
    runner
        .wait_until_the_stream_ends(&first)
        .expect("the first stream stops cleanly");
    assert_eq!(first.status(), RuntimeStatus::Stopped);

    let first_ticks_once_stopped = TICKS_IN_THE_FIRST_STREAM.load(Ordering::SeqCst);
    let second_ticks_once_the_first_stopped = TICKS_IN_THE_SECOND_STREAM.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        TICKS_IN_THE_FIRST_STREAM.load(Ordering::SeqCst),
        first_ticks_once_stopped,
        "the stopped stream's processor kept running"
    );
    assert!(
        TICKS_IN_THE_SECOND_STREAM.load(Ordering::SeqCst) > second_ticks_once_the_first_stopped,
        "the other stream stopped processing when the first shut down"
    );
    assert_eq!(second.status(), RuntimeStatus::Started);
    assert_eq!(runner.ids_of_the_loaded_streams(), ["second"]);

    second
        .add_processor(ProcessorSpec::new(
            TickCountingInTheSecondStream::processor_class_import_path(),
            serde_json::json!({}),
        ))
        .expect("a live add into the running stream");
    second
        .wait_until_every_processor_is_running(Duration::from_secs(30))
        .expect("the live add commits on the running stream's own compiler");
    assert_eq!(
        second.to_json().unwrap()["nodes"].as_array().map(Vec::len),
        Some(2)
    );

    runner.shut_down().expect("the engine shuts down");
    assert!(second.has_ended());

    let project_records = every_record_of_the_stream_log_at(&project_log);
    for (stream_id, own_token, other_token) in [
        (
            "first",
            TOKEN_OF_THE_FIRST_STREAMS_PROCESSOR_THREAD,
            TOKEN_OF_THE_SECOND_STREAMS_PROCESSOR_THREAD,
        ),
        (
            "second",
            TOKEN_OF_THE_SECOND_STREAMS_PROCESSOR_THREAD,
            TOKEN_OF_THE_FIRST_STREAMS_PROCESSOR_THREAD,
        ),
    ] {
        let records: Vec<_> = project_records
            .iter()
            .filter(|record| record.stream.as_deref() == Some(stream_id))
            .collect();
        for record in &records {
            assert_ne!(
                record.message, other_token,
                "the records of `{stream_id}` hold the other stream's processor record"
            );
        }
        assert!(
            records.iter().any(|record| record.message == own_token),
            "the records of `{stream_id}` lack its processor thread's record: {records:#?}"
        );
    }
}

/// Stopping a stream frees the shared pool entries it last acquired and
/// let go of, retiring their frames; another stream's entries stay.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[serial]
fn stopping_a_stream_frees_the_shared_pool_entries_it_last_acquired_and_leaves_another_streams() {
    use crate::core::context::TexturePoolDescriptor;
    use crate::core::rhi::{PixelFormat, TextureFormat, TextureUsages};

    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let stopped = an_empty_stream_loaded_into(&runner, project_directory.path(), "stopped");
    let still_running =
        an_empty_stream_loaded_into(&runner, project_directory.path(), "still-running");
    stopped.start().expect("the stream to stop starts");
    still_running.start().expect("the other stream starts");
    let gpu_of = |stream: &LoadedStreamInThisRuntime| {
        stream
            .runtime_context_while_started()
            .expect("a started stream has its runtime context")
            .gpu
            .clone()
    };
    let stopped_streams_gpu = gpu_of(&stopped);
    let still_running_streams_gpu = gpu_of(&still_running);
    let texture_shape_of_width = |width: u32| {
        TexturePoolDescriptor::new(width, 16, TextureFormat::Rgba8Unorm)
            .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST)
    };
    let let_go_frame_of = |gpu: &crate::core::context::GpuContext, width: u32| {
        gpu.acquire_pixel_buffer(width, 16, PixelFormat::Rgba32)
            .expect("a pixel buffer")
            .0
            .to_string()
    };

    drop(
        stopped_streams_gpu
            .acquire_texture(&texture_shape_of_width(16))
            .expect("the stopped stream's texture"),
    );
    let stopped_streams_frame = let_go_frame_of(&stopped_streams_gpu, 16);
    drop(
        still_running_streams_gpu
            .acquire_texture(&texture_shape_of_width(32))
            .expect("the other stream's texture"),
    );
    let still_running_streams_frame = let_go_frame_of(&still_running_streams_gpu, 32);

    stopped.stop().expect("the stream stops");

    let texture_pool = still_running_streams_gpu.texture_pool();
    assert_eq!(
        texture_pool.slot_count_of_the_shape_of(&texture_shape_of_width(16)),
        0,
        "the stopped stream's texture outlived its stop"
    );
    assert_eq!(
        texture_pool.slot_count_of_the_shape_of(&texture_shape_of_width(32)),
        1,
        "another stream's texture went with the stop"
    );
    assert!(
        still_running_streams_gpu
            .resolve_pixel_buffer_by_surface_id(&stopped_streams_frame)
            .is_err(),
        "the stopped stream's freed frame still resolves"
    );
    still_running_streams_gpu
        .resolve_pixel_buffer_by_surface_id(&still_running_streams_frame)
        .expect("another stream's frame still resolves");

    runner.shut_down().expect("the engine shuts down");
}

/// Check in a memfd under `owner_key` over `connection`, as a surface
/// store registering under that owner does.
#[cfg(target_os = "linux")]
fn check_in_a_surface_under(connection: &std::os::unix::net::UnixStream, owner_key: &str) {
    let name = std::ffi::CString::new("a-surface-a-stream-registered").unwrap();
    let memfd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
    assert!(memfd >= 0, "memfd_create failed");
    let contents = b"pixels";
    let written = unsafe { libc::write(memfd, contents.as_ptr().cast(), contents.len()) };
    assert_eq!(written, contents.len() as isize);
    let (answer, _) = streamlib_surface_client::send_request_with_fds(
        connection,
        &serde_json::json!({
            "op": "check_in",
            "runtime_id": owner_key,
            "width": 16,
            "height": 16,
            "format": "bgra32",
            "resource_type": "pixel_buffer",
        }),
        &[memfd],
        0,
    )
    .expect("the check-in is answered");
    unsafe { libc::close(memfd) };
    assert!(
        answer.get("surface_id").is_some(),
        "the check-in was refused: {answer}"
    );
}

/// An unloaded stream's surface registrations leave the engine's
/// surface-sharing service, which outlives the stream and keeps a
/// registration from this process past its connection's close; the other
/// stream's and the engine's own stay.
///
/// Mental-revert: without the release in the stream's stop, the unloaded
/// stream's registration stays until the engine shuts down.
#[cfg(target_os = "linux")]
#[test]
#[serial]
fn an_unloaded_streams_surface_registrations_go_and_the_others_stay() {
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let unloaded = an_empty_stream_loaded_into(&runner, project_directory.path(), "unloaded");
    let staying = an_empty_stream_loaded_into(&runner, project_directory.path(), "staying");
    let unloaded_streams_owner_key = unloaded.loaded_stream_identity().to_string();
    let staying_streams_owner_key = staying.loaded_stream_identity().to_string();
    let the_engines_owner_key = runner.runtime_id().to_string();
    let connection =
        streamlib_surface_client::connect_to_surface_share_socket(runner.surface_socket_path())
            .expect("the engine's surface-sharing service accepts a connection");
    for owner_key in [
        &unloaded_streams_owner_key,
        &staying_streams_owner_key,
        &the_engines_owner_key,
    ] {
        check_in_a_surface_under(&connection, owner_key);
    }

    runner
        .unload_stream("unloaded")
        .expect("the stream unloads");

    let registrations = &runner
        .engine_resources_shared_by_every_stream
        .surface_share_registrations_by_owner;
    assert!(
        registrations
            .surface_ids_by_runtime(&unloaded_streams_owner_key)
            .is_empty(),
        "the unloaded stream's registration outlived it"
    );
    assert_eq!(
        registrations
            .surface_ids_by_runtime(&staying_streams_owner_key)
            .len(),
        1,
        "another stream's registration went with the unloaded one"
    );
    assert_eq!(
        registrations
            .surface_ids_by_runtime(&the_engines_owner_key)
            .len(),
        1,
        "the engine's own registration went with the unloaded stream"
    );
}

/// Two streams loaded under one name in one runtime — the second loaded
/// once the first left the table — publish on different topics and own
/// different surface registrations.
///
/// Mental-revert: an identity of the runtime id and the name alone gives
/// both one topic, so a late event of the first reaches the second's
/// listener.
#[test]
#[serial]
fn a_stream_loaded_under_the_name_of_one_that_left_has_its_own_topic() {
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let first = an_empty_stream_loaded_into(&runner, project_directory.path(), "main");
    runner
        .unload_stream("main")
        .expect("the first stream unloads");
    let second = an_empty_stream_loaded_into(&runner, project_directory.path(), "main");

    assert_ne!(first.stream_tag(), second.stream_tag());
    assert_ne!(
        crate::core::pubsub::topics::loaded_stream(first.loaded_stream_identity()),
        crate::core::pubsub::topics::loaded_stream(second.loaded_stream_identity())
    );
    assert_ne!(
        first.loaded_stream_identity().to_string(),
        second.loaded_stream_identity().to_string()
    );
}

/// Every record the JSONL log at `path` holds.
pub(super) fn every_record_of_the_stream_log_at(
    path: &std::path::Path,
) -> Vec<streamlib_runtime_client_contract::runtime_log_event::RuntimeLogEvent> {
    crate::core::logging::one_stream_log_file_written_on_a_test_thread::read_every_record_of_a_jsonl_log(path)
}

/// Two streams of one project log into the project's one file: each
/// record — from a stream's own threads, from a thread it spawned, and
/// from engine code acting on it — stamped with its stream's id, and a
/// record no stream emitted lands in neither.
#[test]
#[serial]
fn two_streams_of_one_project_log_to_its_one_file_each_record_stamped_with_its_stream() {
    let project_directory =
        crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
    let runner = Runner::new().expect("Runner::new");
    let load_named = |stream_id: &str| {
        runner
            .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
                project_directory.path(),
                stream_id,
            ))
            .expect("an empty stream loads")
    };
    let first = load_named("first");
    let second = load_named("second");
    let project_log = streamlib_runtime_client_contract::runtime_log_file_paths::project_log_path(
        &std::fs::canonicalize(project_directory.path()).unwrap(),
    );
    assert_eq!(first.jsonl_log_path(), Some(project_log.as_path()));
    assert_eq!(second.jsonl_log_path(), Some(project_log.as_path()));

    first.log_route().run_entered(|| {
        std::thread::spawn(
            crate::core::logging::carrying_this_threads_project_log_route_entry(|| {
                tracing::info!("token-from-a-thread-the-first-stream-spawned")
            }),
        )
        .join()
        .expect("the spawned thread logs")
    });
    second
        .log_route()
        .run_entered(|| tracing::info!("token-of-the-second-stream"));
    tracing::info!("token-no-stream-emitted");
    runner.unload_stream("first").expect("first unloads");
    runner.unload_stream("second").expect("second unloads");

    let runtime_id = runner.runtime_id().to_string();
    let records = every_record_of_the_stream_log_at(&project_log);
    assert!(
        !records
            .iter()
            .any(|record| record.message == "token-no-stream-emitted"),
        "a record no stream emitted reached the project's log"
    );
    for record in &records {
        assert_eq!(record.runtime_id, runtime_id, "{record:?}");
    }
    for (stream_id, own_token, other_stream_id, other_token) in [
        (
            "first",
            "token-from-a-thread-the-first-stream-spawned",
            "second",
            "token-of-the-second-stream",
        ),
        (
            "second",
            "token-of-the-second-stream",
            "first",
            "token-from-a-thread-the-first-stream-spawned",
        ),
    ] {
        let records_of_the_stream: Vec<_> = records
            .iter()
            .filter(|record| record.stream.as_deref() == Some(stream_id))
            .collect();
        for record in &records_of_the_stream {
            assert!(
                !record.message.contains(&format!("`{other_stream_id}`"))
                    && record.message != other_token,
                "a record of `{stream_id}` was emitted by `{other_stream_id}`: {record:?}"
            );
        }
        for message_the_stream_logged in [
            format!("Loading the stream `{stream_id}`"),
            format!("[stop] The stream `{stream_id}` stopped"),
            own_token.to_string(),
        ] {
            assert!(
                records_of_the_stream
                    .iter()
                    .any(|record| record.message.starts_with(&message_the_stream_logged)),
                "the records of `{stream_id}` lack `{message_the_stream_logged}`: \
                 {records_of_the_stream:#?}"
            );
        }
    }
}

/// A second runner in one process logs fully: both runners' streams of
/// one project write the project's one file, each record carrying its
/// runner's id.
#[test]
#[serial]
fn a_second_runner_in_one_process_logs_into_the_projects_file_under_its_own_id() {
    let project_directory =
        crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
    let first_runner = Runner::new().expect("the first runner constructs");
    let second_runner = Runner::new().expect("the second runner constructs");
    for runner in [&first_runner, &second_runner] {
        runner
            .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
                project_directory.path(),
                "main",
            ))
            .expect("an empty stream loads");
        runner.unload_stream("main").expect("the stream unloads");
    }

    let records = every_record_of_the_stream_log_at(
        &streamlib_runtime_client_contract::runtime_log_file_paths::project_log_path(
            &std::fs::canonicalize(project_directory.path()).unwrap(),
        ),
    );
    for runner in [&first_runner, &second_runner] {
        let runtime_id = runner.runtime_id().to_string();
        assert!(
            records.iter().any(|record| record.runtime_id == runtime_id
                && record.stream.as_deref() == Some("main")
                && record.message == "[stop] The stream `main` stopped"),
            "the project's log lacks the stop `{runtime_id}` logged: {records:#?}"
        );
    }
}

/// A stream's records land in its project's log, each stamped with the
/// stream's id, and the project's reader reads them back by stream.
#[test]
#[serial]
fn a_streams_records_land_in_its_projects_log_stamped_with_its_id() {
    let project_directory =
        crate::core::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
    let runner = Runner::new().expect("Runner::new");
    let stream = runner
        .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
            project_directory.path(),
            "main",
        ))
        .expect("an empty stream loads");
    stream.log_route().run_entered(|| {
        for token_index in 0..3 {
            tracing::info!("token-of-the-stream-{token_index}");
        }
    });
    assert_eq!(
        stream.jsonl_log_path(),
        Some(
            streamlib_runtime_client_contract::runtime_log_file_paths::project_log_path(
                &std::fs::canonicalize(project_directory.path()).unwrap()
            )
            .as_path()
        )
    );
    stream.log_route().write_every_record_it_emitted_and_sync();

    let page = streamlib_runtime_client_contract::project_log_records::project_log_records_after(
        project_directory.path(),
        Some("main"),
        None,
        None,
        4096,
    )
    .unwrap();
    let tokens: Vec<&str> = page
        .records
        .iter()
        .filter_map(|record| record["message"].as_str())
        .filter(|message| message.starts_with("token-of-the-stream-"))
        .collect();
    assert_eq!(
        tokens,
        [
            "token-of-the-stream-0",
            "token-of-the-stream-1",
            "token-of-the-stream-2"
        ]
    );
    runner.unload_stream("main").expect("the stream unloads");
}
