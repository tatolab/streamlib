// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-runtime surface-sharing service (Linux).
//!
//! Each `StreamRuntime` owns one [`UnixSocketSurfaceService`] listening on a
//! unique Unix socket under `$XDG_RUNTIME_DIR`. Polyglot subprocesses
//! spawned by the runtime receive the socket path through
//! `STREAMLIB_SURFACE_SOCKET` and exchange DMA-BUF fds via `SCM_RIGHTS`.

pub mod state;
pub mod unix_socket_service;

pub use state::SurfaceShareState;
pub use unix_socket_service::UnixSocketSurfaceService;
