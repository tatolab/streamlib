// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn the_loss_count_board_key_is_two_u32s_under_its_pinned_name() {
    assert_eq!(std::mem::size_of::<HelperProcessLossCountBoardKey>(), 8);
    assert_eq!(std::mem::align_of::<HelperProcessLossCountBoardKey>(), 4);
    assert_eq!(
        std::mem::offset_of!(HelperProcessLossCountBoardKey, section),
        0
    );
    assert_eq!(
        std::mem::offset_of!(HelperProcessLossCountBoardKey, index_within_section),
        4
    );
    assert_ne!(
        HelperProcessLossCountBoardKey::inbound_link_slot(3),
        HelperProcessLossCountBoardKey::output_port(3),
        "a slot and an output port at one index are two entries"
    );
    assert_ne!(
        HelperProcessLossCountBoardKey::STEP_PROGRESS,
        HelperProcessLossCountBoardKey::inbound_link_slot(0),
        "the step-progress entry is in a section of its own"
    );
    assert_ne!(
        HelperProcessLossCountBoardKey::FUNCTION_NODE_CALL_BREADCRUMB,
        HelperProcessLossCountBoardKey::STEP_PROGRESS,
        "the function-node call breadcrumb is in a section of its own"
    );
    assert_eq!(
        // SAFETY: reads the pinned name; the layout it vouches for is asserted above.
        unsafe { <HelperProcessLossCountBoardKey as ZeroCopySend>::type_name() },
        "HelperProcessLossCountBoardKey"
    );
}

#[test]
fn a_function_node_state_entry_is_two_counts_then_its_values_under_its_pinned_name() {
    assert_eq!(
        std::mem::size_of::<HelperProcessFunctionNodeStateBoardEntry>(),
        16 + 8 * STATE_VALUES_ONE_FUNCTION_NODE_STATE_BOARD_ENTRY_HOLDS
    );
    assert_eq!(
        std::mem::align_of::<HelperProcessFunctionNodeStateBoardEntry>(),
        8
    );
    assert_eq!(
        std::mem::offset_of!(HelperProcessFunctionNodeStateBoardEntry, state_writes),
        0
    );
    assert_eq!(
        std::mem::offset_of!(HelperProcessFunctionNodeStateBoardEntry, state_value_count),
        8
    );
    assert_eq!(
        std::mem::offset_of!(HelperProcessFunctionNodeStateBoardEntry, reserved),
        12
    );
    assert_eq!(
        std::mem::offset_of!(HelperProcessFunctionNodeStateBoardEntry, state_values),
        16
    );
    assert_ne!(
        HelperProcessLossCountBoardKey::function_node_state(0),
        HelperProcessLossCountBoardKey::FUNCTION_NODE_CALL_BREADCRUMB,
        "a function node's state is in a section of its own"
    );
    assert_eq!(
        // SAFETY: reads the pinned name; the layout it vouches for is asserted above.
        unsafe { <HelperProcessFunctionNodeStateBoardEntry as ZeroCopySend>::type_name() },
        "HelperProcessFunctionNodeStateBoardEntry"
    );
}

#[test]
fn a_function_node_state_entry_holds_what_was_written_and_nothing_before_its_first_write() {
    assert_eq!(
        HelperProcessFunctionNodeStateBoardEntry::default().state_values_written(),
        None
    );
    let written = HelperProcessFunctionNodeStateBoardEntry::holding(&[0.25, -1.0], 3)
        .expect("two values fit");
    assert_eq!(written.state_values_written(), Some(&[0.25, -1.0][..]));
    assert!(
        HelperProcessFunctionNodeStateBoardEntry::holding(
            &[0.0; STATE_VALUES_ONE_FUNCTION_NODE_STATE_BOARD_ENTRY_HOLDS + 1],
            1
        )
        .is_none(),
        "a state past what an entry holds is refused"
    );
}

#[test]
fn an_inbound_link_slot_is_five_u64s_at_their_documented_offsets_under_its_pinned_name() {
    assert_eq!(std::mem::size_of::<InboundLinkLossCountBoardSlot>(), 40);
    assert_eq!(std::mem::align_of::<InboundLinkLossCountBoardSlot>(), 8);
    assert_eq!(
        std::mem::offset_of!(InboundLinkLossCountBoardSlot, wiring_generation),
        0
    );
    assert_eq!(
        std::mem::offset_of!(InboundLinkLossCountBoardSlot, dropped_bags),
        8
    );
    assert_eq!(
        std::mem::offset_of!(InboundLinkLossCountBoardSlot, discarded_samples),
        16
    );
    assert_eq!(
        std::mem::offset_of!(
            InboundLinkLossCountBoardSlot,
            bags_off_the_mailbox_for_its_reader
        ),
        24
    );
    assert_eq!(
        std::mem::offset_of!(
            InboundLinkLossCountBoardSlot,
            bags_its_reader_counted_lost_after_taking_them
        ),
        32
    );
    assert_eq!(
        // SAFETY: reads the pinned name; the layout it vouches for is asserted above.
        unsafe { <InboundLinkLossCountBoardSlot as ZeroCopySend>::type_name() },
        "InboundLinkLossCountBoardSlot"
    );
}

