// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A loaded stream's records numbered in the order its route appended them,
//! and the most recent of them held in memory for a reader to page through by
//! sequence number.

use std::collections::VecDeque;

use serde::Serialize;

/// How many of a stream's most recent records its route holds in memory.
pub const LOADED_STREAM_LOG_RECORDS_HELD_IN_MEMORY: usize = 4096;

/// One record of a stream's log and the number its route gave it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NumberedLogRecord {
    /// The record's place in its stream's log, counted from 1.
    pub sequence: u64,
    /// The record as its stream's JSONL file holds it.
    pub record: serde_json::Value,
}

/// The records of a stream's log after a sequence number, as one read found
/// them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LoadedStreamLogRecordsPage {
    /// The records read, in order.
    pub records: Vec<NumberedLogRecord>,
    /// The sequence number the next read passes as its `after`.
    pub next_after: u64,
    /// How many records after the `after` asked for were dropped from the
    /// in-memory history before this read.
    pub records_no_longer_held: u64,
}

/// A stream's records numbered from 1, the most recent
/// [`LOADED_STREAM_LOG_RECORDS_HELD_IN_MEMORY`] of them held serialized.
pub(crate) struct LoadedStreamLogRecordHistory {
    held_record_capacity: usize,
    next_sequence: u64,
    held_serialized_records: VecDeque<Box<str>>,
}

impl Default for LoadedStreamLogRecordHistory {
    fn default() -> Self {
        Self::holding_at_most(LOADED_STREAM_LOG_RECORDS_HELD_IN_MEMORY)
    }
}

impl LoadedStreamLogRecordHistory {
    /// An empty history holding at most `held_record_capacity` records.
    pub(crate) fn holding_at_most(held_record_capacity: usize) -> Self {
        Self {
            held_record_capacity,
            next_sequence: 1,
            held_serialized_records: VecDeque::with_capacity(held_record_capacity.min(1024)),
        }
    }

    /// Number `serialized_record` and hold it, dropping the oldest held
    /// record once the history is full; the number it was given.
    pub(crate) fn append(&mut self, serialized_record: &[u8]) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        if self.held_record_capacity == 0 {
            return sequence;
        }
        if self.held_serialized_records.len() == self.held_record_capacity {
            self.held_serialized_records.pop_front();
        }
        self.held_serialized_records
            .push_back(String::from_utf8_lossy(serialized_record).into());
        sequence
    }

    /// The held records numbered after `after`, at most `max_count` of them.
    pub(crate) fn records_after(&self, after: u64, max_count: usize) -> LoadedStreamLogRecordsPage {
        let first_held_sequence = self.next_sequence - self.held_serialized_records.len() as u64;
        let first_sequence_asked_for = after.saturating_add(1);
        let records_no_longer_held = first_held_sequence.saturating_sub(first_sequence_asked_for);
        let first_sequence_read = first_sequence_asked_for.max(first_held_sequence);
        let records: Vec<NumberedLogRecord> = self
            .held_serialized_records
            .iter()
            .zip(first_held_sequence..)
            .skip_while(|(_, sequence)| *sequence < first_sequence_read)
            .take(max_count)
            .map(|(serialized_record, sequence)| NumberedLogRecord {
                sequence,
                record: serde_json::from_str(serialized_record)
                    .unwrap_or_else(|_| serde_json::Value::String(serialized_record.to_string())),
            })
            .collect();
        let next_after = records
            .last()
            .map(|last| last.sequence)
            .unwrap_or_else(|| after.max(first_sequence_read.saturating_sub(1)));
        LoadedStreamLogRecordsPage {
            records,
            next_after,
            records_no_longer_held,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_history_of(
        record_count: u64,
        held_record_capacity: usize,
    ) -> LoadedStreamLogRecordHistory {
        let mut history = LoadedStreamLogRecordHistory::holding_at_most(held_record_capacity);
        for record_index in 1..=record_count {
            history.append(format!("{{\"message\":\"record {record_index}\"}}").as_bytes());
        }
        history
    }

    fn sequences_of(page: &LoadedStreamLogRecordsPage) -> Vec<u64> {
        page.records.iter().map(|record| record.sequence).collect()
    }

    #[test]
    fn records_are_numbered_from_one_in_the_order_they_were_appended() {
        let mut history = LoadedStreamLogRecordHistory::default();

        assert_eq!(history.append(br#"{"message":"first"}"#), 1);
        assert_eq!(history.append(br#"{"message":"second"}"#), 2);

        let page = history.records_after(0, 256);
        assert_eq!(
            page.records,
            [
                NumberedLogRecord {
                    sequence: 1,
                    record: serde_json::json!({"message": "first"}),
                },
                NumberedLogRecord {
                    sequence: 2,
                    record: serde_json::json!({"message": "second"}),
                },
            ]
        );
        assert_eq!(page.next_after, 2);
        assert_eq!(page.records_no_longer_held, 0);
    }

    #[test]
    fn a_reader_pages_through_by_passing_each_next_after_back_and_reads_no_record_twice() {
        let history = a_history_of(7, 4096);

        let first = history.records_after(0, 3);
        let second = history.records_after(first.next_after, 3);
        let third = history.records_after(second.next_after, 3);
        let caught_up = history.records_after(third.next_after, 3);

        assert_eq!(sequences_of(&first), [1, 2, 3]);
        assert_eq!(sequences_of(&second), [4, 5, 6]);
        assert_eq!(sequences_of(&third), [7]);
        assert!(caught_up.records.is_empty());
        assert_eq!(
            caught_up.next_after, 7,
            "a caught-up reader stays where it is"
        );
        assert_eq!(third.records[0].record["message"], "record 7");
    }

    #[test]
    fn a_read_returns_at_most_max_count_records() {
        let history = a_history_of(10, 4096);

        assert_eq!(sequences_of(&history.records_after(2, 4)), [3, 4, 5, 6]);
        let none = history.records_after(2, 0);
        assert!(none.records.is_empty());
        assert_eq!(none.next_after, 2);
    }

    #[test]
    fn once_the_history_wraps_a_read_counts_the_records_it_no_longer_holds_and_resumes_after_them()
    {
        let history = a_history_of(4106, LOADED_STREAM_LOG_RECORDS_HELD_IN_MEMORY);

        let from_the_start = history.records_after(0, 2);
        assert_eq!(from_the_start.records_no_longer_held, 10);
        assert_eq!(sequences_of(&from_the_start), [11, 12]);
        assert_eq!(from_the_start.records[0].record["message"], "record 11");

        let partly_dropped = history.records_after(5, 1);
        assert_eq!(partly_dropped.records_no_longer_held, 5);
        assert_eq!(sequences_of(&partly_dropped), [11]);

        let all_held = history.records_after(4100, 256);
        assert_eq!(all_held.records_no_longer_held, 0);
        assert_eq!(
            sequences_of(&all_held),
            [4101, 4102, 4103, 4104, 4105, 4106]
        );
    }

    #[test]
    fn a_reader_ahead_of_the_log_reads_nothing_and_keeps_its_place() {
        let history = a_history_of(3, 4096);

        let page = history.records_after(9, 256);

        assert!(page.records.is_empty());
        assert_eq!(page.next_after, 9);
        assert_eq!(page.records_no_longer_held, 0);
    }
}
