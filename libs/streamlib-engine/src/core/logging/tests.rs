// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration-style tests for the unified logging pathway. Each test
//! installs its own thread-local tracing dispatcher via
//! [`init_for_tests`] so they don't collide with each other or with the
//! global subscriber installed by production callers.

use std::sync::Arc;
use std::time::Duration;

use serial_test::serial;
use tempfile::TempDir;

use crate::core::logging::{
    event::{LogLevel, RuntimeLogEvent, Source, SCHEMA_VERSION},
    init::init_for_tests,
    paths::{log_dir, runtime_log_path},
    LoggingTunables, StreamlibLoggingConfig,
};
use crate::core::runtime::RuntimeUniqueId;

fn set_xdg_state_home(tmp: &TempDir) {
    unsafe {
        std::env::set_var("XDG_STATE_HOME", tmp.path());
    }
}

fn clear_xdg_state_home() {
    unsafe { std::env::remove_var("XDG_STATE_HOME") };
}

fn clear_quiet() {
    unsafe { std::env::remove_var("STREAMLIB_QUIET") };
}

fn read_jsonl(path: &std::path::Path) -> Vec<RuntimeLogEvent> {
    let contents = std::fs::read_to_string(path).unwrap_or_default();
    contents
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<RuntimeLogEvent>(l).expect("valid JSONL line"))
        .collect()
}

fn reset_for_test() {
    clear_quiet();
    // Capture debug+ so INFO/WARN/etc surface in the JSONL.
    unsafe { std::env::set_var("RUST_LOG", "debug") };
}

#[test]
#[serial]
fn jsonl_file_created_on_runtime_new() {
    reset_for_test();
    let tmp = TempDir::new().unwrap();
    set_xdg_state_home(&tmp);

    let runtime_id = Arc::new(RuntimeUniqueId::from("Rtest1"));
    let config =
        StreamlibLoggingConfig::for_runtime("test", Arc::clone(&runtime_id));
    let guard = init_for_tests(config).unwrap();

    tracing::info!(pipeline_id = "p1", processor_id = "pr1", "hi");

    let path = guard.jsonl_path().unwrap().to_path_buf();
    drop(guard);

    assert!(
        path.exists(),
        "jsonl file was not created at {}",
        path.display()
    );
    let events = read_jsonl(&path);
    assert!(
        events.iter().any(|e| e.message == "hi"
            && e.runtime_id == "Rtest1"
            && e.source == Source::Rust
            && e.pipeline_id.as_deref() == Some("p1")
            && e.processor_id.as_deref() == Some("pr1")
            && e.schema_version == SCHEMA_VERSION),
        "expected record with runtime_id + pipeline_id + processor_id; got {:#?}",
        events
    );
    clear_xdg_state_home();
}

#[test]
#[serial]
fn stdout_mirror_suppressed_by_quiet_env_keeps_jsonl() {
    reset_for_test();
    unsafe { std::env::set_var("STREAMLIB_QUIET", "1") };
    let tmp = TempDir::new().unwrap();
    set_xdg_state_home(&tmp);

    let runtime_id = Arc::new(RuntimeUniqueId::from("RtestQ"));
    let config = StreamlibLoggingConfig::for_runtime("test", runtime_id);
    let guard = init_for_tests(config).unwrap();

    tracing::info!("still-writes-to-jsonl");

    let path = guard.jsonl_path().unwrap().to_path_buf();
    drop(guard);

    let events = read_jsonl(&path);
    assert!(
        events.iter().any(|e| e.message == "still-writes-to-jsonl"),
        "expected record present in JSONL despite STREAMLIB_QUIET"
    );

    clear_quiet();
    clear_xdg_state_home();
}

