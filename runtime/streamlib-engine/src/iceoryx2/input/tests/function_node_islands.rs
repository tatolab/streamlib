// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

struct IslandAnsweringOneHalf;

impl FunctionNodeIslandEvaluatedAtEachRead for IslandAnsweringOneHalf {
    fn evaluate_the_island_output_for_this_read(
        &self,
    ) -> Result<FunctionNodeIslandOutputForOneRead> {
        Ok(FunctionNodeIslandOutputForOneRead {
            value: FunctionNodeIslandOutputValue::ControlValue(0.5),
            evaluated_at_ns: 0,
        })
    }

    fn the_first_function_node_it_runs_as_python(&self) -> Option<&str> {
        None
    }

    fn the_state_each_stateful_function_node_last_returned(&self) -> Vec<(u32, Vec<f64>)> {
        Vec::new()
    }

    fn the_reader_input_ports_its_links_from_outside_it_arrive_at(&self) -> &[String] {
        &[]
    }
}

struct IslandRunningOneNodeAsPython;

impl FunctionNodeIslandEvaluatedAtEachRead for IslandRunningOneNodeAsPython {
    fn evaluate_the_island_output_for_this_read(
        &self,
    ) -> Result<FunctionNodeIslandOutputForOneRead> {
        Ok(FunctionNodeIslandOutputForOneRead {
            value: FunctionNodeIslandOutputValue::ControlValue(0.5),
            evaluated_at_ns: 0,
        })
    }

    fn the_first_function_node_it_runs_as_python(&self) -> Option<&str> {
        Some("tally")
    }

    fn the_state_each_stateful_function_node_last_returned(&self) -> Vec<(u32, Vec<f64>)> {
        vec![(7, vec![3.0])]
    }

    fn the_reader_input_ports_its_links_from_outside_it_arrive_at(&self) -> &[String] {
        &[]
    }
}

const GATE_READER_INPUT_PORT: &str = "function_node_island_input_l-counter";

/// A Gate's island: it reads the newest message at its reader input port
/// at each evaluation and hands it on unread while open.
struct GateIslandReadingALinkFromOutsideIt {
    open: std::sync::atomic::AtomicBool,
    evaluations: std::sync::atomic::AtomicUsize,
    reader_input_ports: Vec<String>,
    mailboxes_it_reads: std::sync::OnceLock<std::sync::Weak<InputMailboxesInner>>,
}

impl GateIslandReadingALinkFromOutsideIt {
    fn feeding_value_of(
        mailboxes: &Arc<InputMailboxesInner>,
    ) -> (Arc<Self>, TestChannelPublisherNumberingItsSends) {
        let gate_island = Arc::new(Self {
            open: std::sync::atomic::AtomicBool::new(true),
            evaluations: std::sync::atomic::AtomicUsize::new(0),
            reader_input_ports: vec![GATE_READER_INPUT_PORT.to_string()],
            mailboxes_it_reads: std::sync::OnceLock::new(),
        });
        gate_island
            .mailboxes_it_reads
            .set(Arc::downgrade(mailboxes))
            .unwrap();
        mailboxes.add_port(GATE_READER_INPUT_PORT, 8, ReadMode::ReadNextInOrder);
        let counter_publisher = a_test_link_into(mailboxes, GATE_READER_INPUT_PORT);
        mailboxes.feed_an_input_port_from_a_function_node_island(
            "value",
            Arc::clone(&gate_island) as Arc<dyn FunctionNodeIslandEvaluatedAtEachRead>,
        );
        (gate_island, counter_publisher)
    }

    fn set_open(&self, open: bool) {
        self.open.store(open, std::sync::atomic::Ordering::SeqCst);
    }

