// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The reading thread's loop over a scripted dialer and recorded ports: no
//! network, no device and no runtime.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
enum WhatThePortsRecorded {
    Write {
        port: WhepPlayerOutputPort,
        packets_unit_bytes: Vec<u8>,
        timestamp_ns: i64,
        starts_group: bool,
    },
    StreamFacts {
        port: WhepPlayerOutputPort,
        stream_facts_json: String,
    },
}

#[derive(Default)]
struct RecordedOutputPorts {
    recorded: RefCell<Vec<WhatThePortsRecorded>>,
    log_records: RefCell<Vec<(WhepPlayerLogLevel, String)>>,
    refuse_writes_with: Option<String>,
}

impl WhepPlayerOutputPorts for RecordedOutputPorts {
    fn write(
        &self,
        port: WhepPlayerOutputPort,
        packets_unit_bytes: &[u8],
        timestamp_ns: i64,
        starts_group: bool,
    ) -> Result<(), String> {
        if let Some(refusal) = &self.refuse_writes_with {
            return Err(refusal.clone());
        }
        self.recorded
            .borrow_mut()
            .push(WhatThePortsRecorded::Write {
                port,
                packets_unit_bytes: packets_unit_bytes.to_vec(),
                timestamp_ns,
                starts_group,
            });
        Ok(())
    }

    fn set_stream_facts(
        &self,
        port: WhepPlayerOutputPort,
        stream_facts: &WhepPlayerPacketsStreamFacts,
    ) -> Result<(), String> {
        self.recorded
            .borrow_mut()
            .push(WhatThePortsRecorded::StreamFacts {
                port,
                stream_facts_json: stream_facts.stream_facts_json(),
            });
        Ok(())
    }

    fn log(&self, level: WhepPlayerLogLevel, message: &str) {
        self.log_records
            .borrow_mut()
            .push((level, message.to_string()));
    }
}

/// A session handing back its scripted messages, then setting the stop.
struct SessionReplayingMessages {
    messages_still_to_hand_back: VecDeque<MessageTheWhepPlayerWrites>,
    stop_signal: Rc<WhepPlayerStopSignal>,
    closed: Rc<RefCell<usize>>,
}

impl WhepPlayerConnectedSession for SessionReplayingMessages {
    fn next_message(
        &mut self,
        _timeout: Duration,
    ) -> Result<Option<MessageTheWhepPlayerWrites>, WebRtcExtensionError> {
        let message = self.messages_still_to_hand_back.pop_front();
        if message.is_none() {
            self.stop_signal.set();
        }
        Ok(message)
    }

    fn close(&mut self) {
        *self.closed.borrow_mut() += 1;
    }
}

/// Each dial pops the next scripted outcome: `None` connects a session that
/// replays `messages`, `Some(refusal)` fails the dial.
struct ScriptedDialer {
    dial_outcomes: RefCell<VecDeque<Option<String>>>,
    messages: Vec<MessageTheWhepPlayerWrites>,
    dials: RefCell<usize>,
    sessions_closed: Rc<RefCell<usize>>,
    stop_signal: Rc<WhepPlayerStopSignal>,
}

impl ScriptedDialer {
    fn new(
        dial_outcomes: Vec<Option<&str>>,
        messages: Vec<MessageTheWhepPlayerWrites>,
        stop_signal: Rc<WhepPlayerStopSignal>,
    ) -> Self {
        Self {
            dial_outcomes: RefCell::new(
                dial_outcomes
                    .into_iter()
                    .map(|outcome| outcome.map(str::to_string))
                    .collect(),
            ),
            messages,
            dials: RefCell::new(0),
            sessions_closed: Rc::new(RefCell::new(0)),
            stop_signal,
        }
    }
}

impl WhepPlayerSessionDialer for ScriptedDialer {
    fn dial(&self) -> Result<Box<dyn WhepPlayerConnectedSession>, WebRtcExtensionError> {
        *self.dials.borrow_mut() += 1;
        match self.dial_outcomes.borrow_mut().pop_front().flatten() {
            Some(refusal) => Err(WebRtcExtensionError::Signalling {
                protocol: "WHEP",
                what: refusal,
            }),
            None => Ok(Box::new(SessionReplayingMessages {
                messages_still_to_hand_back: self.messages.clone().into(),
                stop_signal: Rc::clone(&self.stop_signal),
                closed: Rc::clone(&self.sessions_closed),
            })),
        }
    }
}

