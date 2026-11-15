// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::BTreeMap;

use super::*;

fn slot_and_generation(
    slot: usize,
    wiring_generation: u64,
) -> InboundLinkLossCountBoardSlotAndWiringGeneration {
    InboundLinkLossCountBoardSlotAndWiringGeneration {
        slot,
        wiring_generation,
    }
}

#[test]
fn a_count_written_on_a_claimed_slot_is_what_the_parent_reads() {
    let (board, writer, _helper_node) =
        a_loss_count_board_and_its_helpers_writer_for_this_test_process(&["video"], &[]);

    let mirror = writer
        .claim_inbound_link_slot(slot_and_generation(3, 7))
        .unwrap();
    assert_eq!(
        board.inbound_link_slot(3),
        Some(InboundLinkLossCountBoardSlot {
            wiring_generation: 7,
            ..InboundLinkLossCountBoardSlot::default()
        }),
        "a claim writes the wiring's generation and zero counts before any loss"
    );

    mirror.mirror_dropped_bags(5);
    mirror.mirror_discarded_samples(480);
    mirror.mirror_dropped_bags(4);
    assert_eq!(
        board.inbound_link_slot(3),
        Some(InboundLinkLossCountBoardSlot {
            wiring_generation: 7,
            dropped_bags: 5,
            discarded_samples: 480,
            ..InboundLinkLossCountBoardSlot::default()
        }),
        "a total that arrives late never lowers the count"
    );

    let video_mirror = writer.claim_output_port_entry("video", 8).unwrap();
    video_mirror.mirror_refused_bags(2);
    video_mirror.mirror_written_bags(60);
    video_mirror.mirror_written_bags(59);
    assert_eq!(
        board.output_port_entry("video"),
        Some(OutputPortBagCountBoardEntry {
            wiring_generation: 8,
            refused_bags: 2,
            written_bags: 60,
        }),
        "a written total that arrives late never lowers the count either"
    );
    assert_eq!(board.output_port_entry("audio"), None);
    assert!(writer.claim_output_port_entry("audio", 9).is_none());
}

#[test]
fn a_write_from_a_wiring_the_slot_has_moved_past_lands_nothing() {
    let (board, writer, _helper_node) =
        a_loss_count_board_and_its_helpers_writer_for_this_test_process(&[], &[]);
    let first_wiring = writer
        .claim_inbound_link_slot(slot_and_generation(0, 1))
        .unwrap();
    first_wiring.mirror_dropped_bags(9);

    let second_wiring = writer
        .claim_inbound_link_slot(slot_and_generation(0, 2))
        .unwrap();
    first_wiring.mirror_dropped_bags(12);
    second_wiring.mirror_dropped_bags(1);

    assert_eq!(
        board.inbound_link_slot(0),
        Some(InboundLinkLossCountBoardSlot {
            wiring_generation: 2,
            dropped_bags: 1,
            ..InboundLinkLossCountBoardSlot::default()
        }),
        "the slot carries only the wiring that claimed it last"
    );
}

#[test]
fn an_output_port_claimed_again_starts_from_zero_and_its_earlier_mirror_lands_nothing() {
    let (board, writer, _helper_node) =
        a_loss_count_board_and_its_helpers_writer_for_this_test_process(&["video"], &[]);
    let video_entry = || {
        board
            .output_port_entry("video")
            .map(|written| (written.wiring_generation, written.refused_bags))
    };
    let first_channel = writer.claim_output_port_entry("video", 1).unwrap();
    first_channel.mirror_refused_bags(3);

    let second_channel = writer.claim_output_port_entry("video", 2).unwrap();
    assert_eq!(video_entry(), Some((2, 0)));
    first_channel.mirror_refused_bags(4);
    assert_eq!(video_entry(), Some((2, 0)));
    second_channel.mirror_refused_bags(1);
    assert_eq!(video_entry(), Some((2, 1)));
}

