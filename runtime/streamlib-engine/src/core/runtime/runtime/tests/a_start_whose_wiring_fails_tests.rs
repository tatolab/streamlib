// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A start whose WIRE phase cannot open a link's channel: refused by name, no node set up on
//! links that were never wired, and torn down cleanly.

use std::sync::atomic::AtomicUsize;

use super::*;
use crate::core::nodes::{NODE_REGISTRY, NodeSpec};
use crate::core::test_support::{MockInputOnlyNode, ensure_test_mocks_registered};
use crate::core::{InputLinkPortRef, OutputLinkPortRef};

/// How many times a [`SourceCountingItsSetupsAndStarts`] has run `setup()` or `start()`.
static SETUPS_AND_STARTS_OF_THE_SOURCE: AtomicUsize = AtomicUsize::new(0);

/// A source with two outputs that counts each `setup()` and `start()` it runs.
#[crate::node(
    execution = manual,
    output("out1", family = Records),
    output("out2", family = Records),
)]
pub(crate) struct SourceCountingItsSetupsAndStarts;

impl crate::core::ManualNode for SourceCountingItsSetupsAndStarts::Node {
    fn setup(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        SETUPS_AND_STARTS_OF_THE_SOURCE.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn teardown(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
    fn start(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
    ) -> crate::core::error::Result<()> {
        SETUPS_AND_STARTS_OF_THE_SOURCE.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// A start whose second link's channel is already held with room for fewer subscribers than
/// the link needs is refused naming that channel and what the open asked for. No node of the
/// stream sets up or starts on links the refused start never finished wiring, and the stream
/// unloads, unwinding the first link's channel, and ends.
///
/// Mental-revert: a compile that fails at WIRE drops each parked node thread's CONTINUE sender,
/// and a node thread that reads the dropped sender as CONTINUE sets up and starts with its
/// ports unwired, so the count of setups and starts is no longer zero; and the refusal without
/// the channel's name fails the name assertions.
#[test]
#[serial]
fn a_start_whose_channel_cannot_open_is_refused_by_name_and_sets_up_no_node() {
    ensure_test_mocks_registered();
    NODE_REGISTRY.register::<SourceCountingItsSetupsAndStarts::Node>();
    SETUPS_AND_STARTS_OF_THE_SOURCE.store(0, Ordering::SeqCst);
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let stream = an_empty_stream_loaded_into(&runner, project_directory.path(), "wiring");
    let source = stream
        .add_node(NodeSpec::new(
            SourceCountingItsSetupsAndStarts::node_class_import_path(),
            serde_json::json!({}),
        ))
        .expect("the source is added");
    let reader = stream
        .add_node(NodeSpec::new(
            MockInputOnlyNode::node_class_import_path(),
            serde_json::json!({}),
        ))
        .expect("the reader is added");
    for (output, input) in [("out1", "in1"), ("out2", "in2")] {
        stream
            .connect(
                OutputLinkPortRef::new(source.as_str(), output),
                InputLinkPortRef::new(reader.as_str(), input),
            )
            .expect("the link is queued");
    }
    let refused_channel = crate::iceoryx2::source_channel_name(source.as_str(), "out2")
        .expect("the source's channel has a name")
        .into_string();
    let held_depth = 16;
    let _held_with_room_for_one_subscriber = runner
        .iceoryx2_node()
        .open_or_create_service(&refused_channel, 1, held_depth)
        .expect("the channel is held before the start");

    let refusal = stream
        .start()
        .expect_err("a start whose link's channel cannot open is refused")
        .to_string();

    assert!(
        refusal.contains(&format!("'{refused_channel}'")),
        "{refusal}"
    );
    assert!(
        refusal.contains(&format!("a subscriber buffer of {held_depth} samples")),
        "{refusal}"
    );
    assert!(
        refusal.contains("DoesNotSupportRequestedAmountOfSubscribers"),
        "{refusal}"
    );
    runner
        .unload_stream("wiring")
        .expect("the stream a failed start leaves unloads cleanly");
    assert!(stream.has_ended());
    assert_eq!(
        SETUPS_AND_STARTS_OF_THE_SOURCE.load(Ordering::SeqCst),
        0,
        "a node set up or started on links its refused start never finished wiring"
    );
}