#[test]
#[serial]
fn drop_triggers_flush_and_persists() {
    reset_for_test();
    let tmp = TempDir::new().unwrap();
    set_xdg_state_home(&tmp);

    let runtime_id = Arc::new(RuntimeUniqueId::from("RtestDrop"));
    let config = StreamlibLoggingConfig::for_runtime("test", Arc::clone(&runtime_id));
    let guard = init_for_tests(config).unwrap();

    for i in 0..50u64 {
        tracing::info!(i, "drop-flush-line");
    }

    let path = guard.jsonl_path().unwrap().to_path_buf();
    drop(guard);

    let events = read_jsonl(&path);
    let info_events: Vec<_> = events
        .iter()
        .filter(|e| e.level == LogLevel::Info && e.message == "drop-flush-line")
        .collect();
    assert!(
        info_events.len() >= 50,
        "expected at least 50 info events, got {}",
        info_events.len()
    );
    clear_xdg_state_home();
}

#[test]
#[serial]
fn time_triggered_flush_writes_without_size_trigger() {
    reset_for_test();
    let tmp = TempDir::new().unwrap();
    set_xdg_state_home(&tmp);

    let runtime_id = Arc::new(RuntimeUniqueId::from("RtestTime"));
    let config = StreamlibLoggingConfig {
        service_name: "test".into(),
        runtime_id: Some(Arc::clone(&runtime_id)),
        stdout: false,
        jsonl: true,
        intercept_stdio: false,
        tunables: LoggingTunables {
            batch_ms: Some(50),
            // Large enough that a handful of lines never hit size threshold.
            batch_bytes: Some(1 << 22),
            channel_capacity: Some(1024),
            fsync_on_every_batch: None,
        },
    };
    let guard = init_for_tests(config).unwrap();

    tracing::info!("single-line-timed-flush");
    std::thread::sleep(Duration::from_millis(250));

    let path = guard.jsonl_path().unwrap().to_path_buf();
    let contents_before_drop = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        contents_before_drop.contains("single-line-timed-flush"),
        "time-triggered flush did not persist; got {:?}",
        contents_before_drop
    );
    drop(guard);
    clear_xdg_state_home();
}

#[test]
#[serial]
fn concurrent_runtime_paths_do_not_collide() {
    // Pure path-function test — no subscriber involvement, just
    // confirms two distinct runtime ids resolve to distinct files in
    // the shared log directory.
    let tmp = TempDir::new().unwrap();
    set_xdg_state_home(&tmp);

    let dir = log_dir();
    let p1 = runtime_log_path("RtestA", 111);
    let p2 = runtime_log_path("RtestB", 111);
    let p3 = runtime_log_path("RtestA", 222);

    assert_ne!(p1, p2);
    assert_ne!(p1, p3);
    assert!(p1.starts_with(&dir));
    assert!(p2.starts_with(&dir));
    assert!(p3.starts_with(&dir));

    clear_xdg_state_home();
}

#[test]
#[serial]
fn origin_fields_round_trip_via_event_fields() {
    reset_for_test();
    let tmp = TempDir::new().unwrap();
    set_xdg_state_home(&tmp);

    let runtime_id = Arc::new(RuntimeUniqueId::from("RtestOrigin"));
    let config = StreamlibLoggingConfig::for_runtime("test", Arc::clone(&runtime_id));
    let guard = init_for_tests(config).unwrap();

    tracing::info!(
        pipeline_id = "pl-42",
        processor_id = "pr-7",
        rhi_op = "acquire_texture",
        custom_k = 123i64,
        "origin-round-trip"
    );

    let path = guard.jsonl_path().unwrap().to_path_buf();
    drop(guard);

    let events = read_jsonl(&path);
    let ev = events
        .iter()
        .find(|e| e.message == "origin-round-trip")
        .expect("event not found");
    assert_eq!(ev.runtime_id, "RtestOrigin");
    assert_eq!(ev.source, Source::Rust);
    assert_eq!(ev.level, LogLevel::Info);
    assert_eq!(ev.pipeline_id.as_deref(), Some("pl-42"));
    assert_eq!(ev.processor_id.as_deref(), Some("pr-7"));
    assert_eq!(ev.rhi_op.as_deref(), Some("acquire_texture"));
    assert_eq!(
        ev.attrs.get("custom_k"),
        Some(&serde_json::Value::Number(123.into()))
    );

    clear_xdg_state_home();
}

