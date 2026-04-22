// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Daemon-lifecycle integration tests for the Linux broker binary.
//!
//! These tests spawn the built `streamlib-broker` binary out of
//! `target/debug` (located via `env!("CARGO_BIN_EXE_streamlib-broker")`),
//! exercise it as a client over its Unix-socket protocol, and prove the
//! lifecycle guarantees the daemon advertises: stale-socket cleanup on
//! restart, systemd socket-activation cold start via `LISTEN_FDS=1` +
//! inherited fd 3, `--idle-exit-seconds` auto-exit and subsequent
//! re-activation, dead-runtime pruning, and the shape of the shipped
//! systemd units.

#![cfg(target_os = "linux")]

use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BROKER_BIN: &str = env!("CARGO_BIN_EXE_streamlib-broker");

/// Build a unique per-test temp directory plus child paths for the
/// socket + log + pid. Monotonic nanos keep parallel test runs from
/// colliding on a shared filesystem.
struct TestEnv {
    dir: PathBuf,
    socket: PathBuf,
    log: PathBuf,
}

impl TestEnv {
    fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "streamlib-broker-lifecycle-{}-{}-{}",
            label,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).expect("create test dir");
        let socket = dir.join("broker.sock");
        let log = dir.join("broker.log");
        Self { dir, socket, log }
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Spawn the broker binary in the conventional `--socket-path` mode (no
/// socket activation). Stdout+stderr are redirected to `env.log`.
fn spawn_bound_broker(env: &TestEnv, extra_args: &[&str]) -> Child {
    let log = std::fs::File::create(&env.log).expect("create log");
    let err = log.try_clone().expect("clone log");
    let mut cmd = Command::new(BROKER_BIN);
    cmd.arg("--port")
        .arg(unique_port_arg())
        .arg("--socket-path")
        .arg(&env.socket)
        .args(extra_args)
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err));
    // Don't inherit LISTEN_FDS from the parent (the test harness might set
    // it for a different test).
    cmd.env_remove("LISTEN_FDS").env_remove("LISTEN_PID");
    cmd.spawn().expect("spawn broker")
}