fn a_stop_signal() -> Rc<WhepPlayerStopSignal> {
    Rc::new(WhepPlayerStopSignal::also_set_when(Box::new(|| false)))
}

fn access_unit(
    starts_group: bool,
    timestamp_ns: i64,
    coded_width: u32,
) -> MessageTheWhepPlayerWrites {
    MessageTheWhepPlayerWrites {
        port: WhepPlayerOutputPort::EncodedVideo,
        packets_unit_bytes: vec![0, 0, 0, 1, 0x65],
        timestamp_ns,
        starts_group,
        stream_facts: WhepPlayerPacketsStreamFacts::Video {
            codec: "avc1.42e01f".to_string(),
            coded_width,
            coded_height: 192,
        },
    }
}

fn opus_packet(timestamp_ns: i64) -> MessageTheWhepPlayerWrites {
    MessageTheWhepPlayerWrites {
        port: WhepPlayerOutputPort::EncodedAudio,
        packets_unit_bytes: vec![0x78, 1, 2, 3],
        timestamp_ns,
        starts_group: true,
        stream_facts: WhepPlayerPacketsStreamFacts::Audio {
            number_of_channels: 1,
            opus_identification_header_hex: "4f707573486561640101000080bb0000000000".to_string(),
        },
    }
}

#[test]
fn a_refused_dial_is_retried_with_a_doubling_backoff_rather_than_ending_the_stream() {
    let stop_signal = a_stop_signal();
    let dialer = ScriptedDialer::new(
        vec![Some("409 Conflict"), Some("409 Conflict"), None],
        Vec::new(),
        Rc::clone(&stop_signal),
    );
    let output_ports = RecordedOutputPorts::default();
    let mut waited = Vec::new();

    play_until_stopped(&dialer, &output_ports, &stop_signal, &mut |delay| {
        waited.push(delay)
    });

    assert_eq!(*dialer.dials.borrow(), 3, "one session per attempt");
    assert_eq!(
        *dialer.sessions_closed.borrow(),
        1,
        "the session that connected is closed"
    );
    assert_eq!(
        waited,
        vec![FIRST_RECONNECT_DELAY, FIRST_RECONNECT_DELAY * 2]
    );
    assert!(
        output_ports
            .log_records
            .borrow()
            .iter()
            .any(|(level, message)| *level == WhepPlayerLogLevel::Warn
                && message.contains("409 Conflict")),
        "a refusal reaches the node's log"
    );
}

#[test]
fn the_backoff_stops_doubling_at_its_ceiling() {
    let stop_signal = a_stop_signal();
    let refusals = 12;
    let dialer = ScriptedDialer::new(
        (0..refusals).map(|_| Some("still down")).collect(),
        Vec::new(),
        Rc::clone(&stop_signal),
    );
    let mut waited = Vec::new();
    let stop_after_the_last_refusal = Rc::clone(&stop_signal);

    play_until_stopped(
        &dialer,
        &RecordedOutputPorts::default(),
        &stop_signal,
        &mut |delay| {
            waited.push(delay);
            if waited.len() == refusals {
                stop_after_the_last_refusal.set();
            }
        },
    );

    assert_eq!(waited.iter().max(), Some(&LONGEST_RECONNECT_DELAY));
    assert_eq!(waited.last(), Some(&LONGEST_RECONNECT_DELAY));
}

#[test]
fn a_stop_during_the_backoff_ends_the_thread_without_redialling() {
    let stop_signal = a_stop_signal();
    let dialer = ScriptedDialer::new(vec![Some("down")], Vec::new(), Rc::clone(&stop_signal));
    let stop_arrives_mid_backoff = Rc::clone(&stop_signal);

    play_until_stopped(
        &dialer,
        &RecordedOutputPorts::default(),
        &stop_signal,
        &mut |_| {
            stop_arrives_mid_backoff.set();
        },
    );

    assert_eq!(*dialer.dials.borrow(), 1);
}

#[test]
fn the_node_hosts_stop_flag_stops_the_player_too() {
    let stop_signal = WhepPlayerStopSignal::also_set_when(Box::new(|| true));
    let dialer = ScriptedDialer::new(vec![None], Vec::new(), a_stop_signal());

    play_until_stopped(
        &dialer,
        &RecordedOutputPorts::default(),
        &stop_signal,
        &mut |_| panic!("a stopped player waits out no backoff"),
    );

    assert_eq!(*dialer.dials.borrow(), 0);
}