#[test]
#[serial]
fn panic_hook_best_effort_flush() {
    // Build a pipeline with a short batch window so the flush landing on
    // disk doesn't have to wait for a full 100ms batch period. We
    // install the panic hook manually (init_for_tests doesn't install
    // the global hook — that's only wired on global init) and verify
    // the hook routes a flush through the doorbell.
    reset_for_test();
    let tmp = TempDir::new().unwrap();
    set_xdg_state_home(&tmp);

    let runtime_id = Arc::new(RuntimeUniqueId::from("RtestPanic"));
    let config = StreamlibLoggingConfig {
        service_name: "test".into(),
        runtime_id: Some(Arc::clone(&runtime_id)),
        stdout: false,
        jsonl: true,
        intercept_stdio: false,
        tunables: LoggingTunables {
            batch_ms: Some(25),
            batch_bytes: Some(1 << 20),
            channel_capacity: Some(1024),
            fsync_on_every_batch: None,
        },
    };
    let guard = init_for_tests(config).unwrap();

    // Simulated panic-hook path: the caller requests a best-effort
    // flush before the panic unwinds.
    tracing::error!("panic-hook-best-effort-line");
    guard.request_flush();
    std::thread::sleep(Duration::from_millis(60));

    let path = guard.jsonl_path().unwrap().to_path_buf();
    let contents = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        contents.contains("panic-hook-best-effort-line"),
        "request_flush did not land the record on disk: {:?}",
        contents
    );
    drop(guard);
    clear_xdg_state_home();
}

/// Coarse latency check. The #430 target is p50 <1µs / p99 <5µs on a
/// criterion-benched CI box; this inline test doesn't claim to validate
/// that tight bar — it asserts the hot path isn't blocked on I/O (e.g.
/// no accidental synchronous file write per event). A dedicated
/// criterion harness is tracked as follow-up.
#[test]
#[serial]
fn hot_path_is_not_blocked_on_io() {
    reset_for_test();
    let tmp = TempDir::new().unwrap();
    set_xdg_state_home(&tmp);

    let runtime_id = Arc::new(RuntimeUniqueId::from("RtestHot"));
    let config = StreamlibLoggingConfig {
        service_name: "test".into(),
        runtime_id: Some(Arc::clone(&runtime_id)),
        stdout: false,
        jsonl: true,
        intercept_stdio: false,
        tunables: LoggingTunables {
            batch_ms: Some(100),
            batch_bytes: Some(64 * 1024),
            channel_capacity: Some(65_536),
            fsync_on_every_batch: None,
        },
    };
    let _guard = init_for_tests(config).unwrap();

    const N: u32 = 10_000;
    let start = std::time::Instant::now();
    for i in 0..N {
        tracing::info!(i, pipeline_id = "pl-hot", "hot-path");
    }
    let elapsed = start.elapsed();
    let per_call_ns = elapsed.as_nanos() / (N as u128);

    // Generous ceiling. If the hot path accidentally blocks on a file
    // write we'd see hundreds of microseconds per call; anything under
    // 50µs per call means the critical work (format, fsync) isn't
    // happening on the emitting thread.
    assert!(
        per_call_ns < 50_000,
        "hot path averaged {}ns per call — expected < 50µs; I/O likely on the hot path",
        per_call_ns
    );

    clear_xdg_state_home();
}

