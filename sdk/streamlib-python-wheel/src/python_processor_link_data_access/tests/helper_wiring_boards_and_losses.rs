// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Groups an engine-internal writer declared given up upstream reach a
/// helper's reader as `lost_since_last_read` on the next message, through
/// the same mailboxes a native reader runs.
#[test]
fn a_loss_declared_upstream_of_a_link_reaches_a_helpers_reader_as_lost_since_last_read() {
    Python::initialize();
    Python::attach(|python| {
        let (source, destination) = one_wired_link(python, "declared-upstream-loss");
        source
            .output_writer
            .get()
            .expect("the source wired an output")
            .declare_groups_given_up_upstream_before_the_next_write("frames_to_downstream", 1)
            .unwrap();
        source
            .write_to_output_port(
                python,
                "frames_to_downstream",
                PyBytes::new(python, b"after the given-up group").as_any(),
                None,
                true,
            )
            .unwrap();

        let (_, message, _) = destination
            .read_message_naming_its_inbound_link_with_its_payload_cast(
                python,
                "frames_from_upstream",
                PortPayloadReadShape::RawBorrow,
                None,
            )
            .unwrap()
            .expect("the wired input received nothing");
        assert_eq!(message.lost_since_last_read, 1);
    });
}

/// A helper's egress crosses the subprocess boundary by construction, so the
/// tier it labels a ceiling refusal with is the one the parent classified
/// this channel under when it chose the ceiling the envelope carries.
#[test]
fn a_ceiling_refusal_on_a_helpers_egress_names_the_untrusted_session_tier() {
    Python::initialize();
    Python::attach(|python| {
        let (channel, notify) = unique_channel_names("egress-tier");
        let ceiling_bytes = 1 << 20;
        let source = helper_plane();
        source
            .wire_output_link(
                python,
                "frames_to_downstream",
                &channel,
                &notify,
                1024,
                ceiling_bytes,
                8,
                2,
                1,
                "link-egress-tier",
                None,
            )
            .unwrap();

        let output_writer = source
            .output_writer
            .get()
            .expect("a wired output plane holds its writer");
        let over_the_ceiling = vec![0u8; ceiling_bytes + 1];
        let refusal = output_writer
            .write_bytes_with_timestamp(
                "frames_to_downstream",
                &over_the_ceiling,
                1,
                MessageGroupBoundary::StartsGroup,
            )
            .expect_err("a message past the channel ceiling must be refused");

        match refusal {
            Error::PayloadExceedsChannelCeiling { tier, .. } => assert_eq!(
                tier.to_string(),
                "untrusted-session",
                "a helper's egress is an untrusted-session channel"
            ),
            other => panic!("expected a ceiling refusal, got {other:?}"),
        }
    });
}

/// A helper's callbacks reach its parent's board as the step ledger reads
/// them: in flight while one runs, and the step it finished once it returns.
#[test]
fn a_helpers_step_callbacks_reach_the_board_its_parent_reads() {
    Python::initialize();
    Python::attach(|python| {
        let board_name = format!("wiring{}_steps/loss-counts", std::process::id());
        let parent_board = Iceoryx2Node::for_this_test_process()
            .create_helper_process_loss_count_board(&board_name, Vec::new(), Vec::new())
            .unwrap();
        let helper = helper_plane();
        helper
            .open_loss_count_board(python, &board_name, Vec::new(), Vec::new(), None)
            .expect("the helper opens the board its parent named");

        helper.note_a_step_callback_started();
        let in_a_callback = parent_board.step_progress_entry();
        assert_eq!(
            (
                in_a_callback.callbacks_in_flight,
                in_a_callback.steps_finished
            ),
            (1, 0)
        );
        helper.note_a_step_callback_returned(Some(2));
        let returned = parent_board.step_progress_entry();
        assert_eq!(
            (returned.callbacks_in_flight, returned.steps_finished),
            (0, 3)
        );
    });
}

