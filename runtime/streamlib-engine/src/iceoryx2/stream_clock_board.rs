// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The blackboard a scaled or stepped stream publishes its clock on, and what
//! each of its helper processes reads stream time off.
//!
//! One board per stream start: the engine creates it before any helper of the
//! stream spawns and is its one writer; every helper opens it by the service
//! name its `setup` command carries and reads its one entry whole. Each publish
//! also notifies an event service beside the board, so a helper waiting on a
//! stepped stream's next step wakes as it opens.

use iceoryx2::port::listener::Listener;
use iceoryx2::port::notifier::Notifier;
use iceoryx2::port::reader::{EntryHandle, Reader};
use iceoryx2::port::writer::{EntryHandleMut, Writer};
use iceoryx2::prelude::*;
use iceoryx2::service::port_factory::blackboard::PortFactory as BlackboardPortFactory;
use iceoryx2::service::port_factory::event::PortFactory as EventPortFactory;
use parking_lot::Mutex;
use streamlib_ipc_types::{StreamClockBoardEntry, StreamClockBoardKey};

use crate::core::error::{Error, Result};

/// The blackboard service a stream clock board is.
pub(crate) type StreamClockBoardService =
    BlackboardPortFactory<ipc_threadsafe::Service, StreamClockBoardKey>;

/// The event service each publish on a stream clock board notifies.
pub(crate) type StreamClockBoardPublishEventService = EventPortFactory<ipc_threadsafe::Service>;

/// The name of the event service each publish on the board named
/// `board_service_name` notifies.
pub(crate) fn stream_clock_board_publish_event_service_name(board_service_name: &str) -> String {
    format!("{board_service_name}/each-publish")
}

// =============================================================================
// The engine's board
// =============================================================================

/// A stream's clock board as its engine holds it: the service it created, and
/// the one write handle on its entry.
pub struct StreamClockBoard {
    service_name: String,
    board_write_handles: Mutex<StreamClockBoardWriteHandles>,
}

/// The writer and its entry handle, and the notifier each publish wakes the
/// helpers through, dropped together.
///
/// Fields drop in declaration order: the handle, the writer it came from, the
/// notifier, then the services.
struct StreamClockBoardWriteHandles {
    entry: EntryHandleMut<ipc_threadsafe::Service, StreamClockBoardKey, StreamClockBoardEntry>,
    _writer: Writer<ipc_threadsafe::Service, StreamClockBoardKey>,
    each_publish_notifier: Notifier<ipc_threadsafe::Service>,
    _service: StreamClockBoardService,
    _publish_event_service: StreamClockBoardPublishEventService,
}

// SAFETY: as for the loss-count board's write handles: `Writer` holds
// iceoryx2's `SingleThreaded` `Rc`, the entry handle a clone of it, and both
// are made on one thread and dropped together in one `Drop`. A write,
// `EntryHandleMut::update_with_copy`, stores through the entry's single
// producer, which the surrounding `Mutex` serialises, and never touches the
// `Rc`. The notifier holds the same kind of `Rc`, made here and dropped here,
// and is only ever used under that `Mutex`. Re-check this against the source on
// any iceoryx2 upgrade.
unsafe impl Send for StreamClockBoardWriteHandles {}

impl StreamClockBoard {
    /// Take the write handle of the board `service` just created, and the
    /// notifier of `publish_event_service`.
    pub(crate) fn writing(
        service_name: String,
        service: StreamClockBoardService,
        publish_event_service: StreamClockBoardPublishEventService,
    ) -> Result<Self> {
        let writer = service.writer_builder().create().map_err(|refusal| {
            Error::Runtime(format!(
                "the stream clock board '{service_name}' was created but refused its engine the \
                 writer: {refusal:?}"
            ))
        })?;
        let entry = writer
            .entry::<StreamClockBoardEntry>(&StreamClockBoardKey::STREAM_CLOCK)
            .map_err(|refusal| {
                Error::Runtime(format!(
                    "the stream clock board '{service_name}' was created without its clock \
                     entry: {refusal:?}"
                ))
            })?;
        let each_publish_notifier =
            publish_event_service
                .notifier_builder()
                .create()
                .map_err(|refusal| {
                    Error::Runtime(format!(
                        "the stream clock board '{service_name}' refused its engine the notifier \
                     each publish wakes its helpers through: {refusal:?}"
                    ))
                })?;
        Ok(Self {
            service_name,
            board_write_handles: Mutex::new(StreamClockBoardWriteHandles {
                entry,
                _writer: writer,
                each_publish_notifier,
                _service: service,
                _publish_event_service: publish_event_service,
            }),
        })
    }

