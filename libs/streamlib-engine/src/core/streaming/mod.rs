// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! MoQ transport primitives.

#[cfg(feature = "moq")]
pub mod moq_catalog;
#[cfg(feature = "moq")]
pub mod moq_session;

#[cfg(feature = "moq")]
pub use moq_catalog::{MoqBroadcastCatalog, MoqCatalogTrackEntry};
#[cfg(feature = "moq")]
pub use moq_session::{
    MoqPublishSession, MoqRelayConfig, MoqSubgroupReader, MoqSubscribeSession, MoqTrackReader,
    SharedMoqSessions, DEFAULT_MOQ_RELAY_URL,
};