#[test]
fn a_refused_write_names_its_port_and_ends_the_thread_rather_than_redialling() {
    let stop_signal = a_stop_signal();
    let dialer = ScriptedDialer::new(
        vec![None],
        vec![access_unit(true, 1_000, 320)],
        Rc::clone(&stop_signal),
    );
    let output_ports = RecordedOutputPorts {
        refuse_writes_with: Some("the link refused it".to_string()),
        ..RecordedOutputPorts::default()
    };

    play_until_stopped(&dialer, &output_ports, &stop_signal, &mut |_| {
        panic!("a refused write is not retried")
    });

    assert_eq!(*dialer.dials.borrow(), 1);
    assert_eq!(*dialer.sessions_closed.borrow(), 1);
    let log_records = output_ports.log_records.borrow();
    let (level, message) = log_records.last().unwrap();
    assert_eq!(*level, WhepPlayerLogLevel::Error);
    assert!(message.contains("`encoded_video`"), "{message}");
    assert!(message.contains("the link refused it"), "{message}");
}

#[test]
fn only_a_sync_point_opens_a_group_on_the_video_port_and_every_opus_packet_opens_one() {
    let stop_signal = a_stop_signal();
    let dialer = ScriptedDialer::new(
        vec![None],
        vec![
            access_unit(true, 1_000, 320),
            access_unit(false, 2_000, 320),
            opus_packet(1_500),
            opus_packet(1_520),
        ],
        Rc::clone(&stop_signal),
    );
    let output_ports = RecordedOutputPorts::default();

    play_until_stopped(&dialer, &output_ports, &stop_signal, &mut |_| {});

    let writes: Vec<(WhepPlayerOutputPort, i64, bool)> = output_ports
        .recorded
        .borrow()
        .iter()
        .filter_map(|recorded| match recorded {
            WhatThePortsRecorded::Write {
                port,
                timestamp_ns,
                starts_group,
                ..
            } => Some((*port, *timestamp_ns, *starts_group)),
            WhatThePortsRecorded::StreamFacts { .. } => None,
        })
        .collect();
    assert_eq!(
        writes,
        vec![
            (WhepPlayerOutputPort::EncodedVideo, 1_000, true),
            (WhepPlayerOutputPort::EncodedVideo, 2_000, false),
            (WhepPlayerOutputPort::EncodedAudio, 1_500, true),
            (WhepPlayerOutputPort::EncodedAudio, 1_520, true),
        ]
    );
}

#[test]
fn each_entry_is_stated_in_hangs_names_before_the_first_frame_and_again_only_on_change() {
    let stop_signal = a_stop_signal();
    let dialer = ScriptedDialer::new(
        vec![None],
        vec![
            access_unit(true, 1_000, 320),
            opus_packet(1_000),
            access_unit(false, 2_000, 320),
            access_unit(true, 3_000, 640),
        ],
        Rc::clone(&stop_signal),
    );
    let output_ports = RecordedOutputPorts::default();

    play_until_stopped(&dialer, &output_ports, &stop_signal, &mut |_| {});

    let recorded = output_ports.recorded.borrow();
    let video_stream_facts = |coded_width: u32| WhatThePortsRecorded::StreamFacts {
        port: WhepPlayerOutputPort::EncodedVideo,
        stream_facts_json: format!(
            "{{\"rfc6381_codec\":\"avc1.42e01f\",\"coded_width\":{coded_width},\"coded_height\":192}}"
        ),
    };
    let write = |message: MessageTheWhepPlayerWrites| WhatThePortsRecorded::Write {
        port: message.port,
        packets_unit_bytes: message.packets_unit_bytes,
        timestamp_ns: message.timestamp_ns,
        starts_group: message.starts_group,
    };
    assert_eq!(
        *recorded,
        vec![
            video_stream_facts(320),
            write(access_unit(true, 1_000, 320)),
            WhatThePortsRecorded::StreamFacts {
                port: WhepPlayerOutputPort::EncodedAudio,
                stream_facts_json: "{\"rfc6381_codec\":\"opus\",\"sample_rate\":48000,\"channel_count\":1,\
                             \"codec_configuration_hex\":\"4f707573486561640101000080bb0000000000\"}"
                    .to_string(),
            },
            write(opus_packet(1_000)),
            write(access_unit(false, 2_000, 320)),
            video_stream_facts(640),
            write(access_unit(true, 3_000, 640)),
        ]
    );
}

