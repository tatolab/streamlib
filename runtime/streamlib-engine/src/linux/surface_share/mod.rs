// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Per-runtime surface-sharing service (Linux).
//!
//! Each `Runner` owns one [`UnixSocketSurfaceService`] listening on a
//! unique Unix socket in the runtime directory. Polyglot subprocesses
//! spawned by the runtime receive the socket path through
//! `STREAMLIB_SURFACE_SOCKET` and exchange DMA-BUF fds via `SCM_RIGHTS`.

pub mod gpu_pool_claimant_process;
pub mod state;
pub mod surface_share_sockets_left_by_dead_runtimes;
pub mod unix_socket_service;

pub use state::{SurfaceShareState, ViewedFrameSource};
pub use surface_share_sockets_left_by_dead_runtimes::remove_the_surface_share_sockets_left_by_dead_runtimes;
pub use unix_socket_service::UnixSocketSurfaceService;