/// Spawn the broker binary with a pre-bound `UnixListener` handed over as
/// fd 3 + `LISTEN_FDS=1`, mimicking what systemd does for a socket-
/// activated service. Returns the child handle; the caller owns the
/// listener and is responsible for keeping it alive across restarts.
fn spawn_activated_broker(env: &TestEnv, listener: &UnixListener, extra_args: &[&str]) -> Child {
    let log = std::fs::File::create(&env.log).expect("create log");
    let err = log.try_clone().expect("clone log");
    let listener_fd = listener.as_raw_fd();
    let mut cmd = Command::new(BROKER_BIN);
    cmd.arg("--port")
        .arg(unique_port_arg())
        // --socket-path is ignored when LISTEN_FDS is set — pass it
        // anyway so the binary doesn't complain about missing flags.
        .arg("--socket-path")
        .arg(&env.socket)
        .args(extra_args)
        .env("LISTEN_FDS", "1")
        // LISTEN_PID deliberately omitted — `sd_listen_fd` accepts the
        // absence and skips the pid check. Setting it here would require
        // knowing the child's pid before exec, which is awkward.
        .env_remove("LISTEN_PID")
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err));
    unsafe {
        cmd.pre_exec(move || {
            // Move the listener to fd 3. In the common case dup2
            // clears CLOEXEC on the destination, but when source ==
            // destination POSIX treats dup2 as a no-op and leaves
            // flags alone — so the original fd's CLOEXEC would still
            // be set and exec would close it, leaving the broker with
            // no inherited listener. Always clear CLOEXEC explicitly.
            if libc::dup2(listener_fd, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let flags = libc::fcntl(3, libc::F_GETFD);
            if flags < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().expect("spawn broker (activated)")
}

/// Return a port string that changes per invocation to avoid collisions
/// between parallel tests. The gRPC port is bound by the broker; tests
/// don't talk to it but two daemons cannot both own the same port.
fn unique_port_arg() -> String {
    use std::sync::atomic::{AtomicU16, Ordering};
    static NEXT: AtomicU16 = AtomicU16::new(51000);
    let port = NEXT.fetch_add(1, Ordering::Relaxed);
    port.to_string()
}

/// Poll the broker with the binary's `--probe` subcommand until it
/// responds, or the deadline expires. Returns true on success.
fn wait_until_ready(socket: &Path, deadline: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        let status = Command::new(BROKER_BIN)
            .arg("--probe")
            .arg(socket)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Child inherits the parent's env; strip LISTEN_FDS so the
            // probe subprocess doesn't try to inherit anything.
            .env_remove("LISTEN_FDS")
            .env_remove("LISTEN_PID")
            .status();
        if matches!(status, Ok(s) if s.success()) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Kill the child (SIGTERM, then SIGKILL after 3s) and reap. Idempotent.
fn stop_broker(mut child: Child) {
    let pid = child.id() as i32;
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Baseline: the daemon starts, binds a socket, and accepts a ping from
/// a client. If this breaks, every other test in this file is suspect.
#[test]
fn daemon_starts_and_accepts_client() {
    let env = TestEnv::new("starts");
    let child = spawn_bound_broker(&env, &[]);
    assert!(
        wait_until_ready(&env.socket, Duration::from_secs(10)),
        "broker did not become ready; log at {:?}",
        env.log
    );
    stop_broker(child);
}

/// A stale socket file left by a previous crash must not block the next
/// start. The bind-path start() does `remove_file` before `bind` when a
/// stale file is present.
#[test]
fn daemon_cleans_up_stale_socket_on_restart() {
    let env = TestEnv::new("stale-socket");
    // Plant a stale file that looks like a socket but isn't bound to
    // anything. A previous daemon that was SIGKILLed leaves this shape.
    std::fs::write(&env.socket, b"stale").expect("plant stale file");
    assert!(env.socket.exists());

    let child = spawn_bound_broker(&env, &[]);
    assert!(
        wait_until_ready(&env.socket, Duration::from_secs(10)),
        "broker did not replace stale socket; log at {:?}",
        env.log
    );
    stop_broker(child);
}

/// Simulate systemd socket activation: pre-bind a UnixListener in this
/// process, hand it to the daemon as fd 3 with `LISTEN_FDS=1`, and
/// confirm the client can round-trip a ping through the inherited
/// listener — AND that the inherited listener is actually what's
/// serving the ping, not a replacement socket bound by the fallback
/// path.
///
/// The invariant is checked by inode equality: the bind-path start()
/// `remove_file`s the socket before `bind`ing, so a broker that falls
/// back to the bind path creates a new filesystem entry with a new
/// inode. A broker that honors `LISTEN_FDS` leaves the pre-bound
/// socket file untouched and reuses its inode.
#[test]
fn socket_activation_cold_start() {
    use std::os::unix::fs::MetadataExt;

    let env = TestEnv::new("activation-cold");
    let listener = UnixListener::bind(&env.socket).expect("pre-bind listener");
    let inode_before = std::fs::metadata(&env.socket)
        .expect("stat pre-bind socket")
        .ino();

    let child = spawn_activated_broker(&env, &listener, &[]);
    assert!(
        wait_until_ready(&env.socket, Duration::from_secs(10)),
        "activated broker did not become ready; log at {:?}",
        env.log
    );

    let inode_after = std::fs::metadata(&env.socket)
        .expect("stat live socket")
        .ino();
    assert_eq!(
        inode_before, inode_after,
        "inherited listener must serve on the pre-bound socket inode; \
         got before={} after={} — a re-bind fallback would change this",
        inode_before, inode_after
    );

    stop_broker(child);

    // systemd would hold the socket through the restart — the parent
    // listener is still alive here, and the socket file is still on
    // disk.
    assert!(env.socket.exists());
    drop(listener);
    let _ = std::fs::remove_file(&env.socket);
}

/// `--idle-exit-seconds` makes the daemon self-terminate after N seconds
/// with no active connections; the test-harness listener (playing
/// systemd) survives, and a second daemon spawned against the same
/// listener serves the next client. This is the re-activation path.
#[test]
fn socket_activation_idle_exit_and_restart() {
    let env = TestEnv::new("idle-exit");
    let listener = UnixListener::bind(&env.socket).expect("pre-bind listener");

    // First daemon: --idle-exit-seconds=2. It should come up, then exit
    // by itself after ~2s of no clients.
    let mut child = spawn_activated_broker(&env, &listener, &["--idle-exit-seconds", "2"]);
    assert!(
        wait_until_ready(&env.socket, Duration::from_secs(10)),
        "first activated broker did not become ready; log at {:?}",
        env.log
    );

    // Wait for the idle-exit. The probe loop above counts as one client
    // round-trip per probe, so the idle clock effectively starts now.
    // Give it a generous 6s to exit.
    let exited_within = {
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => break Some(start.elapsed()),
                Ok(None) => {
                    if start.elapsed() > Duration::from_secs(8) {
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => panic!("try_wait failed: {}", e),
            }
        }
    };
    assert!(
        exited_within.is_some(),
        "broker did not self-exit after --idle-exit-seconds=2; log tail:\n{}",
        tail_log(&env.log)
    );

    // The listener (systemd's role) still holds the socket; a fresh
    // daemon spawned against it should serve a client again. This proves
    // the socket-activation re-spawn path works end-to-end.
    let child2 = spawn_activated_broker(&env, &listener, &[]);
    assert!(
        wait_until_ready(&env.socket, Duration::from_secs(10)),
        "restarted activated broker did not become ready; log at {:?}",
        env.log
    );
    stop_broker(child2);

    drop(listener);
    let _ = std::fs::remove_file(&env.socket);
}

/// When a runtime registers a surface and then dies, a later call to
/// `prune_dead_runtimes` on the BrokerState releases that runtime's
/// surfaces as part of the prune pass. Driven purely through the library
/// so the test doesn't need a daemon, but it's the behavior the daemon's
/// periodic prune loop relies on.
#[test]
fn prune_dead_runtimes_releases_orphaned_surfaces() {
    use streamlib_broker::BrokerState;

    let state = BrokerState::new();

    // Register a runtime with a pid that definitely isn't us and is
    // unlikely to be alive. Pid 1 is always alive on Linux (init/systemd),
    // so pick a "reserved" sentinel: we fork a short-lived child, reap
    // it, and use its pid. After waitpid, that pid is free to be
    // re-assigned but the short window is enough for the test.
    let dead_pid = {
        // SAFETY: fork in a test with no threads running in the
        // process — std's test harness runs tests in threads but the
        // fork is on the current thread only, and the child exits
        // immediately without doing work that would hit shared state.
        // This is the minimum-risk pattern — a kernel-assigned dead pid.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            // Child: exit immediately.
            unsafe { libc::_exit(0) };
        }
        // Parent: reap.
        let mut status: libc::c_int = 0;
        unsafe { libc::waitpid(pid, &mut status, 0) };
        pid
    };

    state.register_runtime_with_metadata("rt-dead", "dead-runtime", "", "", dead_pid);
    state.register_surface("orphan-1", "rt-dead", -1, 640, 480, "Rgba8Unorm", "texture");
    state.register_surface(
        "orphan-2",
        "rt-dead",
        -1,
        320,
        240,
        "Rgba8Unorm",
        "pixel_buffer",
    );
    assert_eq!(state.surface_count(), 2);
    assert_eq!(state.runtime_count(), 1);

    let pruned = state.prune_dead_runtimes();
    assert_eq!(pruned.len(), 1);
    assert_eq!(pruned[0], "dead-runtime");
    assert_eq!(
        state.surface_count(),
        0,
        "orphaned surfaces must be released when their runtime is pruned"
    );
}

/// Parse a systemd unit file into `(section, key, value)` directive
/// tuples. Comment lines (`#` or `;`) and blank lines are skipped, and
/// only content before an unquoted `#` on a directive line counts —
/// otherwise a comment containing `Accept=no` would falsely satisfy
/// the "has this directive" assertions below.
fn parse_unit_directives(contents: &str) -> Vec<(String, String, String)> {
    let mut section = String::new();
    let mut out = Vec::new();
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].to_string();
            continue;
        }
        // Strip an inline comment if present. systemd unit files permit
        // `Key=value # comment` but only before `=` parsing matters here.
        let effective = line.splitn(2, '#').next().unwrap_or(line).trim();
        if let Some((k, v)) = effective.split_once('=') {
            out.push((section.clone(), k.trim().to_string(), v.trim().to_string()));
        }
    }
    out
}

/// Parse-level invariants on the shipped systemd units. If these keys
/// drift silently a socket-activated install would quietly fail (broker
/// would block in bind(), or never get the inherited fd).
#[test]
fn systemd_units_have_required_fields() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate has a parent")
        .parent()
        .expect("workspace root exists")
        .to_path_buf();
    let socket_unit = repo_root.join("scripts/streamlib-broker.socket");
    let service_unit = repo_root.join("scripts/streamlib-broker.service");

    let socket = std::fs::read_to_string(&socket_unit)
        .unwrap_or_else(|e| panic!("read {:?} failed: {}", socket_unit, e));
    let service = std::fs::read_to_string(&service_unit)
        .unwrap_or_else(|e| panic!("read {:?} failed: {}", service_unit, e));

    let socket_dirs = parse_unit_directives(&socket);
    let service_dirs = parse_unit_directives(&service);

    let has_directive = |dirs: &[(String, String, String)], section: &str, key: &str| -> bool {
        dirs.iter().any(|(s, k, _)| s == section && k == key)
    };
    let directive_value =
        |dirs: &[(String, String, String)], section: &str, key: &str| -> Option<String> {
            dirs.iter()
                .find(|(s, k, _)| s == section && k == key)
                .map(|(_, _, v)| v.clone())
        };

    // .socket unit essentials
    assert!(
        has_directive(&socket_dirs, "Socket", "ListenStream"),
        "streamlib-broker.socket missing [Socket] ListenStream= — nothing to listen on"
    );
    assert_eq!(
        directive_value(&socket_dirs, "Socket", "Accept").as_deref(),
        Some("no"),
        "streamlib-broker.socket must set Accept=no — a per-connection \
         fork would break the persistent-state daemon"
    );

    // .service unit essentials
    let exec_start = directive_value(&service_dirs, "Service", "ExecStart").unwrap_or_default();
    assert!(
        !exec_start.is_empty(),
        "streamlib-broker.service missing [Service] ExecStart="
    );
    assert!(
        exec_start.contains("streamlib-broker"),
        "streamlib-broker.service's ExecStart= ({}) does not reference \
         the broker binary",
        exec_start
    );
    let binds_to_socket = service_dirs.iter().any(|(section, key, value)| {
        section == "Unit"
            && (key == "Requires" || key == "Also")
            && value
                .split_whitespace()
                .any(|v| v == "streamlib-broker.socket")
    });
    assert!(
        binds_to_socket,
        "streamlib-broker.service must set [Unit] Requires=streamlib-broker.socket \
         (or Also=) so systemd starts the socket unit alongside the service"
    );
}

fn tail_log(log: &Path) -> String {
    let mut buf = String::new();
    let _ = std::fs::File::open(log).and_then(|mut f| f.read_to_string(&mut buf));
    let lines: Vec<_> = buf.lines().collect();
    let start = lines.len().saturating_sub(30);
    lines[start..].join("\n")
}
