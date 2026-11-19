// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `connect` on a stopped stream from a test's own thread, and the links the stream's graph then
//! holds, for the tests of what connect refuses and keeps.

use std::sync::Arc;

use super::{LiveGraphChangeOnOneStream, connect_impl};
use crate::core::Result;
use crate::core::compiler::Compiler;
use crate::core::graph::{ALinkConnected, InputLinkPortRef, LinkUniqueId, OutputLinkPortRef};
use crate::core::pubsub::LoadedStreamIdentity;

/// Connect `from` to `to` as a link made now, in the stopped stream `compiler` builds.
pub(super) fn connect_on_this_thread(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<LinkUniqueId> {
    connect_on_this_thread_as(compiler, from, to, ALinkConnected::MadeNow)
}

/// Connect `from` to `to` as `a_link_connected` says it is connected, in the stopped stream
/// `compiler` builds.
pub(super) fn connect_on_this_thread_as(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
    a_link_connected: ALinkConnected,
) -> Result<LinkUniqueId> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("current-thread runtime")
        .block_on(connect_impl(
            LiveGraphChangeOnOneStream {
                compiler: Arc::clone(compiler),
                live: None,
                stream: LoadedStreamIdentity {
                    runtime_id: "Rconnecttest".to_string(),
                    stream_id: "main".to_string(),
                    stream_tag: crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1),
                },
                one_input_value_change_at_a_time: Default::default(),
            },
            from,
            to,
            a_link_connected,
        ))
}

/// How many links the graph `compiler` builds holds.
pub(super) fn the_links_in(compiler: &Compiler) -> usize {
    compiler.scope(|graph, _tx| graph.traversal().e(()).iter().count())
}