    /// The name a helper opens this board by.
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// Publish `entry` as the stream's clock to every reader, and wake every
    /// helper listening for a publish.
    pub fn publish(&self, entry: StreamClockBoardEntry) {
        let board_write_handles = self.board_write_handles.lock();
        board_write_handles.entry.update_with_copy(entry);
        if let Err(refusal) = board_write_handles.each_publish_notifier.notify() {
            tracing::warn!(
                "the stream clock board '{}' published without waking its helpers: {refusal:?}",
                self.service_name
            );
        }
    }
}

// =============================================================================
// A helper's reader
// =============================================================================

/// A stream's clock board as one of its helpers reads it.
pub struct StreamClockBoardReader {
    service_name: String,
    board_read_handles: StreamClockBoardReadHandles,
}

/// The reader and its entry handle, dropped together.
struct StreamClockBoardReadHandles {
    entry: EntryHandle<ipc_threadsafe::Service, StreamClockBoardKey, StreamClockBoardEntry>,
    _reader: Reader<ipc_threadsafe::Service, StreamClockBoardKey>,
    _service: StreamClockBoardService,
}

// SAFETY: as for the loss-count board's read handles: the reader's `Rc` and
// the entry handle's clone of it are made on one thread and dropped together in
// one `Drop`, and a read, `EntryHandle::get`, loads the entry's lock-free
// atomic without touching the `Rc`. Re-check on any iceoryx2 upgrade.
unsafe impl Send for StreamClockBoardReadHandles {}
unsafe impl Sync for StreamClockBoardReadHandles {}

impl StreamClockBoardReader {
    /// Take the read handle of the board `service` opened.
    pub(crate) fn reading(service_name: String, service: StreamClockBoardService) -> Result<Self> {
        let reader = service.reader_builder().create().map_err(|refusal| {
            Error::Runtime(format!(
                "the stream clock board '{service_name}' refused this helper a reader: \
                 {refusal:?}"
            ))
        })?;
        let entry = reader
            .entry::<StreamClockBoardEntry>(&StreamClockBoardKey::STREAM_CLOCK)
            .map_err(|refusal| {
                Error::Runtime(format!(
                    "the stream clock board '{service_name}' carries no clock entry: {refusal:?}"
                ))
            })?;
        Ok(Self {
            service_name,
            board_read_handles: StreamClockBoardReadHandles {
                entry,
                _reader: reader,
                _service: service,
            },
        })
    }

    /// The name this board was opened by.
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// The stream's clock as its engine last published it.
    pub fn read(&self) -> StreamClockBoardEntry {
        *self.board_read_handles.entry.get()
    }
}

// =============================================================================
// A helper's wake on each publish
// =============================================================================

/// A helper's listener on the event each publish of its stream's clock board
/// notifies: readable on its descriptor once the engine published.
pub struct StreamClockBoardPublishListener {
    listener_and_service: Mutex<StreamClockBoardPublishListenerAndService>,
    file_descriptor: i32,
}

/// The listener and the service it came from, dropped together.
struct StreamClockBoardPublishListenerAndService {
    listener: Listener<ipc_threadsafe::Service>,
    _publish_event_service: StreamClockBoardPublishEventService,
}

impl StreamClockBoardPublishListener {
    /// Listen on `publish_event_service` through `listener`.
    pub(crate) fn listening(
        listener: Listener<ipc_threadsafe::Service>,
        publish_event_service: StreamClockBoardPublishEventService,
    ) -> Self {
        // SAFETY: the descriptor is the listener's own, and lives as long as
        // this value holds the listener; `file_descriptor` hands it out only
        // while it does.
        let file_descriptor = unsafe { listener.file_descriptor().native_handle() };
        Self {
            listener_and_service: Mutex::new(StreamClockBoardPublishListenerAndService {
                listener,
                _publish_event_service: publish_event_service,
            }),
            file_descriptor,
        }
    }