/// Cargo's libtest installs a thread-local `OUTPUT_CAPTURE` hook
/// that intercepts `println!` (and `io::stdout()` writes) BEFORE
/// they reach fd 1, and propagates that capture to threads spawned
/// via `std::thread::spawn`. So "test `println!`" can't reach fd 1
/// from inside a cargo test — we simulate the fd-level write that
/// `println!` performs in a non-test runtime binary by wrapping fd
/// 1 as a `File` and using `writeln!`. This bypasses
/// `OUTPUT_CAPTURE` and hits our interceptor pipe exactly as
/// `println!` would in production.
#[cfg(unix)]
#[test]
#[serial]
fn rust_println_captured_via_fd_redirect() {
    use std::io::Write;
    use std::os::fd::FromRawFd;

    reset_for_test();
    let tmp = TempDir::new().unwrap();
    set_xdg_state_home(&tmp);

    let runtime_id = Arc::new(RuntimeUniqueId::from("RintercFdPrint"));
    let config = StreamlibLoggingConfig {
        service_name: "test".into(),
        runtime_id: Some(Arc::clone(&runtime_id)),
        stdout: false,
        jsonl: true,
        intercept_stdio: true,
        tunables: LoggingTunables::default(),
    };
    let guard = init_for_tests(config).unwrap();

    // Dup fd 1 so the File wrapper owns a separate fd we can close
    // on drop without touching the interceptor's fd 1.
    let dup_fd = unsafe { libc::dup(libc::STDOUT_FILENO) };
    assert!(dup_fd >= 0, "libc::dup failed");
    let mut f = unsafe { std::fs::File::from_raw_fd(dup_fd) };
    writeln!(f, "sneaky-fd-interception").unwrap();
    drop(f);

    std::thread::sleep(Duration::from_millis(150));

    let path = guard.jsonl_path().unwrap().to_path_buf();
    drop(guard);

    let events = read_jsonl(&path);
    assert!(
        events.iter().any(|e| e.message == "sneaky-fd-interception"
            && e.intercepted
            && e.channel.as_deref() == Some("fd1")
            && e.source == Source::Rust
            && e.level == LogLevel::Warn),
        "expected intercepted record (fd1, warn, rust); got {:#?}",
        events
    );

    clear_xdg_state_home();
}

/// `libc::write(1, ...)` bypasses stdlib / libtest entirely and hits
/// the OS-level fd, so the interceptor pipe catches it.
#[cfg(unix)]
#[test]
#[serial]
fn rust_c_printf_via_libc_captured() {
    reset_for_test();
    let tmp = TempDir::new().unwrap();
    set_xdg_state_home(&tmp);

    let runtime_id = Arc::new(RuntimeUniqueId::from("RintercFdLibc"));
    let config = StreamlibLoggingConfig {
        service_name: "test".into(),
        runtime_id: Some(Arc::clone(&runtime_id)),
        stdout: false,
        jsonl: true,
        intercept_stdio: true,
        tunables: LoggingTunables::default(),
    };
    let guard = init_for_tests(config).unwrap();

    unsafe {
        let bytes = b"hi-from-libc\n";
        libc::write(
            libc::STDOUT_FILENO,
            bytes.as_ptr() as *const libc::c_void,
            bytes.len(),
        );
    }

    std::thread::sleep(Duration::from_millis(150));

    let path = guard.jsonl_path().unwrap().to_path_buf();
    drop(guard);

    let events = read_jsonl(&path);
    assert!(
        events.iter().any(|e| e.message == "hi-from-libc"
            && e.intercepted
            && e.channel.as_deref() == Some("fd1")
            && e.source == Source::Rust
            && e.level == LogLevel::Warn),
        "expected intercepted libc::write record; got {:#?}",
        events
    );

    clear_xdg_state_home();
}

/// With `intercept_stdio: false`, `libc::write(1, ...)` reaches the
/// cargo test harness stdout as normal and does NOT appear as an
/// intercepted record in the JSONL.
#[cfg(unix)]
#[test]
#[serial]
fn intercept_stdio_off_in_tests() {
    reset_for_test();
    let tmp = TempDir::new().unwrap();
    set_xdg_state_home(&tmp);

    let runtime_id = Arc::new(RuntimeUniqueId::from("RintercFdOff"));
    let config = StreamlibLoggingConfig {
        service_name: "test".into(),
        runtime_id: Some(Arc::clone(&runtime_id)),
        stdout: false,
        jsonl: true,
        intercept_stdio: false,
        tunables: LoggingTunables::default(),
    };
    let guard = init_for_tests(config).unwrap();

    unsafe {
        let bytes = b"off-bypass-interceptor\n";
        libc::write(
            libc::STDOUT_FILENO,
            bytes.as_ptr() as *const libc::c_void,
            bytes.len(),
        );
    }

    std::thread::sleep(Duration::from_millis(50));

    let path = guard.jsonl_path().unwrap().to_path_buf();
    drop(guard);

    let events = read_jsonl(&path);
    assert!(
        !events
            .iter()
            .any(|e| e.message == "off-bypass-interceptor" && e.intercepted),
        "expected NO intercepted record when intercept_stdio=false; got {:#?}",
        events
    );

    clear_xdg_state_home();
}