#[test]
fn the_last_counts_stay_readable_after_the_helpers_writer_is_gone() {
    let (board, writer, helper_node) =
        a_loss_count_board_and_its_helpers_writer_for_this_test_process(&["video"], &[]);
    writer
        .claim_inbound_link_slot(slot_and_generation(1, 4))
        .unwrap()
        .mirror_dropped_bags(6);
    writer
        .claim_output_port_entry("video", 5)
        .unwrap()
        .mirror_refused_bags(2);

    drop(writer);
    drop(helper_node);

    assert_eq!(
        board.inbound_link_slot(1).map(|slot| slot.dropped_bags),
        Some(6)
    );
    assert_eq!(
        board
            .output_port_entry("video")
            .map(|written| written.refused_bags),
        Some(2)
    );
}

#[test]
fn slots_are_reused_lowest_first_and_every_wiring_gets_a_new_generation() {
    let counts = HelperPlacedProcessorLossCounts::default();
    assert_eq!(
        counts.assign_inbound_link_slot("L-a", false).unwrap(),
        slot_and_generation(0, 1)
    );
    assert_eq!(
        counts.assign_inbound_link_slot("L-b", false).unwrap(),
        slot_and_generation(1, 2)
    );

    counts.forget_link("L-a");
    assert_eq!(
        counts.assign_inbound_link_slot("L-c", false).unwrap(),
        slot_and_generation(0, 3)
    );
    assert_eq!(
        counts.assign_inbound_link_slot("L-b", true).unwrap(),
        slot_and_generation(1, 4),
        "a link wired again gives up its slot and takes a new generation"
    );

    let snapshot = counts.loss_count_snapshot();
    assert_eq!(
        snapshot.dropped_bags_by_inbound_link,
        BTreeMap::from([("L-b".to_string(), 0), ("L-c".to_string(), 0)]),
        "an assigned link renders zero before any board exists"
    );
    assert_eq!(
        snapshot.discarded_samples_by_inbound_link,
        BTreeMap::from([("L-b".to_string(), 0)]),
        "only a link into a windowed port carries a sample count"
    );
}

/// Fail-without-fix: render an output port's entry whatever generation is
/// written on it, and a port whose channel was reopened shows the three
/// bags its earlier channel refused.
#[test]
fn the_parent_reads_each_entry_only_for_the_wiring_it_assigned() {
    let (board, writer, _helper_node) =
        a_loss_count_board_and_its_helpers_writer_for_this_test_process(&["video"], &[]);
    let counts = HelperPlacedProcessorLossCounts::default();
    counts.hold_the_board_of_this_spawn(board);
    let refused_bags_of_video = || counts.loss_count_snapshot().refused_bags_by_output_port;
    let written_bags_of_video = || counts.loss_count_snapshot().written_bags_by_output_port;

    let first_channel = counts.note_outbound_link("video", "L-first-out");
    let first_channels_mirror = writer
        .claim_output_port_entry("video", first_channel)
        .unwrap();
    first_channels_mirror.mirror_refused_bags(3);
    first_channels_mirror.mirror_written_bags(90);
    assert_eq!(
        refused_bags_of_video(),
        BTreeMap::from([("video".to_string(), 3)])
    );
    assert_eq!(
        written_bags_of_video(),
        BTreeMap::from([("video".to_string(), 90)])
    );

    counts.forget_link("L-first-out");
    let second_channel = counts.note_outbound_link("video", "L-second-out");
    assert_eq!(
        refused_bags_of_video(),
        BTreeMap::from([("video".to_string(), 0)]),
        "the earlier channel's total is not the reopened channel's"
    );
    assert_eq!(
        written_bags_of_video(),
        BTreeMap::from([("video".to_string(), 0)]),
        "the earlier channel's written total is not the reopened channel's"
    );
    writer
        .claim_output_port_entry("video", second_channel)
        .unwrap()
        .mirror_refused_bags(1);
    assert_eq!(
        refused_bags_of_video(),
        BTreeMap::from([("video".to_string(), 1)])
    );

    let inbound = counts.assign_inbound_link_slot("L-in", false).unwrap();
    writer
        .claim_inbound_link_slot(inbound)
        .unwrap()
        .mirror_dropped_bags(4);
    assert_eq!(counts.total_dropped_bag_count(), 4);
    counts.assign_inbound_link_slot("L-in", false).unwrap();
    assert_eq!(
        counts.total_dropped_bag_count(),
        0,
        "the total reads each link for its own wiring"
    );
}