    fn evaluations(&self) -> usize {
        self.evaluations.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl FunctionNodeIslandEvaluatedAtEachRead for GateIslandReadingALinkFromOutsideIt {
    fn evaluate_the_island_output_for_this_read(
        &self,
    ) -> Result<FunctionNodeIslandOutputForOneRead> {
        self.evaluations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mailboxes = self.mailboxes_it_reads.get().unwrap().upgrade().unwrap();
        let mut newest_message = None;
        while let Some(message) = mailboxes.read_raw(GATE_READER_INPUT_PORT)? {
            newest_message = Some(message);
        }
        let value = match newest_message {
            Some(message) if self.open.load(std::sync::atomic::Ordering::SeqCst) => {
                FunctionNodeIslandOutputValue::MessageHandedOnUnread(
                    crate::iceoryx2::MessageAFunctionNodeIslandHandsOnUnread::read_off_its_channel(
                        &message,
                    ),
                )
            }
            _ => FunctionNodeIslandOutputValue::Nothing,
        };
        Ok(FunctionNodeIslandOutputForOneRead {
            value,
            evaluated_at_ns: 0,
        })
    }

    fn the_first_function_node_it_runs_as_python(&self) -> Option<&str> {
        None
    }

    fn the_state_each_stateful_function_node_last_returned(&self) -> Vec<(u32, Vec<f64>)> {
        Vec::new()
    }

    fn the_reader_input_ports_its_links_from_outside_it_arrive_at(&self) -> &[String] {
        &self.reader_input_ports
    }
}

#[test]
fn a_closed_gate_wakes_no_reader_and_reports_no_data_however_many_messages_arrive() {
    let mailboxes = Arc::new(InputMailboxesInner::new());
    let (gate_island, counter_publisher) =
        GateIslandReadingALinkFromOutsideIt::feeding_value_of(&mailboxes);
    gate_island.set_open(false);
    publish_one_frame(&counter_publisher, "count", b"1");
    publish_one_frame(&counter_publisher, "count", b"2");

    assert!(
        !mailboxes.any_port_has_data(),
        "a message the closed gate hands nothing out for wakes no reader"
    );
    assert!(
        !mailboxes.any_port_has_data(),
        "and it does not wake the reader on the next judgement either"
    );
    assert!(!mailboxes.has_data("value"));
    assert!(mailboxes.read_raw("value").unwrap().is_none());
    assert_eq!(
        gate_island.evaluations(),
        1,
        "only the judgement the messages arrived for evaluates the island; the has_data \
         and the read after it take what it evaluated"
    );
}

#[test]
fn an_open_gate_wakes_its_reader_until_a_read_takes_the_message_it_judged() {
    let mailboxes = Arc::new(InputMailboxesInner::new());
    let (gate_island, counter_publisher) =
        GateIslandReadingALinkFromOutsideIt::feeding_value_of(&mailboxes);
    assert!(!mailboxes.any_port_has_data());
    assert!(!mailboxes.has_data("value"));
    assert!(mailboxes.read_raw("value").unwrap().is_none());

    publish_one_frame(&counter_publisher, "count", b"1");
    assert!(mailboxes.any_port_has_data());
    assert!(
        mailboxes.any_port_has_data(),
        "a message handed on unread keeps waking the reader until a read takes it"
    );
    let evaluations_before_the_read = gate_island.evaluations();
    gate_island.set_open(false);
    assert!(mailboxes.has_data("value"));
    let message = mailboxes.read_raw("value").unwrap().unwrap();
    assert_eq!(message.payload.bytes(), b"1");
    assert_eq!(
        gate_island.evaluations(),
        evaluations_before_the_read,
        "the read takes the message the judgement evaluated rather than dropping it"
    );
    assert!(!mailboxes.any_port_has_data());
    assert!(mailboxes.read_raw("value").unwrap().is_none());
}

#[test]
fn a_reader_that_never_reads_the_gated_port_is_not_woken_again_once_the_gate_closes() {
    let mailboxes = Arc::new(InputMailboxesInner::new());
    let (gate_island, counter_publisher) =
        GateIslandReadingALinkFromOutsideIt::feeding_value_of(&mailboxes);
    gate_island.set_open(false);
    for count in 0..5u8 {
        publish_one_frame(&counter_publisher, "count", &[count]);
        assert!(!mailboxes.any_port_has_data());
    }
    gate_island.set_open(true);
    publish_one_frame(&counter_publisher, "count", b"6");
    assert!(mailboxes.any_port_has_data());
    let message = mailboxes.read_raw("value").unwrap().unwrap();
    assert_eq!(
        message.payload.bytes(),
        b"6",
        "a message the closed gate passed over is not handed on once it opens"
    );
}

#[test]
fn the_function_node_an_island_runs_as_python_is_named_by_the_port_it_feeds() {
    let mailboxes = InputMailboxesInner::new();
    mailboxes.feed_an_input_port_from_a_function_node_island(
        "brightness",
        Arc::new(IslandAnsweringOneHalf),
    );
    mailboxes.feed_an_input_port_from_a_function_node_island(
        "count",
        Arc::new(IslandRunningOneNodeAsPython),
    );
    assert_eq!(
        mailboxes.the_function_node_run_as_python_by_the_island_feeding("brightness"),
        None
    );
    assert_eq!(
        mailboxes.the_function_node_run_as_python_by_the_island_feeding("count"),
        Some("tally".to_string())
    );
    assert_eq!(
        mailboxes.the_function_node_run_as_python_by_the_island_feeding("unfed"),
        None
    );
}

#[test]
fn an_unlinked_input_port_reads_its_pin_value_until_a_link_feeds_it() {
    let mailboxes = InputMailboxesInner::new();
    let stream_clock_now_ns: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(|| 1_000);
    mailboxes.read_each_unlinked_input_port_as_its_pin_value(
        HashMap::from([("level".to_string(), 0.25)]),
        Arc::clone(&stream_clock_now_ns),
    );
    assert!(mailboxes.has_port("level"));
    assert!(mailboxes.has_data("level"));
    assert!(!mailboxes.any_port_has_data());
    let message = mailboxes.read_raw("level").unwrap().unwrap();
    assert_eq!(message.payload.bytes(), 0.25f64.to_le_bytes());
    assert_eq!(message.timestamp_ns, 1_000);
    assert_eq!(start_sample_of_the_next_read(&mailboxes, "level"), 1);

    mailboxes.read_each_unlinked_input_port_as_its_pin_value(
        HashMap::from([("level".to_string(), 0.75)]),
        Arc::clone(&stream_clock_now_ns),
    );
    let message = mailboxes.read_raw("level").unwrap().unwrap();
    assert_eq!(message.payload.bytes(), 0.75f64.to_le_bytes());
    assert_eq!(
        streamlib_ipc_types::ChannelsBlockDescriptorBody::from_descriptor_body(
            &message.payload_descriptor.body,
        )
        .start_sample,
        2,
        "a pin edit counts reads on from where the port was"
    );

    let (_publisher, subscriber) = open_channel_for_one_link("pin/linked", 4);
    mailboxes.add_port("level", 8, ReadMode::SkipToLatest);
    mailboxes.add_channel_subscriber(
        "level",
        "L-lfo",
        &InboundLinkName::from("plfo/result"),
        subscriber,
    );
    assert!(
        mailboxes.read_raw("level").unwrap().is_none(),
        "a linked port reads its link, which has sent nothing"
    );
    mailboxes.remove_channel_link("L-lfo");
    assert!(mailboxes.read_raw("level").unwrap().is_some());

    mailboxes.read_each_unlinked_input_port_as_its_pin_value(HashMap::new(), stream_clock_now_ns);
    assert!(
        !mailboxes.has_port("level"),
        "a port with no pin value and no link left is not one this processor reads"
    );
}

fn start_sample_of_the_next_read(mailboxes: &InputMailboxesInner, port: &str) -> u64 {
    let message = mailboxes.read_raw(port).unwrap().unwrap();
    streamlib_ipc_types::ChannelsBlockDescriptorBody::from_descriptor_body(
        &message.payload_descriptor.body,
    )
    .start_sample
}

#[test]
fn an_island_replacing_the_one_feeding_a_port_carries_its_sample_positions_on() {
    let mailboxes = InputMailboxesInner::new();
    mailboxes.feed_an_input_port_from_a_function_node_island(
        "brightness",
        Arc::new(IslandAnsweringOneHalf),
    );
    assert_eq!(start_sample_of_the_next_read(&mailboxes, "brightness"), 0);
    assert_eq!(start_sample_of_the_next_read(&mailboxes, "brightness"), 1);
    mailboxes.feed_an_input_port_from_a_function_node_island(
        "brightness",
        Arc::new(IslandRunningOneNodeAsPython),
    );
    assert_eq!(start_sample_of_the_next_read(&mailboxes, "brightness"), 2);
    assert_eq!(
        mailboxes.the_function_node_run_as_python_by_the_island_feeding("brightness"),
        Some("tally".to_string()),
        "the replacing island answers the port's reads"
    );
    assert_eq!(
        mailboxes.the_state_each_stateful_function_node_of_the_island_feeding_last_returned(
            "brightness"
        ),
        vec![(7, vec![3.0])]
    );
    assert!(
        mailboxes
            .the_state_each_stateful_function_node_of_the_island_feeding_last_returned("unfed")
            .is_empty()
    );
}

#[test]
fn a_port_an_island_stops_feeding_is_no_longer_named_among_the_ports() {
    let mailboxes = InputMailboxesInner::new();
    mailboxes.feed_an_input_port_from_a_function_node_island(
        "brightness",
        Arc::new(IslandAnsweringOneHalf),
    );
    assert!(mailboxes.stop_feeding_an_input_port_from_a_function_node_island("brightness"));
    assert!(!mailboxes.has_port("brightness"));
    assert!(!mailboxes.stop_feeding_an_input_port_from_a_function_node_island("brightness"));
}

#[test]
fn a_port_an_island_feeds_is_named_among_the_ports_once() {
    let mailboxes = InputMailboxesInner::new();
    mailboxes.feed_an_input_port_from_a_function_node_island(
        "brightness",
        Arc::new(IslandAnsweringOneHalf),
    );
    assert!(mailboxes.has_port("brightness"));
    assert_eq!(mailboxes.port_names(), vec!["brightness".to_string()]);
}