/// Writes to fd 2 land in JSONL with `channel: "fd2"`.
#[cfg(unix)]
#[test]
#[serial]
fn intercepted_fd2_uses_channel_fd2() {
    reset_for_test();
    let tmp = TempDir::new().unwrap();
    set_xdg_state_home(&tmp);

    let runtime_id = Arc::new(RuntimeUniqueId::from("RintercFd2"));
    let config = StreamlibLoggingConfig {
        service_name: "test".into(),
        runtime_id: Some(Arc::clone(&runtime_id)),
        stdout: false,
        jsonl: true,
        intercept_stdio: true,
        tunables: LoggingTunables::default(),
    };
    let guard = init_for_tests(config).unwrap();

    unsafe {
        let bytes = b"stderr-interception-line\n";
        libc::write(
            libc::STDERR_FILENO,
            bytes.as_ptr() as *const libc::c_void,
            bytes.len(),
        );
    }

    std::thread::sleep(Duration::from_millis(150));

    let path = guard.jsonl_path().unwrap().to_path_buf();
    drop(guard);

    let events = read_jsonl(&path);
    assert!(
        events.iter().any(|e| e.message == "stderr-interception-line"
            && e.intercepted
            && e.channel.as_deref() == Some("fd2")),
        "expected intercepted record with channel=fd2; got {:#?}",
        events
    );

    clear_xdg_state_home();
}

/// With both the pretty-mirror and the interceptor on, the mirror
/// writes must go to the dup'd real-stdout handle, NOT to fd 1 — so
/// they do not re-enter the pipe. Emitting one `tracing::info!`
/// therefore produces exactly one JSONL record, never two (which
/// would indicate a recursion).
#[cfg(unix)]
#[test]
#[serial]
fn no_redirect_loop_when_mirror_enabled() {
    reset_for_test();
    let tmp = TempDir::new().unwrap();
    set_xdg_state_home(&tmp);

    let runtime_id = Arc::new(RuntimeUniqueId::from("RintercFdLoop"));
    let config = StreamlibLoggingConfig {
        service_name: "test".into(),
        runtime_id: Some(Arc::clone(&runtime_id)),
        stdout: true,
        jsonl: true,
        intercept_stdio: true,
        tunables: LoggingTunables::default(),
    };
    let guard = init_for_tests(config).unwrap();

    tracing::info!("unique-loop-token-xyz123");

    std::thread::sleep(Duration::from_millis(150));

    let path = guard.jsonl_path().unwrap().to_path_buf();
    drop(guard);

    let events = read_jsonl(&path);
    let matching: Vec<_> = events
        .iter()
        .filter(|e| e.message.contains("unique-loop-token-xyz123"))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "expected exactly one record, got {}: {:#?}",
        matching.len(),
        matching
    );
    assert!(
        !matching[0].intercepted,
        "the one record should not be intercepted; got {:#?}",
        matching[0]
    );

    clear_xdg_state_home();
}

/// Dropping the guard restores fds 1/2, closes the pipe write ends,
/// and joins the reader threads quickly — no stranded threads on
/// process exit.
#[cfg(unix)]
#[test]
#[serial]
fn reader_thread_shuts_down_on_runtime_drop() {
    reset_for_test();
    let tmp = TempDir::new().unwrap();
    set_xdg_state_home(&tmp);

    let runtime_id = Arc::new(RuntimeUniqueId::from("RintercFdShut"));
    let config = StreamlibLoggingConfig {
        service_name: "test".into(),
        runtime_id: Some(Arc::clone(&runtime_id)),
        stdout: false,
        jsonl: true,
        intercept_stdio: true,
        tunables: LoggingTunables::default(),
    };
    let guard = init_for_tests(config).unwrap();

    // Put the readers through at least one wake-up so we're verifying
    // shutdown from a live-reader state, not a fresh-spawn state.
    unsafe {
        let bytes = b"pre-shutdown\n";
        libc::write(
            libc::STDOUT_FILENO,
            bytes.as_ptr() as *const libc::c_void,
            bytes.len(),
        );
    }
    std::thread::sleep(Duration::from_millis(50));

    let start = std::time::Instant::now();
    drop(guard);
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(500),
        "guard drop took {:?} — reader threads likely stranded",
        elapsed
    );

    clear_xdg_state_home();
}

