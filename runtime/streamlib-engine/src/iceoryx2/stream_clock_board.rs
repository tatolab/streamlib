// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The blackboard a scaled or stepped stream publishes its clock on, and what
//! each of its helper processes reads stream time off.
//!
//! One board per stream start: the engine creates it before any helper of the
//! stream spawns and is its one writer; every helper opens it by the service
//! name its `setup` command carries and reads its one entry whole.

use iceoryx2::port::reader::{EntryHandle, Reader};
use iceoryx2::port::writer::{EntryHandleMut, Writer};
use iceoryx2::prelude::*;
use iceoryx2::service::port_factory::blackboard::PortFactory as BlackboardPortFactory;
use parking_lot::Mutex;
use streamlib_ipc_types::{StreamClockBoardEntry, StreamClockBoardKey};

use crate::core::error::{Error, Result};

/// The blackboard service a stream clock board is.
pub(crate) type StreamClockBoardService = BlackboardPortFactory<ipc::Service, StreamClockBoardKey>;

// =============================================================================
// The engine's board
// =============================================================================

/// A stream's clock board as its engine holds it: the service it created, and
/// the one write handle on its entry.
pub struct StreamClockBoard {
    service_name: String,
    board_write_handles: Mutex<StreamClockBoardWriteHandles>,
}

/// The writer and its entry handle, dropped together.
///
/// Fields drop in declaration order: the handle, the writer it came from, then
/// the service.
struct StreamClockBoardWriteHandles {
    entry: EntryHandleMut<ipc::Service, StreamClockBoardKey, StreamClockBoardEntry>,
    _writer: Writer<ipc::Service, StreamClockBoardKey>,
    _service: StreamClockBoardService,
}

// SAFETY: as for the loss-count board's write handles: `Writer` holds
// iceoryx2's `SingleThreaded` `Rc`, the entry handle a clone of it, and both
// are made on one thread and dropped together in one `Drop`. A write,
// `EntryHandleMut::update_with_copy`, stores through the entry's single
// producer, which the surrounding `Mutex` serialises, and never touches the
// `Rc`. Re-check this against the source on any iceoryx2 upgrade.
unsafe impl Send for StreamClockBoardWriteHandles {}

impl StreamClockBoard {
    /// Take the write handle of the board `service` just created.
    pub(crate) fn writing(service_name: String, service: StreamClockBoardService) -> Result<Self> {
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
        Ok(Self {
            service_name,
            board_write_handles: Mutex::new(StreamClockBoardWriteHandles {
                entry,
                _writer: writer,
                _service: service,
            }),
        })
    }

    /// The name a helper opens this board by.
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// Publish `entry` as the stream's clock to every reader.
    pub fn publish(&self, entry: StreamClockBoardEntry) {
        self.board_write_handles
            .lock()
            .entry
            .update_with_copy(entry);
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
    entry: EntryHandle<ipc::Service, StreamClockBoardKey, StreamClockBoardEntry>,
    _reader: Reader<ipc::Service, StreamClockBoardKey>,
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
}
