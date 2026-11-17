// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const INTERVAL: Duration = Duration::from_millis(33);

/// Sixty changes a second to one input are sent no closer than the interval apart, the
/// newest held each time, and the last change is the last one sent.
///
/// Mental-revert: sending each change as heard sends all sixty; keeping the first held
/// instead of the newest ends a follower on a stale value; dropping the held one at the end
/// leaves the follower one change behind for good.
#[test]
fn one_key_is_sent_at_most_once_each_interval_and_ends_on_its_last_value() {
    let start = Instant::now();
    let mut coalesced = NotificationsSentAtMostOnceEachInterval::new(INTERVAL);
    let mut sent: Vec<(Instant, u32)> = Vec::new();
    let tick = Duration::from_micros(16_667);
    for change in 0..60u32 {
        let now = start + tick * change;
        for due in coalesced.take_the_due(now) {
            sent.push((now, due));
        }
        if let Some(sent_now) = coalesced.heard("gain", change, now) {
            sent.push((now, sent_now));
        }
    }
    let after_the_last = start + tick * 60 + INTERVAL;
    assert!(
        coalesced
            .next_due_at()
            .is_some_and(|due| due <= after_the_last)
    );
    for due in coalesced.take_the_due(after_the_last) {
        sent.push((after_the_last, due));
    }

    assert_eq!(sent.first().map(|(_, change)| *change), Some(0));
    assert_eq!(sent.last().map(|(_, change)| *change), Some(59));
    assert!(sent.len() < 60 && sent.len() >= 25, "{} sent", sent.len());
    for pair in sent.windows(2) {
        assert!(pair[1].0 - pair[0].0 >= INTERVAL, "{pair:?}");
        assert!(pair[1].1 > pair[0].1, "{pair:?}");
    }
    assert_eq!(coalesced.next_due_at(), None);
}

/// Two keys are held apart: a change to one never delays or replaces the other's.
///
/// Mental-revert: one interval for every key sends one input's change and drops the other's.
#[test]
fn each_key_has_an_interval_of_its_own() {
    let start = Instant::now();
    let mut coalesced = NotificationsSentAtMostOnceEachInterval::new(INTERVAL);
    assert_eq!(coalesced.heard("gain", 1, start), Some(1));
    assert_eq!(coalesced.heard("width", 2, start), Some(2));
    assert_eq!(coalesced.heard("gain", 3, start + INTERVAL / 2), None);
    assert_eq!(coalesced.next_due_at(), Some(start + INTERVAL));
    assert_eq!(coalesced.take_the_due(start + INTERVAL), vec![3]);
    assert_eq!(coalesced.heard("width", 4, start + INTERVAL), Some(4));
}

/// A key sent once and heard no more is forgotten once its interval runs, though nothing is held
/// and no held notification falls due.
#[test]
fn a_key_heard_once_is_forgotten_once_its_interval_runs() {
    let start = Instant::now();
    let mut coalesced = NotificationsSentAtMostOnceEachInterval::new(INTERVAL);
    for (index, key) in ["gain", "pan", "mix"].into_iter().enumerate() {
        coalesced.heard(key, index, start);
    }
    coalesced.heard("gain", 3, start + INTERVAL * 2);
    assert_eq!(coalesced.last_sent_at.len(), 1);
}

/// The intervals the contract names for `input_value_changed`, and for `edited` sent for a value
/// change, are the ones a connection keeps.
#[test]
fn the_contract_names_the_intervals_a_connection_keeps() {
    let contract = streamlib_control_contract::control_contract_file_contents();
    for interval in [
        ONE_INPUT_VALUE_CHANGE_SENT_AT_MOST_EVERY,
        A_VALUE_CHANGE_TOLD_AS_EDITED_AT_MOST_EVERY,
    ] {
        let named = format!("at most once each {} ms", interval.as_millis());
        assert!(
            contract.contains(&named),
            "the contract does not say {named}"
        );
    }
}