/// A helper in a stepped stream wakes on its descriptor when the engine
/// opens a step, and goes quiet again once it took the wake.
#[test]
fn a_helper_listening_on_its_stream_clock_wakes_when_a_step_opens() {
    use streamlib::sdk::context::{StreamClock, StreamClockSpec};

    Python::initialize();
    Python::attach(|python| {
        let board_name = format!("wiring{}_steps/stream-clock", std::process::id());
        let clock = StreamClock::starting_at(
            StreamClockSpec::Stepped {
                step_ns: 1_000_000,
                pace: None,
                stop_after_steps: None,
            },
            10,
        );
        let board = Iceoryx2Node::for_this_test_process()
            .create_stream_clock_board(&board_name, clock.published())
            .unwrap();
        let clock = clock.published_on(board);
        let helper = helper_plane();
        assert_eq!(helper.stream_clock_board_publish_listener_fd(), None);
        helper
            .listen_for_each_stream_clock_board_publish(python, &board_name)
            .expect("the helper listens on the board its engine named");
        let publish_listener_fd = helper
            .stream_clock_board_publish_listener_fd()
            .expect("a listening helper has the descriptor to wait on");
        let is_readable_within = |timeout_ms: i32| {
            let mut poll_descriptor = libc::pollfd {
                fd: publish_listener_fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one live descriptor, counted as one.
            unsafe { libc::poll(&mut poll_descriptor, 1, timeout_ms) == 1 }
        };
        helper.take_every_stream_clock_board_publish_noted();
        assert!(!is_readable_within(0));

        assert_eq!(clock.open_the_waiting_step(), Some(0));
        assert!(is_readable_within(1_000));
        helper.take_every_stream_clock_board_publish_noted();
        assert!(!is_readable_within(0));
    });
}

/// A helper that opened its parent's board mirrors an overrun `ordered`
/// port's losses onto its link's slot and a refusal at the ceiling onto its
/// output port's entry, with nothing but the reads and writes the processor
/// already makes — no loop flush and no timer.
#[test]
fn a_helper_that_opened_its_board_mirrors_its_losses_as_its_reads_and_writes_count_them() {
    Python::initialize();
    Python::attach(|python| {
        let (channel, notify) = unique_channel_names("board");
        let board_name = format!("wiring{}_board/loss-counts", std::process::id());
        let output_ports = vec!["frames_to_downstream".to_string()];
        let parent_board = Iceoryx2Node::for_this_test_process()
            .create_helper_process_loss_count_board(&board_name, output_ports.clone(), Vec::new())
            .unwrap();
        let source = helper_plane();
        let destination = helper_plane();
        destination
            .open_loss_count_board(python, &board_name, output_ports, Vec::new(), None)
            .expect("the helper opens the board its parent named");

        destination
            .wire_input_link(
                python,
                "frames_from_upstream",
                &channel,
                &channel,
                &notify,
                "read_next_in_order",
                8,
                8,
                2,
                1,
                "link-1",
                None,
                Some(4),
                Some(21),
                None,
            )
            .unwrap();
        destination
            .wire_output_link(
                python,
                "frames_to_downstream",
                &format!("{channel}_onward"),
                &format!("{notify}_onward"),
                64,
                1024,
                8,
                2,
                1,
                "link-onward",
                Some(31),
            )
            .unwrap();
        source
            .wire_output_link(
                python,
                "frames_to_downstream",
                &channel,
                &notify,
                1024,
                1 << 20,
                8,
                2,
                1,
                "link-1",
                None,
            )
            .unwrap();

        let write_frame = |frame_index: i64| {
            source
                .write_to_output_port(
                    python,
                    "frames_to_downstream",
                    PyBytes::new(python, &frame_index.to_le_bytes()).as_any(),
                    None,
                    true,
                )
                .unwrap();
        };
        let read_frame_index = || {
            let payload = destination
                .read_from_input_port(python, "frames_from_upstream", None)
                .unwrap()
                .expect("the port holds a message");
            i64::from_le_bytes(
                PythonPortPayloadBorrow::bytes_the_read_borrowed(&payload)
                    .try_into()
                    .expect("an eight-byte frame index"),
            )
        };
        let slot_counts = || {
            parent_board.inbound_link_slot(4).map(|slot| {
                (
                    slot.wiring_generation,
                    slot.dropped_bags,
                    slot.discarded_samples,
                )
            })
        };

        write_frame(0);
        assert_eq!(read_frame_index(), 0);
        assert_eq!(slot_counts(), Some((21, 0, 0)));
        for frame_index in 1..=12 {
            write_frame(frame_index);
        }
        assert_eq!(read_frame_index(), 5);
        assert_eq!(
            slot_counts(),
            Some((21, 4, 0)),
            "the four messages the eight-deep ring overwrote land on the link's slot at the \
             read"
        );
        destination
            .count_the_message_last_read_from_input_port_as_lost(python, "frames_from_upstream")
            .expect("the message the link delivered is countable");
        assert_eq!(
            slot_counts(),
            Some((21, 5, 0)),
            "a message the reader counts lost lands on the link's slot as it is counted"
        );

        destination
            .write_to_output_port(
                python,
                "frames_to_downstream",
                PyBytes::new(python, &[0u8; 4096]).as_any(),
                None,
                true,
            )
            .expect("a refusal at the ceiling is never raised");
        destination
            .write_to_output_port(
                python,
                "frames_to_downstream",
                PyBytes::new(python, &[0u8; 16]).as_any(),
                None,
                true,
            )
            .expect("a message under the ceiling is sent");
        assert_eq!(
            parent_board
                .output_port_entry("frames_to_downstream")
                .map(|entry| (
                    entry.wiring_generation,
                    entry.refused_bags,
                    entry.written_bags
                )),
            Some((31, 1, 1)),
            "the refused message is counted as refused, the sent one as written"
        );

        let second_channel = format!("{channel}_second");
        let refusal = destination
            .wire_input_link(
                python,
                "frames_from_upstream",
                &second_channel,
                &second_channel,
                &notify,
                "read_next_in_order",
                8,
                8,
                2,
                1,
                "link-with-no-slot",
                None,
                None,
                None,
                None,
            )
            .expect_err("a helper with a board open is told where every link's counts go");
        assert!(
            refusal.to_string().contains("link-with-no-slot")
                && refusal.to_string().contains("loss_count_slot"),
            "the refusal names the link and what it was wired without: {refusal}"
        );
        let refusal = destination
            .wire_output_link(
                python,
                "frames_to_downstream",
                &format!("{channel}_onward_again"),
                &format!("{notify}_onward_again"),
                64,
                1024,
                8,
                2,
                1,
                "link-with-no-channel-generation",
                None,
            )
            .expect_err(
                "a helper with a board open is told which channel a port's refusals belong to",
            );
        assert!(
            refusal
                .to_string()
                .contains("link-with-no-channel-generation")
                && refusal
                    .to_string()
                    .contains("output_port_wiring_generation"),
            "the refusal names the link and what it was wired without: {refusal}"
        );
    });
}

/// The window contract crosses the parent→child envelope and the child's
/// own stage honours it: a 48 kHz stereo source published on the wired
/// output as Channels blocks reaches the wired input as exactly-512-sample
/// 16 kHz mono Channels blocks, in this process, through the same engine
/// code the parent's mailboxes run.
#[test]
fn a_windowed_input_link_hands_the_child_windows_rather_than_the_blocks_it_was_sent() {
    Python::initialize();
    Python::attach(|python| {
        let (channel, notify) = unique_channel_names("windowed");
        let source = helper_plane();
        let destination = helper_plane();

        let contract = PyDict::new(python);
        contract.set_item("sample_rate", 16_000i64).unwrap();
        contract.set_item("channels", 1i64).unwrap();
        contract.set_item("dtype", "f32").unwrap();
        contract.set_item("window_size", 512i64).unwrap();
        contract.set_item("hop", 512i64).unwrap();

        destination
            .wire_input_link(
                python,
                "audio_from_upstream",
                &channel,
                &channel,
                &notify,
                "read_next_in_order",
                8,
                8,
                2,
                1,
                "link-1",
                Some(contract.as_any()),
                None,
                None,
                None,
            )
            .unwrap();
        source
            .wire_output_link(
                python,
                "audio",
                &channel,
                &notify,
                16_384,
                1 << 20,
                8,
                2,
                1,
                "link-1",
                None,
            )
            .unwrap();

        const SOURCE_FRAMES_PER_BLOCK: u64 = 512;
        const SOURCE_RATE: u64 = 48_000;
        let stereo_block = python
            .eval(
                c"memoryview(__import__('array').array('f', [i / 1024.0 for i in range(1024)])).cast('B').cast('f', [2, 512])",
                None,
                None,
            )
            .unwrap();
        for block in 0..8u64 {
            source
                .write_channels_block_to_output_port(
                    python,
                    "audio",
                    &stereo_block,
                    block * SOURCE_FRAMES_PER_BLOCK,
                    SOURCE_RATE as u32,
                    ChannelsOutputPortDeclaredShape::default(),
                    Some((block * SOURCE_FRAMES_PER_BLOCK * 1_000_000_000 / SOURCE_RATE) as i64),
                    true,
                )
                .unwrap();
        }

        let window = destination
            .read_from_input_port_offering_gpu_access(
                python,
                "audio_from_upstream",
                PortPayloadReadShape::FamilyType(PortFamiliesOfTheRead::of_one(
                    PortFamilyOfTheRead::Channels,
                )),
                None,
            )
            .unwrap()
            .expect("the windowed input received nothing");
        let attribute = |name: &str| window.getattr(name).unwrap();
        assert_eq!(
            attribute("samples_per_channel").extract::<u32>().unwrap(),
            512,
            "the child's stage owes exactly what the contract declared"
        );
        assert_eq!(attribute("sample_rate").extract::<u32>().unwrap(), 16_000);
        assert_eq!(attribute("channel_count").extract::<u32>().unwrap(), 1);
        assert_eq!(attribute("dtype").extract::<String>().unwrap(), "f32");
        assert_eq!(attribute("start_sample").extract::<u64>().unwrap(), 0);
    });
}

/// Fan-out sends one entry per link, and iceoryx2 admits exactly one
/// publisher per channel — so the second link must append its notifier
/// rather than open a second publisher.
#[test]
fn a_second_link_out_of_one_port_reuses_the_publisher() {
    Python::initialize();
    Python::attach(|python| {
        let (channel, notify) = unique_channel_names("fanout");
        let (_, second_notify) = unique_channel_names("fanout_second");
        let source = helper_plane();

        source
            .wire_output_link(
                python,
                "frames_to_downstream",
                &channel,
                &notify,
                1024,
                1 << 20,
                8,
                2,
                1,
                "link-1",
                None,
            )
            .unwrap();
        source
            .wire_output_link(
                python,
                "frames_to_downstream",
                &channel,
                &second_notify,
                1024,
                1 << 20,
                8,
                2,
                1,
                "link-2",
                None,
            )
            .expect("a second link out of one port must not reopen the publisher");
    });
}

/// The parent's copy is wired by the compiler op and owns no node of its
/// own; asking it to wire itself must say so rather than panic on a missing
/// node.
#[test]
fn an_engine_wired_plane_refuses_to_wire_itself() {
    Python::initialize();
    Python::attach(|python| {
        let engine_wired = PythonProcessorLinkDataAccess::new();
        let (channel, notify) = unique_channel_names("engineowned");
        let refusal = engine_wired
            .wire_output_link(
                python,
                "frames_to_downstream",
                &channel,
                &notify,
                1024,
                1 << 20,
                8,
                2,
                1,
                "link-1",
                None,
            )
            .unwrap_err();
        assert!(
            refusal.to_string().contains("helper process wires its own"),
            "unexpected refusal: {refusal}"
        );
    });
}

#[test]
fn an_unknown_read_mode_is_refused_by_name() {
    Python::initialize();
    Python::attach(|python| {
        let (channel, notify) = unique_channel_names("readmode");
        let destination = helper_plane();
        let refusal = destination
            .wire_input_link(
                python,
                "frames_from_upstream",
                &channel,
                &channel,
                &notify,
                "whenever",
                8,
                8,
                2,
                1,
                "link-1",
                None,
                None,
                None,
                None,
            )
            .unwrap_err();
        assert!(
            refusal.to_string().contains("whenever"),
            "the refusal must name what it got: {refusal}"
        );
    });
}
