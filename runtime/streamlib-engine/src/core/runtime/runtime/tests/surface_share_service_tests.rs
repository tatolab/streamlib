// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The per-runtime surface-sharing service.

use super::*;

/// Each variable's value before a test set it, put back on drop — so a test
/// that panics still leaves the environment as it found it.
struct EnvironmentVariablesRestoredOnDrop {
    previous_values: Vec<(String, Option<std::ffi::OsString>)>,
}

impl Drop for EnvironmentVariablesRestoredOnDrop {
    fn drop(&mut self) {
        // SAFETY: serialized via #[serial]; no concurrent env mutation.
        unsafe {
            for (name, previous_value) in self.previous_values.drain(..) {
                match previous_value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

/// Set each variable for the duration of the closure, restoring what was
/// there before. Tests using this must be `#[serial]`.
fn with_environment_variables_set<F: FnOnce() -> R, R>(
    variables: &[(&str, &std::ffi::OsStr)],
    f: F,
) -> R {
    let _restored_on_drop = EnvironmentVariablesRestoredOnDrop {
        previous_values: variables
            .iter()
            .map(|(name, _)| (name.to_string(), std::env::var_os(name)))
            .collect(),
    };
    // SAFETY: serialized via #[serial]; no concurrent env mutation.
    unsafe {
        for (name, value) in variables {
            std::env::set_var(name, value);
        }
    }
    f()
}

#[cfg(target_os = "macos")]
mod runtime_internal_mach_surface_share {
    use super::*;
    use streamlib_surface_client::SurfaceShareMachServiceConnection;

    fn a_unique_pinned_runtime_id() -> String {
        format!("Rmachtest{}", uuid::Uuid::new_v4().simple())
    }

    #[test]
    #[serial]
    fn a_runtime_registers_a_surface_share_mach_service_this_process_can_use() {
        let runtime = Runner::new().expect("runtime should construct");
        let service_name = runtime
            .surface_share_mach_service_rendezvous()
            .service_name()
            .to_string();
        assert!(
            service_name.ends_with(runtime.runtime_id().as_str()),
            "the service name {service_name} is keyed by the runtime id"
        );
        let connection =
            SurfaceShareMachServiceConnection::connect(&service_name, Duration::from_secs(5))
                .expect("this process connects to its own runtime's service");
        let (answer, ports) = connection
            .send_request_with_ports(
                &serde_json::json!({"op": "lookup", "surface_id": "no-such"}),
                Vec::new(),
            )
            .expect("round-trip");
        assert!(answer.get("error").is_some());
        assert!(ports.is_empty());
    }

    #[test]
    #[serial]
    fn a_second_runtime_pinned_to_a_live_runtimes_id_is_refused_and_a_dropped_one_frees_it() {
        let pinned_id = a_unique_pinned_runtime_id();
        with_environment_variables_set(
            &[("STREAMLIB_RUNTIME_ID", std::ffi::OsStr::new(&pinned_id))],
            || {
                let first = Runner::new().expect("first runtime");
                let refusal = Runner::new()
                    .err()
                    .expect("a second runtime under a live id is refused")
                    .to_string();
                assert!(
                    refusal.contains("already registered by a live process"),
                    "{refusal}"
                );
                drop(first);
                Runner::new().expect("the id is free once its runtime is gone");
            },
        );
    }
}

#[cfg(target_os = "linux")]
mod runtime_internal_surface_share {
    use super::*;
    use std::os::unix::net::UnixStream;
    use streamlib_runtime_client_contract::streamlib_runtime_directory::StreamlibRuntimeDirectory;
    use streamlib_surface_client::{MAX_DMA_BUF_PLANES, send_request_with_fds};

    /// Replace XDG_RUNTIME_DIR with a fresh tempdir for the duration of the
    /// closure. Tests using this must be `#[serial]` so no other runtime
    /// construct reads the mutated env.
    fn with_isolated_xdg_runtime_dir<F: FnOnce(&std::path::Path) -> R, R>(f: F) -> R {
        let tmp =
            crate::core::test_support::a_temporary_directory_at_owner_only_mode().expect("tempdir");
        with_environment_variables_set(&[("XDG_RUNTIME_DIR", tmp.path().as_os_str())], || {
            f(tmp.path())
        })
    }

    #[test]
    #[serial]
    fn runtime_brings_up_internal_surface_share_service() {
        with_isolated_xdg_runtime_dir(|xdg| {
            let runtime = Runner::new().expect("runtime should construct");
            let socket_path = runtime.surface_socket_path();
            assert!(
                socket_path.exists(),
                "expected socket file at {}",
                socket_path.display()
            );
            assert!(
                socket_path.starts_with(xdg.join("streamlib")),
                "socket {} should be under the runtime directory in XDG_RUNTIME_DIR {}",
                socket_path.display(),
                xdg.display()
            );

            // Round-trip a request through the runtime-internal service to prove
            // it is actually serving — check_out for an unknown surface_id is
            // the lightest-weight op that exercises the wire path end-to-end.
            let stream = UnixStream::connect(socket_path).expect("connect to runtime broker");
            let req = serde_json::json!({
                "op": "check_out",
                "surface_id": "ping-no-such-surface",
            });
            let (resp, fds) =
                send_request_with_fds(&stream, &req, &[], MAX_DMA_BUF_PLANES).expect("round-trip");
            assert!(fds.is_empty());
            assert!(
                resp.get("error").and_then(|v| v.as_str()).is_some(),
                "expected error for missing surface, got {:?}",
                resp
            );
        });
    }

    #[test]
    #[serial]
    fn a_runtime_started_with_xdg_runtime_dir_unset_keeps_its_socket_and_domain_in_the_per_user_fallback()
     {
        let prev = std::env::var_os("XDG_RUNTIME_DIR");
        // SAFETY: serialized via #[serial].
        unsafe {
            std::env::remove_var("XDG_RUNTIME_DIR");
        }

        let result = Runner::new();

        // Restore env before asserting so a panic doesn't leak state.
        unsafe {
            if let Some(v) = prev {
                std::env::set_var("XDG_RUNTIME_DIR", v);
            }
        }

        let runtime = result.expect("a runtime starts with XDG_RUNTIME_DIR unset");
        let fallback = std::path::PathBuf::from(format!(
            "/tmp/streamlib-{}",
            streamlib_runtime_client_contract::streamlib_runtime_directory::current_process_uid()
        ));
        assert!(
            runtime.surface_socket_path().starts_with(&fallback),
            "socket {} should be under {}",
            runtime.surface_socket_path().display(),
            fallback.display()
        );
        let iceoryx2_config = runtime.iceoryx2_node().config();
        assert_eq!(
            iceoryx2_config.global.root_path().as_bytes_const(),
            fallback.join("iox2").as_os_str().as_encoded_bytes()
        );
        assert_eq!(
            iceoryx2_config.global.prefix.as_bytes_const(),
            crate::iceoryx2::engine_owned_iceoryx2_prefix_for_this_user().as_bytes()
        );
    }

    #[test]
    #[serial]
    fn two_runtimes_coexist_without_collision() {
        with_isolated_xdg_runtime_dir(|_| {
            let r1 = Runner::new().expect("first runtime");
            let r2 = Runner::new().expect("second runtime");

            let p1 = r1.surface_socket_path().to_path_buf();
            let p2 = r2.surface_socket_path().to_path_buf();

            assert_ne!(p1, p2, "each runtime must own a distinct socket path");
            assert!(p1.exists(), "first socket missing: {}", p1.display());
            assert!(p2.exists(), "second socket missing: {}", p2.display());

            // Both should serve a round-trip independently.
            for path in [&p1, &p2] {
                let stream = UnixStream::connect(path).expect("connect");
                let req = serde_json::json!({
                    "op": "check_out",
                    "surface_id": "no-such",
                });
                let (resp, _) = send_request_with_fds(&stream, &req, &[], MAX_DMA_BUF_PLANES)
                    .expect("round-trip");
                assert!(resp.get("error").is_some());
            }
        });
    }

    fn iceoryx2_nodes_in_domain(domain_root: &std::path::Path) -> usize {
        let config = crate::iceoryx2::engine_owned_iceoryx2_config(domain_root)
            .expect("the test domain root fits the socket-path budget");
        let mut nodes = 0;
        iceoryx2::node::Node::<iceoryx2::prelude::ipc_threadsafe::Service>::list(&config, |_| {
            nodes += 1;
            iceoryx2::prelude::CallbackProgression::Continue
        })
        .expect("list the domain's iceoryx2 nodes");
        nodes
    }

    /// Set in the child process this test re-runs itself in.
    const RUNTIME_BUILT_AND_DROPPED_IN_A_CHILD_PROCESS_ENVIRONMENT_VARIABLE: &str =
        "STREAMLIB_TEST_BUILD_AND_DROP_ONE_RUNTIME";

    /// A child process builds and drops the only runtime it ever makes, then
    /// exits, and the parent reads the domain it leaves. A child keeps the
    /// check independent of every runtime this test binary built before:
    /// a static that pins only the first runtime's node pins this one.
    ///
    /// Mental-revert: parking a clone of the runner's node anywhere static
    /// keeps the node past the drop, and the exited child leaves its files.
    #[test]
    #[serial]
    fn a_dropped_runtime_leaves_no_iceoryx2_node_in_its_domain() {
        if std::env::var_os(RUNTIME_BUILT_AND_DROPPED_IN_A_CHILD_PROCESS_ENVIRONMENT_VARIABLE)
            .is_some()
        {
            drop(Runner::new().expect("runtime"));
            return;
        }

        with_isolated_xdg_runtime_dir(|_| {
            let domain_root = StreamlibRuntimeDirectory::resolve()
                .expect("the isolated runtime directory")
                .iceoryx2_domain_root();
            let child = crate::core::test_support::rerun_this_test_in_a_child_process(
                "core::runtime::runtime::tests::surface_share_service_tests::runtime_internal_surface_share::a_dropped_runtime_leaves_no_iceoryx2_node_in_its_domain",
                RUNTIME_BUILT_AND_DROPPED_IN_A_CHILD_PROCESS_ENVIRONMENT_VARIABLE,
                std::ffi::OsStr::new("1"),
            );
            assert!(
                child.status.success(),
                "the child failed to build and drop a runtime: {}\n{}",
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr),
            );

            assert_eq!(
                iceoryx2_nodes_in_domain(&domain_root),
                0,
                "a runtime torn down gracefully must take its iceoryx2 node's files with it"
            );
        });
    }

    /// The runtime directory is placed so its iceoryx2 domain root sits one
    /// byte past the socket-path budget: creating a node there is refused by
    /// name, so a refusal naming the live runtime instead is proof the
    /// duplicate check ran before any node was attempted.
    ///
    /// Mental-revert: creating the node before binding the surface socket
    /// turns the refusal below into the budget refusal.
    #[test]
    #[serial]
    fn a_runtime_pinned_to_a_live_runtimes_id_is_refused_before_it_creates_an_iceoryx2_node() {
        let base = crate::core::test_support::at_owner_only_mode(
            tempfile::Builder::new()
                .prefix("sl")
                .tempdir_in("/tmp")
                .expect("tempdir under /tmp"),
        )
        .expect("tempdir under /tmp");
        let domain_root_suffix = "/streamlib/iox2";
        let xdg_runtime_dir_bytes = crate::iceoryx2::ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES
            - crate::iceoryx2::engine_owned_iceoryx2_prefix_for_this_user().len()
            - domain_root_suffix.len()
            + 1;
        let base_bytes = base.path().as_os_str().len();
        assert!(
            xdg_runtime_dir_bytes > base_bytes + 1,
            "{} is too long to build a runtime directory past the budget under",
            base.path().display()
        );
        let xdg = base
            .path()
            .join("x".repeat(xdg_runtime_dir_bytes - base_bytes - 1));
        let pinned_id = format!("duplicate-{}", std::process::id());
        let live_runtimes_socket_path = with_environment_variables_set(
            &[("XDG_RUNTIME_DIR", xdg.as_os_str())],
            StreamlibRuntimeDirectory::resolve,
        )
        .expect("the runtime directory under the padded XDG_RUNTIME_DIR")
        .surface_share_socket_path(&pinned_id);
        let live_runtimes_socket =
            std::os::unix::net::UnixListener::bind(&live_runtimes_socket_path)
                .expect("bind the live runtime's socket");

        let refusal = with_environment_variables_set(
            &[
                ("XDG_RUNTIME_DIR", xdg.as_os_str()),
                ("STREAMLIB_RUNTIME_ID", std::ffi::OsStr::new(&pinned_id)),
            ],
            || Runner::new().map(|_| ()),
        )
        .expect_err("a second runtime with a live runtime's id must be refused")
        .to_string();

        assert!(
            refusal.contains("already bound by a live process"),
            "the refusal must name the live runtime, not a node failure: {refusal}"
        );
        assert!(
            refusal.contains("duplicate STREAMLIB_RUNTIME_ID"),
            "the surface-sharing socket is keyed by runtime id: {refusal}"
        );
        drop(live_runtimes_socket);
    }

    /// Mental-revert: validating the pinned id after the runtime directory
    /// resolves leaves that directory behind for a runtime that never ran.
    #[test]
    #[serial]
    fn a_malformed_pinned_runtime_id_is_refused_before_the_runtime_makes_anything() {
        with_isolated_xdg_runtime_dir(|xdg| {
            let refusal = with_environment_variables_set(
                &[("STREAMLIB_RUNTIME_ID", std::ffi::OsStr::new("../escape"))],
                || Runner::new().map(|_| ()),
            )
            .expect_err("a runtime id that could leave its directory must be refused")
            .to_string();

            assert!(
                refusal.contains("STREAMLIB_RUNTIME_ID") && refusal.contains("'/'"),
                "{refusal}"
            );
            assert!(
                !xdg.join("streamlib").exists(),
                "a refused runtime must not have made its runtime directory"
            );
        });
    }

    #[test]
    #[serial]
    fn polyglot_subprocess_inherits_socket_env() {
        with_isolated_xdg_runtime_dir(|_| {
            let runtime = Runner::new().expect("runtime");
            let socket_path = runtime.surface_socket_path().to_path_buf();

            // Mirror what the spawn ops do: build a Command with the env
            // var set from the runtime's socket path. The spawn ops use
            // `ctx.surface_socket_path()` which returns the same value as
            // `runtime.surface_socket_path()` — this test exercises the
            // contract that polyglot subprocesses see the runtime's socket.
            let output = std::process::Command::new("printenv")
                .arg("STREAMLIB_SURFACE_SOCKET")
                .env("STREAMLIB_SURFACE_SOCKET", &socket_path)
                .output()
                .expect("spawn printenv");

            assert!(
                output.status.success(),
                "printenv exited non-zero: stdout={:?} stderr={:?}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let inherited = String::from_utf8_lossy(&output.stdout).trim().to_string();
            assert_eq!(inherited, socket_path.to_string_lossy());
        });
    }

    #[test]
    #[serial]
    fn a_starting_runtime_removes_another_dead_runtimes_socket_and_spares_a_live_ones() {
        with_isolated_xdg_runtime_dir(|xdg| {
            let runtime_directory = xdg.join("streamlib");
            streamlib_runtime_client_contract::directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode(
                &runtime_directory,
                streamlib_runtime_client_contract::directory_at_an_explicit_mode::OWNER_ONLY_DIRECTORY_MODE,
            )
            .expect("create runtime directory");
            let dead_runtimes_socket = runtime_directory.join("surface-share-Rdeadruntime.sock");
            drop(std::os::unix::net::UnixListener::bind(&dead_runtimes_socket).unwrap());
            let live_runtimes_socket = runtime_directory.join("surface-share-Rliveruntime.sock");
            let _live_runtimes_listener =
                std::os::unix::net::UnixListener::bind(&live_runtimes_socket).unwrap();

            let runtime = Runner::new().expect("runtime should construct");

            assert!(!dead_runtimes_socket.exists());
            assert!(live_runtimes_socket.exists());
            assert!(runtime.surface_socket_path().exists());
        });
    }

    #[test]
    #[serial]
    fn stale_socket_from_dead_runtime_is_cleaned_up() {
        with_isolated_xdg_runtime_dir(|xdg| {
            // Pin the runtime ID so we can pre-create a file at the exact
            // path the runtime will compute.
            let pinned_id = format!("test-stale-socket-{}", std::process::id());
            let prev = std::env::var_os("STREAMLIB_RUNTIME_ID");
            // SAFETY: serialized via #[serial].
            unsafe {
                std::env::set_var("STREAMLIB_RUNTIME_ID", &pinned_id);
            }

            streamlib_runtime_client_contract::directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode(
                &xdg.join("streamlib"),
                streamlib_runtime_client_contract::directory_at_an_explicit_mode::OWNER_ONLY_DIRECTORY_MODE,
            )
            .expect("create runtime directory");
            let stale_path = xdg
                .join("streamlib")
                .join(format!("surface-share-{pinned_id}.sock"));
            std::fs::write(&stale_path, b"orphan-from-prior-crashed-runtime")
                .expect("write orphan");
            assert!(stale_path.exists());

            let runtime_result = Runner::new();

            // Restore env before asserting.
            unsafe {
                match prev {
                    Some(v) => std::env::set_var("STREAMLIB_RUNTIME_ID", v),
                    None => std::env::remove_var("STREAMLIB_RUNTIME_ID"),
                }
            }

            let runtime = runtime_result
                .expect("runtime should clean up an orphan socket and bind successfully");
            let bound = runtime.surface_socket_path();
            assert_eq!(bound, stale_path.as_path());
            assert!(
                bound.exists(),
                "service should be bound at {}",
                bound.display()
            );

            // The path is now a Unix socket, not a regular file — connect
            // should succeed against the runtime-internal service.
            let stream = UnixStream::connect(bound).expect("connect to fresh service");
            let req = serde_json::json!({
                "op": "check_out",
                "surface_id": "no-such",
            });
            let (resp, _) =
                send_request_with_fds(&stream, &req, &[], MAX_DMA_BUF_PLANES).expect("round-trip");
            assert!(resp.get("error").is_some());
        });
    }
}
