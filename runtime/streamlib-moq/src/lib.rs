// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! MoQ (Media over QUIC) transport for StreamLib — publish/subscribe sessions
//! and the broadcast catalog. The reusable library half of `@tatolab/moq`:
//! the `@tatolab/moq` package's track processors and the api-server control
//! plane both build on these types; the loadable processors live in the
//! `@tatolab/moq` package.

pub mod moq_catalog;
pub mod moq_session;

pub use moq_catalog::{
    MoqBroadcastCatalog, MoqCatalogTrackEntry, catalog_entry_for_output_port,
    processor_port_to_moq_track_name,
};
pub use moq_session::{
    DEFAULT_MOQ_RELAY_URL, MoqPublishSession, MoqRelayConfig, MoqSubgroupReader,
    MoqSubscribeSession, MoqTrackReader, SharedMoqSessions, sessions_for_runtime,
    try_sessions_for_runtime,
};