/// A restarted helper writes on a fresh board, and each wiring's counts
/// keep counting from what the dead helper's board last held rather than
/// going back to zero; a reconnect still starts from zero.
///
/// Fail-without-fix: keep one board per processor and the restarted
/// helper's board is refused; drop the earlier board's counts and a link's
/// losses go backwards in `graph` at every restart.
#[test]
fn a_restarted_helpers_board_keeps_each_wirings_counts_cumulative() {
    let counts = HelperPlacedProcessorLossCounts::default();
    let inbound = counts.assign_inbound_link_slot("L-in", false).unwrap();
    let video_channel = counts.note_outbound_link("video", "L-out");
    let (dead_helpers_board, dead_helpers_writer, dead_helper_node) =
        a_loss_count_board_and_its_helpers_writer_for_this_test_process(&["video"], &[]);
    counts.hold_the_board_of_this_spawn(dead_helpers_board);
    dead_helpers_writer
        .claim_inbound_link_slot(inbound)
        .unwrap()
        .mirror_dropped_bags(6);
    dead_helpers_writer
        .claim_output_port_entry("video", video_channel)
        .unwrap()
        .mirror_written_bags(100);
    drop(dead_helpers_writer);
    drop(dead_helper_node);

    let (new_helpers_board, new_helpers_writer, _new_helper_node) =
        a_loss_count_board_and_its_helpers_writer_for_this_test_process(&["video"], &[]);
    counts.hold_the_board_of_this_spawn(new_helpers_board);
    assert_eq!(counts.total_dropped_bag_count(), 6);
    new_helpers_writer
        .claim_inbound_link_slot(inbound)
        .unwrap()
        .mirror_dropped_bags(2);
    new_helpers_writer
        .claim_output_port_entry("video", video_channel)
        .unwrap()
        .mirror_written_bags(5);

    let snapshot = counts.loss_count_snapshot();
    assert_eq!(
        snapshot.dropped_bags_by_inbound_link,
        BTreeMap::from([("L-in".to_string(), 8)])
    );
    assert_eq!(
        snapshot.written_bags_by_output_port,
        BTreeMap::from([("video".to_string(), 105)])
    );

    counts.assign_inbound_link_slot("L-in", false).unwrap();
    assert_eq!(
        counts.total_dropped_bag_count(),
        0,
        "a reconnected link starts from zero, restarts or not"
    );
}

#[test]
fn a_links_taken_bags_are_read_off_its_slot_less_the_ones_counted_lost_after_taking() {
    let (board, writer, _helper_node) =
        a_loss_count_board_and_its_helpers_writer_for_this_test_process(&[], &[]);
    let counts = HelperPlacedProcessorLossCounts::default();
    counts.hold_the_board_of_this_spawn(board);
    let inbound = counts.assign_inbound_link_slot("L-in", false).unwrap();
    let mirror = writer.claim_inbound_link_slot(inbound).unwrap();

    mirror.mirror_bags_off_the_mailbox_for_its_reader(7);
    mirror.mirror_bags_its_reader_counted_lost_after_taking_them(2);
    mirror.mirror_bags_off_the_mailbox_for_its_reader(6);
    assert_eq!(
        counts.loss_count_snapshot().taken_bags_by_inbound_link,
        BTreeMap::from([("L-in".to_string(), 5)])
    );
}