#[test]
fn an_output_ports_bag_count_entry_is_three_u64s_at_their_documented_offsets_under_its_pinned_name()
{
    assert_eq!(std::mem::size_of::<OutputPortBagCountBoardEntry>(), 24);
    assert_eq!(std::mem::align_of::<OutputPortBagCountBoardEntry>(), 8);
    assert_eq!(
        std::mem::offset_of!(OutputPortBagCountBoardEntry, wiring_generation),
        0
    );
    assert_eq!(
        std::mem::offset_of!(OutputPortBagCountBoardEntry, refused_bags),
        8
    );
    assert_eq!(
        std::mem::offset_of!(OutputPortBagCountBoardEntry, written_bags),
        16
    );
    assert_eq!(
        // SAFETY: reads the pinned name; the layout it vouches for is asserted above.
        unsafe { <OutputPortBagCountBoardEntry as ZeroCopySend>::type_name() },
        "OutputPortBagCountBoardEntry"
    );
}

#[test]
fn a_helpers_step_progress_entry_is_a_u64_and_two_u32s_under_its_pinned_name() {
    assert_eq!(
        std::mem::size_of::<HelperProcessStepProgressBoardEntry>(),
        16
    );
    assert_eq!(
        std::mem::align_of::<HelperProcessStepProgressBoardEntry>(),
        8
    );
    assert_eq!(
        std::mem::offset_of!(HelperProcessStepProgressBoardEntry, steps_finished),
        0
    );
    assert_eq!(
        std::mem::offset_of!(HelperProcessStepProgressBoardEntry, callbacks_in_flight),
        8
    );
    assert_eq!(
        std::mem::offset_of!(HelperProcessStepProgressBoardEntry, reserved),
        12
    );
    assert_eq!(
        // SAFETY: reads the pinned name; the layout it vouches for is asserted above.
        unsafe { <HelperProcessStepProgressBoardEntry as ZeroCopySend>::type_name() },
        "HelperProcessStepProgressBoardEntry"
    );
}

#[test]
fn a_helpers_function_node_call_breadcrumb_is_a_u64_and_two_u32s_under_its_pinned_name() {
    assert_eq!(
        std::mem::size_of::<HelperProcessFunctionNodeCallBreadcrumbBoardEntry>(),
        16
    );
    assert_eq!(
        std::mem::align_of::<HelperProcessFunctionNodeCallBreadcrumbBoardEntry>(),
        8
    );
    assert_eq!(
        std::mem::offset_of!(
            HelperProcessFunctionNodeCallBreadcrumbBoardEntry,
            call_started_at_monotonic_ns
        ),
        0
    );
    assert_eq!(
        std::mem::offset_of!(
            HelperProcessFunctionNodeCallBreadcrumbBoardEntry,
            function_node_breadcrumb_number
        ),
        8
    );
    assert_eq!(
        std::mem::offset_of!(
            HelperProcessFunctionNodeCallBreadcrumbBoardEntry,
            function_node_calls_under_way
        ),
        12
    );
    assert_eq!(
        // SAFETY: reads the pinned name; the layout it vouches for is asserted above.
        unsafe { <HelperProcessFunctionNodeCallBreadcrumbBoardEntry as ZeroCopySend>::type_name() },
        "HelperProcessFunctionNodeCallBreadcrumbBoardEntry"
    );
}

#[test]
fn a_helpers_function_node_overrun_entry_is_a_u64_and_two_u32s_under_its_pinned_name() {
    assert_eq!(
        std::mem::size_of::<HelperProcessFunctionNodeOverrunBoardEntry>(),
        16
    );
    assert_eq!(
        std::mem::align_of::<HelperProcessFunctionNodeOverrunBoardEntry>(),
        8
    );
    assert_eq!(
        std::mem::offset_of!(
            HelperProcessFunctionNodeOverrunBoardEntry,
            longest_overrunning_call_ns
        ),
        0
    );
    assert_eq!(
        std::mem::offset_of!(
            HelperProcessFunctionNodeOverrunBoardEntry,
            function_node_breadcrumb_number
        ),
        8
    );
    assert_eq!(
        std::mem::offset_of!(HelperProcessFunctionNodeOverrunBoardEntry, reserved),
        12
    );
    assert_ne!(
        HelperProcessLossCountBoardKey::FUNCTION_NODE_OVERRUN,
        HelperProcessLossCountBoardKey::function_node_state(0),
        "the overrun entry is in a section of its own"
    );
    assert_eq!(
        // SAFETY: reads the pinned name; the layout it vouches for is asserted above.
        unsafe { <HelperProcessFunctionNodeOverrunBoardEntry as ZeroCopySend>::type_name() },
        "HelperProcessFunctionNodeOverrunBoardEntry"
    );
    assert_eq!(
        HelperProcessFunctionNodeOverrunBoardEntry::default().function_node_that_overran(),
        None
    );
}

#[test]
fn a_breadcrumb_names_a_function_node_only_while_its_call_is_under_way() {
    let between_calls = HelperProcessFunctionNodeCallBreadcrumbBoardEntry {
        call_started_at_monotonic_ns: 0,
        function_node_breadcrumb_number: 3,
        function_node_calls_under_way: 0,
    };
    assert_eq!(between_calls.function_node_whose_call_is_under_way(), None);
    let during_a_call = HelperProcessFunctionNodeCallBreadcrumbBoardEntry {
        call_started_at_monotonic_ns: 42,
        ..between_calls
    };
    assert_eq!(
        during_a_call.function_node_whose_call_is_under_way(),
        Some(3)
    );
}