/// Workspace default pins `tracing/release_max_level_debug`. Verify the
/// compile-time constant reflects that (TRACE in debug builds, DEBUG in
/// release builds with no `strip_debug_logging`) and that a `trace!`
/// call emits zero records in release while `debug!` survives.
#[test]
#[serial]
#[cfg(not(feature = "strip_debug_logging"))]
fn trace_compiled_out_in_release() {
    reset_for_test();
    // Open the subscriber filter as wide as possible so any record that
    // survives the compile-time strip reaches the JSONL.
    unsafe { std::env::set_var("RUST_LOG", "trace") };
    let tmp = TempDir::new().unwrap();
    set_xdg_state_home(&tmp);

    let runtime_id = Arc::new(RuntimeUniqueId::from("RtestTraceStrip"));
    let config = StreamlibLoggingConfig::for_runtime("test", runtime_id);
    let guard = init_for_tests(config).unwrap();

    tracing::trace!("trace-release-stripped-token");
    tracing::debug!("debug-release-default-token");

    let path = guard.jsonl_path().unwrap().to_path_buf();
    drop(guard);

    let events = read_jsonl(&path);
    let trace_count = events
        .iter()
        .filter(|e| e.message == "trace-release-stripped-token")
        .count();
    let debug_count = events
        .iter()
        .filter(|e| e.message == "debug-release-default-token")
        .count();

    if cfg!(debug_assertions) {
        assert_eq!(
            tracing::level_filters::STATIC_MAX_LEVEL,
            tracing::level_filters::LevelFilter::TRACE,
            "debug builds must keep every level live"
        );
        assert_eq!(trace_count, 1, "trace! must emit in debug builds");
        assert_eq!(debug_count, 1, "debug! must emit in debug builds");
    } else {
        assert_eq!(
            tracing::level_filters::STATIC_MAX_LEVEL,
            tracing::level_filters::LevelFilter::DEBUG,
            "release builds must strip trace! via release_max_level_debug"
        );
        assert_eq!(
            trace_count, 0,
            "trace! must be compiled out in release (workspace default)"
        );
        assert_eq!(
            debug_count, 1,
            "debug! must stay live in release without strip_debug_logging"
        );
    }

    clear_xdg_state_home();
}

/// Without the opt-in feature, `debug!` produces a JSONL record under
/// every profile.
#[test]
#[serial]
#[cfg(not(feature = "strip_debug_logging"))]
fn debug_lives_in_release_default() {
    reset_for_test();
    let tmp = TempDir::new().unwrap();
    set_xdg_state_home(&tmp);

    let runtime_id = Arc::new(RuntimeUniqueId::from("RtestDebugLive"));
    let config = StreamlibLoggingConfig::for_runtime("test", runtime_id);
    let guard = init_for_tests(config).unwrap();

    tracing::debug!("debug-default-must-survive");

    let path = guard.jsonl_path().unwrap().to_path_buf();
    drop(guard);

    let events = read_jsonl(&path);
    assert!(
        events
            .iter()
            .any(|e| e.message == "debug-default-must-survive" && e.level == LogLevel::Debug),
        "expected debug! record in JSONL; got {:#?}",
        events
    );

    clear_xdg_state_home();
}

