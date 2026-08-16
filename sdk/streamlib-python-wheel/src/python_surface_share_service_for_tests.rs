// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A real surface-share service, for tests that have to prove a claim.
//!
//! The lease is the whole lifetime contract, and the wheel's device tests are
//! `requires_gpu` while CI declares no GPU runner — so anything about leases
//! that is not provable here is not protected anywhere. The service is the real
//! one; only the surface behind it is a stand-in, a sized memfd, because a
//! claim is bookkeeping over an id and never touches the memory.

use std::path::PathBuf;
use std::sync::Arc;

use streamlib::sdk::context::SurfaceCheckOutLeaseRegistry;
use streamlib::sdk::engine::linux_surface_share::{SurfaceShareState, UnixSocketSurfaceService};

/// A running service, and the lease table the pool reads to decide whether a
/// slot may be rehanded.
pub(crate) struct SurfaceShareUnderTest {
    _service: UnixSocketSurfaceService,
    pub(crate) socket_path: PathBuf,
    check_out_leases: Arc<SurfaceCheckOutLeaseRegistry>,
    socket_directory: PathBuf,
}

impl Drop for SurfaceShareUnderTest {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.socket_directory);
    }
}

impl SurfaceShareUnderTest {
    /// Start a service on a socket of this test's own.
    pub(crate) fn start(label: &str) -> Self {
        let socket_directory = Self::a_directory_short_enough_for_a_unix_socket(label);
        let _ = std::fs::remove_dir_all(&socket_directory);
        std::fs::create_dir_all(&socket_directory).expect("a directory for the test socket");
        let socket_path = socket_directory.join("s.sock");

        let state = SurfaceShareState::new();
        let check_out_leases = Arc::clone(state.check_out_leases());
        let mut service = UnixSocketSurfaceService::new(state, socket_path.clone());
        service.start().expect("the surface-share service starts");
        std::thread::sleep(std::time::Duration::from_millis(50));

        Self {
            _service: service,
            socket_path,
            check_out_leases,
            socket_directory,
        }
    }

    /// A directory whose socket path fits in `sun_path`.
    ///
    /// A unix socket address is 108 bytes including the terminator, and it is
    /// the *path* that has to fit, not the filename — so a build box with a
    /// long `TMPDIR` (a CI workspace, a sandbox) makes `bind` fail for a reason
    /// that looks nothing like the cause. `/tmp` is the fallback because this
    /// module is Linux-only and every Linux box has one.
    fn a_directory_short_enough_for_a_unix_socket(label: &str) -> PathBuf {
        const SUN_PATH_CAPACITY: usize = 108;
        let leaf = format!("sl-ss-{}-{}", label, std::process::id());
        for root in [std::env::temp_dir(), PathBuf::from("/tmp")] {
            let candidate = root.join(&leaf);
            if candidate.join("s.sock").as_os_str().len() < SUN_PATH_CAPACITY {
                return candidate;
            }
        }
        panic!("no temporary directory leaves room for a {SUN_PATH_CAPACITY}-byte socket path");
    }

    /// Publish one surface and return the id it lives under.
    pub(crate) fn publish_one_surface(&self) -> String {
        use std::os::unix::io::{FromRawFd as _, IntoRawFd as _};

        let name = std::ffi::CString::new("streamlib-lease-debt-test").unwrap();
        // SAFETY: an ordinary libc call taking a NUL-terminated name; the fd it
        // answers with is adopted immediately below.
        let raw_fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
        assert!(raw_fd >= 0, "memfd_create failed");
        // SAFETY: adopting the fd `memfd_create` just returned; nothing else
        // holds it.
        let backing = unsafe { std::fs::File::from_raw_fd(raw_fd) };
        backing.set_len(4096).expect("size the backing memfd");
        let backing_fd = backing.into_raw_fd();

        let publisher =
            streamlib_surface_client::connect_to_surface_share_socket(&self.socket_path)
                .expect("a publisher connection");
        let (response, _no_reply_fds) = streamlib_surface_client::send_request_with_fds(
            &publisher,
            &serde_json::json!({
                "op": "check_in",
                "runtime_id": "lease-debt-test-runtime",
                "width": 32,
                "height": 32,
                "format": "bgra32",
                "resource_type": "pixel_buffer",
            }),
            &[backing_fd],
            0,
        )
        .expect("check_in");
        // SAFETY: the service dup'd this fd over SCM_RIGHTS; this side's copy
        // is ours to close.
        unsafe { libc::close(backing_fd) };
        // Held open deliberately: closing it would take the surface with it.
        std::mem::forget(publisher);
        response
            .get("surface_id")
            .and_then(|value| value.as_str())
            .expect("the service minted a surface id")
            .to_string()
    }

    /// How many claims are outstanding on `surface_id` — what the pool asks
    /// before it rehands a slot.
    pub(crate) fn outstanding_claims_on(&self, surface_id: &str) -> u32 {
        self.check_out_leases
            .outstanding_check_out_count(surface_id)
            .expect("the lease table stays readable")
    }

    /// Register `slot_id` as a pool-slot surface and publish `frame_generation`
    /// as its current frame — the state a pool producer's acquire leaves
    /// behind, without needing a pool or a GPU.
    pub(crate) fn publish_pool_slot_frame(&self, slot_id: &str, frame_generation: u64) {
        use std::os::unix::io::{FromRawFd as _, IntoRawFd as _};

        if self
            .check_out_leases
            .current_frame_generation(slot_id)
            .expect("the lease table stays readable")
            .is_none()
        {
            let name = std::ffi::CString::new("streamlib-frame-generation-test").unwrap();
            // SAFETY: an ordinary libc call taking a NUL-terminated name; the
            // fd it answers with is adopted immediately below.
            let raw_fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
            assert!(raw_fd >= 0, "memfd_create failed");
            // SAFETY: adopting the fd `memfd_create` just returned; nothing
            // else holds it.
            let backing = unsafe { std::fs::File::from_raw_fd(raw_fd) };
            backing.set_len(4096).expect("size the backing memfd");
            let backing_fd = backing.into_raw_fd();

            let publisher =
                streamlib_surface_client::connect_to_surface_share_socket(&self.socket_path)
                    .expect("a publisher connection");
            let (response, _no_reply_fds) = streamlib_surface_client::send_request_with_fds(
                &publisher,
                &serde_json::json!({
                    "op": "register",
                    "surface_id": slot_id,
                    "runtime_id": "frame-generation-test-runtime",
                    "width": 32,
                    "height": 32,
                    "format": "bgra32",
                    "resource_type": "pixel_buffer",
                }),
                &[backing_fd],
                0,
            )
            .expect("register");
            // SAFETY: the service dup'd this fd over SCM_RIGHTS; this side's
            // copy is ours to close.
            unsafe { libc::close(backing_fd) };
            // Held open deliberately: closing it would take the surface with it.
            std::mem::forget(publisher);
            assert_eq!(
                response.get("success").and_then(serde_json::Value::as_bool),
                Some(true),
                "the slot registration must succeed: {response:?}"
            );
        }

        self.check_out_leases
            .publish_frame_generation(slot_id, frame_generation)
            .expect("the lease table stays readable");
    }
}