#[test]
fn the_helpers_step_progress_is_what_its_parent_reads_off_the_current_board() {
    let counts = HelperPlacedProcessorLossCounts::default();
    assert_eq!(counts.step_progress_its_helper_wrote(), None);
    let (board, writer, _helper_node) =
        a_loss_count_board_and_its_helpers_writer_for_this_test_process(&[], &[]);
    counts.hold_the_board_of_this_spawn(board);

    writer.note_a_callback_started();
    assert_eq!(
        counts.step_progress_its_helper_wrote(),
        Some(HelperProcessStepProgressBoardEntry {
            steps_finished: 0,
            callbacks_in_flight: 1,
            reserved: 0,
        })
    );
    writer.note_a_callback_returned(Some(4));
    writer.note_a_callback_started();
    writer.note_a_callback_returned(None);
    assert_eq!(
        counts.step_progress_its_helper_wrote(),
        Some(HelperProcessStepProgressBoardEntry {
            steps_finished: 5,
            callbacks_in_flight: 0,
            reserved: 0,
        }),
        "a callback that names no step leaves the steps finished where they were"
    );

    let (restarted_helpers_board, _restarted_writer, _restarted_helper_node) =
        a_loss_count_board_and_its_helpers_writer_for_this_test_process(&[], &[]);
    counts.hold_the_board_of_this_spawn(restarted_helpers_board);
    assert_eq!(
        counts
            .step_progress_its_helper_wrote()
            .map(|entry| entry.steps_finished),
        Some(0),
        "a restarted helper has finished no step until it finishes the one open"
    );
}

#[test]
fn a_function_node_call_breadcrumb_is_set_for_the_call_and_outlives_the_helper_that_set_it() {
    let counts = HelperPlacedProcessorLossCounts::default();
    assert_eq!(
        counts.function_node_call_breadcrumb_its_helper_wrote(),
        None
    );
    let (board, writer, helper_node) =
        a_loss_count_board_and_its_helpers_writer_for_this_test_process(&[], &[]);
    counts.hold_the_board_of_this_spawn(board);
    let breadcrumb_under_way = || {
        counts
            .function_node_call_breadcrumb_its_helper_wrote()
            .and_then(|entry| entry.function_node_whose_call_is_under_way())
    };
    assert_eq!(breadcrumb_under_way(), None);

    let call = writer.note_a_function_node_call_started(2, 1_000);
    assert_eq!(breadcrumb_under_way(), Some(2));
    writer.note_the_function_node_call_returned(call);
    assert_eq!(
        breadcrumb_under_way(),
        None,
        "a call that returned leaves no breadcrumb a later crash could read"
    );

    writer.note_a_function_node_call_started(5, 2_000);
    drop(writer);
    drop(helper_node);
    assert_eq!(
        breadcrumb_under_way(),
        Some(5),
        "the parent reads the call a helper died in off the board it holds"
    );
}

/// A node is named once three of its calls running overran the frame
/// interval: a call inside it starts the count again, another node's
/// overruns count apart, and a board told no interval names nobody.
#[test]
fn a_function_node_is_named_once_three_of_its_calls_running_overrun_the_frame_interval() {
    let (board, writer, _helper_node) =
        a_loss_count_board_and_its_helpers_writer_for_this_test_process(&[], &[]);
    let call_lasting = |function_node_breadcrumb_number: u32, call_ms: u64| {
        let started_at = 1_000_000_000;
        let call =
            writer.note_a_function_node_call_started(function_node_breadcrumb_number, started_at);
        writer.note_the_function_node_call_returned_at(call, started_at + call_ms * 1_000_000);
    };

    for _ in 0..4 {
        call_lasting(1, 50);
    }
    assert_eq!(
        board
            .function_node_overrun_entry()
            .function_node_that_overran(),
        None,
        "a helper told no frame interval times nothing"
    );

    writer.time_each_function_node_call_against_the_frame_interval(
        std::time::Duration::from_millis(10),
    );
    call_lasting(1, 15);
    call_lasting(1, 25);
    call_lasting(1, 5);
    call_lasting(1, 15);
    call_lasting(2, 40);
    call_lasting(1, 15);
    assert_eq!(
        board
            .function_node_overrun_entry()
            .function_node_that_overran(),
        None,
        "a call inside the interval starts the count again, and another node's overrun \
         counts apart"
    );

    call_lasting(1, 30);
    let overrun = board.function_node_overrun_entry();
    assert_eq!(overrun.function_node_that_overran(), Some(1));
    assert_eq!(overrun.longest_overrunning_call_ns, 30_000_000);

    call_lasting(2, 40);
    call_lasting(2, 40);
    assert_eq!(
        board
            .function_node_overrun_entry()
            .function_node_that_overran(),
        Some(1),
        "the first node named stays named"
    );
}