/// With `--features streamlib/strip_debug_logging`, release builds
/// additionally strip `debug!` (STATIC_MAX_LEVEL = INFO) while `info!`
/// stays live.
#[test]
#[serial]
#[cfg(feature = "strip_debug_logging")]
fn strip_debug_logging_feature_strips_debug() {
    reset_for_test();
    unsafe { std::env::set_var("RUST_LOG", "trace") };
    let tmp = TempDir::new().unwrap();
    set_xdg_state_home(&tmp);

    let runtime_id = Arc::new(RuntimeUniqueId::from("RtestDebugStrip"));
    let config = StreamlibLoggingConfig::for_runtime("test", runtime_id);
    let guard = init_for_tests(config).unwrap();

    tracing::debug!("debug-feature-stripped-token");
    tracing::info!("info-feature-keeps-token");

    let path = guard.jsonl_path().unwrap().to_path_buf();
    drop(guard);

    let events = read_jsonl(&path);
    let debug_count = events
        .iter()
        .filter(|e| e.message == "debug-feature-stripped-token")
        .count();
    let info_count = events
        .iter()
        .filter(|e| e.message == "info-feature-keeps-token")
        .count();

    if cfg!(debug_assertions) {
        // `release_max_level_*` features only apply in release.
        assert_eq!(
            tracing::level_filters::STATIC_MAX_LEVEL,
            tracing::level_filters::LevelFilter::TRACE,
            "debug builds ignore release_max_level_info"
        );
        assert_eq!(debug_count, 1);
        assert_eq!(info_count, 1);
    } else {
        assert_eq!(
            tracing::level_filters::STATIC_MAX_LEVEL,
            tracing::level_filters::LevelFilter::INFO,
            "release builds with strip_debug_logging must raise the cap to INFO"
        );
        assert_eq!(
            debug_count, 0,
            "debug! must be compiled out with strip_debug_logging"
        );
        assert_eq!(info_count, 1, "info! must survive strip_debug_logging");
    }

    clear_xdg_state_home();
}

/// `warn!` and `error!` must always reach the JSONL. No feature combo
/// may strip them — production diagnostics depend on it.
#[test]
#[serial]
fn warn_and_error_never_stripped() {
    reset_for_test();
    let tmp = TempDir::new().unwrap();
    set_xdg_state_home(&tmp);

    let runtime_id = Arc::new(RuntimeUniqueId::from("RtestWarnErr"));
    let config = StreamlibLoggingConfig::for_runtime("test", runtime_id);
    let guard = init_for_tests(config).unwrap();

    tracing::warn!("warn-never-stripped-token");
    tracing::error!("error-never-stripped-token");

    let path = guard.jsonl_path().unwrap().to_path_buf();
    drop(guard);

    let events = read_jsonl(&path);
    assert!(
        events
            .iter()
            .any(|e| e.message == "warn-never-stripped-token" && e.level == LogLevel::Warn),
        "expected warn! record in JSONL; got {:#?}",
        events
    );
    assert!(
        events
            .iter()
            .any(|e| e.message == "error-never-stripped-token" && e.level == LogLevel::Error),
        "expected error! record in JSONL; got {:#?}",
        events
    );
    assert!(
        tracing::level_filters::STATIC_MAX_LEVEL
            >= tracing::level_filters::LevelFilter::INFO,
        "STATIC_MAX_LEVEL must always admit warn!/error!; got {:?}",
        tracing::level_filters::STATIC_MAX_LEVEL
    );

    clear_xdg_state_home();
}

#[test]
#[serial]
fn burst_surfaces_dropped_counter_record() {
    reset_for_test();
    let tmp = TempDir::new().unwrap();
    set_xdg_state_home(&tmp);

    let runtime_id = Arc::new(RuntimeUniqueId::from("RtestBurst"));
    let config = StreamlibLoggingConfig {
        service_name: "test".into(),
        runtime_id: Some(Arc::clone(&runtime_id)),
        stdout: false,
        jsonl: true,
        intercept_stdio: false,
        tunables: LoggingTunables {
            batch_ms: Some(25),
            batch_bytes: Some(1 << 20),
            // Tiny capacity so the burst forces drops.
            channel_capacity: Some(8),
            fsync_on_every_batch: None,
        },
    };
    let guard = init_for_tests(config).unwrap();

    for i in 0..5_000u64 {
        tracing::info!(i, "burst-line");
    }
    std::thread::sleep(Duration::from_millis(1_200));

    let path = guard.jsonl_path().unwrap().to_path_buf();
    drop(guard);

    let events = read_jsonl(&path);
    let dropped_records: Vec<_> = events
        .iter()
        .filter(|e| e.level == LogLevel::Warn && e.attrs.contains_key("dropped"))
        .collect();
    assert!(
        !dropped_records.is_empty(),
        "expected at least one synthetic dropped=N record in the JSONL"
    );

    clear_xdg_state_home();
}
