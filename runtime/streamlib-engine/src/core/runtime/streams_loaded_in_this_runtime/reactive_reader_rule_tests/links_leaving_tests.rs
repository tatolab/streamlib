// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The reader rule judged by a batch whose links leave a chain of function nodes: a link
//! leaving feeds the island no longer.

use super::*;

/// A source feeding the island `one` → `two`, which feeds a reactive reader.
fn a_reactive_reader_fed_by_a_source_through_a_chain_of_islands() -> Value {
    a_document_of(
        &[
            ("source", SOURCE),
            ("one", SWITCH),
            ("two", SWITCH),
            ("reader", REACTIVE_READER),
        ],
        &[
            (("source", "out"), ("one", "x")),
            (("one", "result"), ("two", "x")),
            (("two", "result"), ("reader", "in1")),
        ],
    )
}

/// A batch disconnecting the link between two function nodes of the island feeding a reactive
/// reader is refused as it closes, and the link stands again: the island reached through it no
/// longer reaches the source.
///
/// Mental-revert: following the link leaving to reach the source takes the batch, and the
/// reader is left to an island no source feeds.
#[test]
fn a_batch_cutting_the_chain_from_a_reactive_readers_source_is_refused_as_it_closes() {
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    let async_runtime = an_async_runtime();
    load(
        &stream,
        a_reactive_reader_fed_by_a_source_through_a_chain_of_islands(),
    );
    assert!(the_links_set_aside(&stream).is_empty());

    let batch = a_graph_edit_batch_of(&stream);
    async_runtime
        .block_on(
            batch.apply_a_stream_graph_operation_live(StreamGraphOperation::Disconnect {
                link: a_document_link(("one", "result"), ("two", "x")),
            }),
        )
        .expect("the batch judges its end");
    let refused = batch.close().expect_err("the reader would never run");

    assert!(refused.to_string().contains("would never run"), "{refused}");
    assert_eq!(the_live_links(&stream), 3);
}

/// A batch disconnecting the link into the reactive reader of that chain, which leaves no reader
/// to strand, is taken.
#[test]
fn a_batch_taking_out_the_link_into_the_reader_of_the_chain_is_taken() {
    let (_runner, stream, _project) = a_stopped_stream_resolving_a_reactive_reader();
    let async_runtime = an_async_runtime();
    load(
        &stream,
        a_reactive_reader_fed_by_a_source_through_a_chain_of_islands(),
    );

    let batch = a_graph_edit_batch_of(&stream);
    async_runtime
        .block_on(
            batch.apply_a_stream_graph_operation_live(StreamGraphOperation::Disconnect {
                link: a_document_link(("two", "result"), ("reader", "in1")),
            }),
        )
        .expect("the batch judges its end");
    batch.close().expect("no reader is left to an island alone");

    assert_eq!(the_live_links(&stream), 2);
}