    /// The descriptor that turns readable once the engine published; valid as
    /// long as this listener lives.
    pub fn file_descriptor(&self) -> i32 {
        self.file_descriptor
    }

    /// Take every publish noted so far, so the descriptor waits for the next.
    pub fn take_every_publish_noted(&self) {
        if let Err(refusal) = self
            .listener_and_service
            .lock()
            .listener
            .try_wait(|_publish| {})
        {
            tracing::trace!("taking the stream clock board's publishes failed: {refusal:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::context::{
        StreamClock, StreamClockSpec, stream_clock_reading_of_a_published_clock,
        stream_time_ns_on_a_published_clock,
    };
    use crate::iceoryx2::Iceoryx2Node;

    fn a_board_service_name() -> String {
        format!(
            "streamlib-test/stream-clock/{}",
            crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix()
        )
    }

    #[test]
    fn a_helper_reads_the_scaled_clock_its_engine_published() {
        let service_name = a_board_service_name();
        let started_at = 3_000_000_000;
        let clock = StreamClock::starting_at(
            StreamClockSpec::Scaled {
                scale: 0.5,
                step_ns: 1_000,
            },
            started_at,
        );
        let engine_node = Iceoryx2Node::for_this_test_process();
        let board = engine_node
            .create_stream_clock_board(&service_name, clock.published())
            .expect("the engine creates its stream's clock board");
        let clock = clock.published_on(board);
        assert_eq!(
            clock.board_service_name_for_helpers(),
            Some(service_name.as_str())
        );

        let helper_node = Iceoryx2Node::for_this_test_process();
        let reader = helper_node
            .open_stream_clock_board_reader(&service_name)
            .expect("a helper opens the board its engine named");
        let read = reader.read();

        assert_eq!(read, clock.published());
        let machine_now_ns = started_at + 8_000;
        assert_eq!(
            stream_time_ns_on_a_published_clock(&read, machine_now_ns),
            clock.stream_time_ns_at(machine_now_ns),
        );
        assert_eq!(stream_clock_reading_of_a_published_clock(&read).scale, 0.5);
    }

    #[test]
    fn a_helper_sees_what_its_engine_publishes_next() {
        let service_name = a_board_service_name();
        let engine_node = Iceoryx2Node::for_this_test_process();
        let first = StreamClock::starting_at(
            StreamClockSpec::Scaled {
                scale: 0.5,
                step_ns: 1_000,
            },
            10,
        )
        .published();
        let board = engine_node
            .create_stream_clock_board(&service_name, first)
            .expect("the engine creates its stream's clock board");
        let reader = Iceoryx2Node::for_this_test_process()
            .open_stream_clock_board_reader(&service_name)
            .expect("a helper opens the board its engine named");

        let next = streamlib_ipc_types::StreamClockBoardEntry {
            base_ns: 99,
            anchor_ns: 77,
            scale_f64_bits: 0.25f64.to_bits(),
            ..first
        };
        board.publish(next);

        assert_eq!(reader.read(), next);
    }

    #[test]
    fn a_helper_listening_for_a_publish_wakes_when_its_engine_publishes() {
        let service_name = a_board_service_name();
        let engine_node = Iceoryx2Node::for_this_test_process();
        let first = StreamClock::starting_at(
            StreamClockSpec::Stepped {
                step_ns: 1_000,
                pace: None,
                stop_after_steps: None,
            },
            10,
        )
        .published();
        let board = engine_node
            .create_stream_clock_board(&service_name, first)
            .expect("the engine creates its stream's clock board");
        let listener = Iceoryx2Node::for_this_test_process()
            .listen_for_each_stream_clock_board_publish(&service_name)
            .expect("a helper listens on the board its engine named");
        let is_readable_within = |timeout_ms: i32| {
            let mut poll_descriptor = libc::pollfd {
                fd: listener.file_descriptor(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one live descriptor, counted as one.
            unsafe { libc::poll(&mut poll_descriptor, 1, timeout_ms) == 1 }
        };
        assert!(!is_readable_within(0));

        board.publish(streamlib_ipc_types::StreamClockBoardEntry {
            step_index: 1,
            ..first
        });
        assert!(is_readable_within(1_000));
        listener.take_every_publish_noted();
        assert!(
            !is_readable_within(0),
            "a publish taken wakes nothing again"
        );
    }
}
