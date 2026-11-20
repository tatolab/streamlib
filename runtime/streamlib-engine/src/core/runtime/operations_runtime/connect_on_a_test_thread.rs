// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `connect` on a stopped stream from a test's own thread, and the links the stream's graph then
//! holds, for the tests of what connect refuses and keeps.

use std::sync::Arc;

use super::{
    ALinkConnectedOrSetAside, InputValueChangesAConnectWaitsFor, LiveGraphChangeOnOneStream,
    WhenTheEngineCannotWireALink, connect_impl,
};
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
/// `compiler` builds, refusing a link the engine cannot wire.
pub(super) fn connect_on_this_thread_as(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
    a_link_connected: ALinkConnected,
) -> Result<LinkUniqueId> {
    connect_on_this_thread_judging(
        compiler,
        from,
        to,
        a_link_connected,
        WhenTheEngineCannotWireALink::Refuse,
        InputValueChangesAConnectWaitsFor::default(),
    )
    .and_then(ALinkConnectedOrSetAside::connected)
}

/// Connect `from` to `to` as a link made now, in the stopped stream `compiler` builds, waiting
/// for the input value changes `one_input_value_change_at_a_time` names.
pub(super) fn connect_on_this_thread_waiting_for(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
    one_input_value_change_at_a_time: InputValueChangesAConnectWaitsFor,
) -> Result<LinkUniqueId> {
    connect_on_this_thread_judging(
        compiler,
        from,
        to,
        ALinkConnected::MadeNow,
        WhenTheEngineCannotWireALink::Refuse,
        one_input_value_change_at_a_time,
    )
    .and_then(ALinkConnectedOrSetAside::connected)
}

/// Connect a link of a saved graph `from` to `to` as a load or a reconcile makes it, in the
/// stopped stream `compiler` builds: one the engine cannot wire is set aside.
pub(super) fn connect_a_link_of_a_saved_graph_on_this_thread(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<ALinkConnectedOrSetAside> {
    connect_on_this_thread_judging(
        compiler,
        from,
        to,
        ALinkConnected::OfASavedGraph,
        WhenTheEngineCannotWireALink::SetAsideLeavingTheReaderRuleToASweep,
        InputValueChangesAConnectWaitsFor::default(),
    )
}

/// Connect `from` to `to` as `a_link_connected` says, a link the engine cannot wire handled as
/// `when_the_engine_cannot_wire_it` says, in the stopped stream `compiler` builds, waiting for
/// the input value changes `one_input_value_change_at_a_time` names.
fn connect_on_this_thread_judging(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
    a_link_connected: ALinkConnected,
    when_the_engine_cannot_wire_it: WhenTheEngineCannotWireALink,
    one_input_value_change_at_a_time: InputValueChangesAConnectWaitsFor,
) -> Result<ALinkConnectedOrSetAside> {
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
                held_for_one_commit: None,
                one_input_value_change_at_a_time,
            },
            from,
            to,
            a_link_connected,
            when_the_engine_cannot_wire_it,
        ))
}

/// How many links the graph `compiler` builds holds.
pub(super) fn the_links_in(compiler: &Compiler) -> usize {
    compiler.scope(|graph, _tx| graph.traversal().e(()).iter().count())
}
