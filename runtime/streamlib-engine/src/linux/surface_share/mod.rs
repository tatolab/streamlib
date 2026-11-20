// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Per-runtime surface-sharing service (Linux).
//!
//! Each `Runner` owns one [`UnixSocketSurfaceService`] listening on a
//! unique Unix socket in the runtime directory. Polyglot subprocesses
//! spawned by the runtime receive the socket path through
//! `STREAMLIB_SURFACE_SOCKET` and exchange DMA-BUF fds via `SCM_RIGHTS`.

pub(crate) mod gpu_pool_claimant_process;
pub(crate) mod state;
pub(crate) mod surface_share_sockets_left_by_dead_runtimes;
pub(crate) mod unix_socket_service;

#[cfg(test)]
pub(crate) use state::SurfaceRegistration;
pub(crate) use state::{SurfaceShareState, ViewedFrameSource};
pub(crate) use surface_share_sockets_left_by_dead_runtimes::remove_the_surface_share_sockets_left_by_dead_runtimes;
pub use unix_socket_service::UnixSocketSurfaceService;
