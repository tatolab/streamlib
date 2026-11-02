// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

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
        media_frame: Vec<u8>,
        timestamp_ns: i64,
        starts_group: bool,
    },
    Entry {
        port: WhepPlayerOutputPort,
        section: &'static str,
        entry_json: String,
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
        media_frame: &[u8],
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
                media_frame: media_frame.to_vec(),
                timestamp_ns,
                starts_group,
            });
        Ok(())
    }

    fn set_catalog_entry(
        &self,
        port: WhepPlayerOutputPort,
        catalog_entry: &WhepPlayerCatalogEntry,
    ) -> Result<(), String> {
        self.recorded
            .borrow_mut()
            .push(WhatThePortsRecorded::Entry {
                port,
                section: catalog_entry.section(),
                entry_json: catalog_entry.entry_json(),
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
        media_frame: vec![0x25, 0, 0, 0, 1, 0x65],
        timestamp_ns,
        starts_group,
        catalog_entry: WhepPlayerCatalogEntry::Video {
            codec: "avc1.42e01f".to_string(),
            coded_width,
            coded_height: 192,
        },
    }
}

fn opus_packet(timestamp_ns: i64) -> MessageTheWhepPlayerWrites {
    MessageTheWhepPlayerWrites {
        port: WhepPlayerOutputPort::EncodedAudio,
        media_frame: vec![0x25, 0x78, 1, 2, 3],
        timestamp_ns,
        starts_group: true,
        catalog_entry: WhepPlayerCatalogEntry::Audio {
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
            WhatThePortsRecorded::Entry { .. } => None,
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
    let video_entry = |coded_width: u32| WhatThePortsRecorded::Entry {
        port: WhepPlayerOutputPort::EncodedVideo,
        section: "video",
        entry_json: format!(
            "{{\"codec\":\"avc1.42e01f\",\"codedWidth\":{coded_width},\"codedHeight\":192}}"
        ),
    };
    let write = |message: MessageTheWhepPlayerWrites| WhatThePortsRecorded::Write {
        port: message.port,
        media_frame: message.media_frame,
        timestamp_ns: message.timestamp_ns,
        starts_group: message.starts_group,
    };
    assert_eq!(
        *recorded,
        vec![
            video_entry(320),
            write(access_unit(true, 1_000, 320)),
            WhatThePortsRecorded::Entry {
                port: WhepPlayerOutputPort::EncodedAudio,
                section: "audio",
                entry_json: "{\"codec\":\"opus\",\"sampleRate\":48000,\"numberOfChannels\":1,\
                             \"description\":\"4f707573486561640101000080bb0000000000\"}"
                    .to_string(),
            },
            write(opus_packet(1_000)),
            write(access_unit(false, 2_000, 320)),
            video_entry(640),
            write(access_unit(true, 3_000, 640)),
        ]
    );
}

#[test]
fn an_oversized_media_frame_is_reported_once_rather_than_silently_dropped() {
    let stop_signal = a_stop_signal();
    let mut oversized = access_unit(true, 1_000, 320);
    oversized.media_frame = vec![0; HELPER_LINK_PAYLOAD_CEILING_BYTES + 1];
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
fn a_codec_string_with_a_quote_is_escaped_in_the_entry_json() {
    let entry = WhepPlayerCatalogEntry::Video {
        codec: "avc1\"x".to_string(),
        coded_width: 1,
        coded_height: 1,
    };
    assert_eq!(
        entry.entry_json(),
        "{\"codec\":\"avc1\\\"x\",\"codedWidth\":1,\"codedHeight\":1}"
    );
}