#[test]
fn an_oversized_unit_is_reported_once_rather_than_silently_dropped() {
    let stop_signal = a_stop_signal();
    let mut oversized = access_unit(true, 1_000, 320);
    oversized.packets_unit_bytes = vec![0; HELPER_LINK_PAYLOAD_CEILING_BYTES + 1];
    let dialer = ScriptedDialer::new(
        vec![None],
        vec![oversized.clone(), oversized],
        Rc::clone(&stop_signal),
    );
    let output_ports = RecordedOutputPorts::default();

    play_until_stopped(&dialer, &output_ports, &stop_signal, &mut |_| {});

    let reports = output_ports
        .log_records
        .borrow()
        .iter()
        .filter(|(level, message)| {
            *level == WhepPlayerLogLevel::Error && message.contains("link ceiling")
        })
        .count();
    assert_eq!(reports, 1);
}

#[test]
fn a_backoff_wait_ends_as_soon_as_the_stop_arrives() {
    let stop_signal = std::sync::Arc::new(WhepPlayerStopSignal::also_set_when(Box::new(|| false)));
    let stopper = std::sync::Arc::clone(&stop_signal);
    let started = std::time::Instant::now();
    let stopping = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        stopper.set();
    });

    stop_signal.wait_unless_stopped(LONGEST_RECONNECT_DELAY);

    stopping.join().unwrap();
    assert!(started.elapsed() < LONGEST_RECONNECT_DELAY / 2);
    assert!(stop_signal.is_set());
}

#[test]
fn a_codec_string_with_a_quote_is_escaped_in_the_stream_facts_json() {
    let stream_facts = WhepPlayerPacketsStreamFacts::Video {
        codec: "avc1\"x".to_string(),
        coded_width: 1,
        coded_height: 1,
    };
    assert_eq!(
        stream_facts.stream_facts_json(),
        "{\"rfc6381_codec\":\"avc1\\\"x\",\"coded_width\":1,\"coded_height\":1}"
    );
}

/// A session that hands back nothing and then reports it has ended.
struct SessionEndingQuietly {
    closed: Rc<RefCell<usize>>,
}

impl WhepPlayerConnectedSession for SessionEndingQuietly {
    fn next_message(
        &mut self,
        _timeout: Duration,
    ) -> Result<Option<MessageTheWhepPlayerWrites>, WebRtcExtensionError> {
        Err(WebRtcExtensionError::Transport {
            what: "the WHEP session's peer connection is failed".to_owned(),
        })
    }

    fn close(&mut self) {
        *self.closed.borrow_mut() += 1;
    }
}

/// The first dial connects a session that ends with no media; every later dial
/// connects one that sets the stop as soon as it is read.
struct DialerWhoseFirstSessionEndsQuietly {
    dials: RefCell<usize>,
    sessions_closed: Rc<RefCell<usize>>,
    stop_signal: Rc<WhepPlayerStopSignal>,
}

impl WhepPlayerSessionDialer for DialerWhoseFirstSessionEndsQuietly {
    fn dial(&self) -> Result<Box<dyn WhepPlayerConnectedSession>, WebRtcExtensionError> {
        *self.dials.borrow_mut() += 1;
        if *self.dials.borrow() == 1 {
            return Ok(Box::new(SessionEndingQuietly {
                closed: Rc::clone(&self.sessions_closed),
            }));
        }
        Ok(Box::new(SessionReplayingMessages {
            messages_still_to_hand_back: VecDeque::new(),
            stop_signal: Rc::clone(&self.stop_signal),
            closed: Rc::clone(&self.sessions_closed),
        }))
    }
}

#[test]
fn a_session_that_ends_with_no_media_is_closed_and_redialled() {
    let stop_signal = a_stop_signal();
    let dialer = DialerWhoseFirstSessionEndsQuietly {
        dials: RefCell::new(0),
        sessions_closed: Rc::new(RefCell::new(0)),
        stop_signal: Rc::clone(&stop_signal),
    };
    let output_ports = RecordedOutputPorts::default();
    let mut backoffs_waited = Vec::new();

    play_until_stopped(&dialer, &output_ports, &stop_signal, &mut |delay| {
        backoffs_waited.push(delay)
    });

    assert_eq!(*dialer.dials.borrow(), 2);
    assert_eq!(*dialer.sessions_closed.borrow(), 2);
    assert_eq!(backoffs_waited, [FIRST_RECONNECT_DELAY]);
    assert!(
        output_ports
            .log_records
            .borrow()
            .iter()
            .any(|(level, message)| *level == WhepPlayerLogLevel::Warn
                && message.contains("the session ended")),
        "{:?}",
        output_ports.log_records.borrow()
    );
}