/// Two calls under way at once each keep their own breadcrumb: the board
/// names the older until it returns, and the younger returning first
/// neither clears nor replaces it.
#[test]
fn concurrent_function_node_calls_each_keep_their_breadcrumb_and_the_board_names_the_oldest() {
    let counts = HelperPlacedProcessorLossCounts::default();
    let (board, writer, _helper_node) =
        a_loss_count_board_and_its_helpers_writer_for_this_test_process(&[], &[]);
    counts.hold_the_board_of_this_spawn(board);
    let breadcrumb_under_way = || {
        counts
            .function_node_call_breadcrumb_its_helper_wrote()
            .and_then(|entry| entry.function_node_whose_call_is_under_way())
    };

    let calls_under_way = || {
        counts
            .function_node_call_breadcrumb_its_helper_wrote()
            .map(|entry| entry.function_node_calls_under_way)
    };

    let older_call = writer.note_a_function_node_call_started(1, 1_000);
    let younger_call = writer.note_a_function_node_call_started(2, 2_000);
    assert_eq!(breadcrumb_under_way(), Some(1));
    assert_eq!(
        calls_under_way(),
        Some(2),
        "the board says the oldest call is not the only one, so a death blames neither"
    );
    writer.note_the_function_node_call_returned(younger_call);
    assert_eq!(
        breadcrumb_under_way(),
        Some(1),
        "a younger call returning leaves the older one under way on the board"
    );

    let youngest_call = writer.note_a_function_node_call_started(3, 3_000);
    writer.note_the_function_node_call_returned(older_call);
    assert_eq!(breadcrumb_under_way(), Some(3));
    assert_eq!(calls_under_way(), Some(1));
    writer.note_the_function_node_call_returned(youngest_call);
    assert_eq!(breadcrumb_under_way(), None);
    assert_eq!(calls_under_way(), Some(0));
}

#[test]
fn a_destination_past_the_slot_count_is_refused_by_name() {
    let counts = HelperPlacedProcessorLossCounts::default();
    for link in 0..INBOUND_LINK_SLOTS_PER_LOSS_COUNT_BOARD {
        counts
            .assign_inbound_link_slot(&format!("L-{link}"), false)
            .unwrap();
    }
    let refusal = counts
        .assign_inbound_link_slot("L-one-too-many", false)
        .unwrap_err();
    assert!(
        refusal.to_string().contains("L-one-too-many"),
        "the refusal names the link: {refusal}"
    );
}

#[test]
fn an_output_port_keeps_its_generation_while_it_has_a_link_and_takes_a_new_one_after_its_last_goes()
{
    let counts = HelperPlacedProcessorLossCounts::default();
    let first_channel = counts.note_outbound_link("video", "L-first");
    assert_eq!(
        counts.note_outbound_link("video", "L-second"),
        first_channel
    );
    counts.forget_link("L-first");
    assert_eq!(
        counts.loss_count_snapshot().refused_bags_by_output_port,
        BTreeMap::from([("video".to_string(), 0)])
    );

    counts.forget_link("L-second");
    assert!(
        counts
            .loss_count_snapshot()
            .refused_bags_by_output_port
            .is_empty()
    );
    assert_ne!(
        counts.note_outbound_link("video", "L-third"),
        first_channel,
        "a reopened channel is a new wiring"
    );
}
