// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-runtime surface-sharing broker (Linux).
//!
//! Each `StreamRuntime` owns one [`UnixSocketSurfaceService`] listening on a
//! unique Unix socket under `$XDG_RUNTIME_DIR`. Polyglot subprocesses
//! spawned by the runtime receive the socket path through
//! `STREAMLIB_BROKER_SOCKET` and exchange DMA-BUF fds via `SCM_RIGHTS`.

pub mod state;
pub mod unix_socket_service;

pub use state::SurfaceBrokerState;
pub use unix_socket_service::UnixSocketSurfaceService;
