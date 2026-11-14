// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `data` scripts that reach a tab before its page can listen for them.

/// How many `data` messages wait for the page's document before the oldest is dropped.
pub const MOST_DATA_SCRIPTS_AWAITING_THE_LOADED_DOCUMENT: usize = 256;
/// How many bytes of `data` scripts wait for the page's document before the oldest is dropped.
pub const MOST_DATA_SCRIPT_BYTES_AWAITING_THE_LOADED_DOCUMENT: usize = 64 * 1024 * 1024;

/// `data` dispatch scripts that arrived before the main frame finished loading
/// its document, held in arrival order until it has.
#[derive(Debug, Default)]
pub struct PageDataScriptsAwaitingTheLoadedDocument {
    scripts_in_arrival_order: std::collections::VecDeque<String>,
    bytes_held: usize,
}

impl PageDataScriptsAwaitingTheLoadedDocument {
    /// Hold `script`; `true` when older held scripts were dropped to make room.
    pub fn hold(&mut self, script: String) -> bool {
        self.bytes_held += script.len();
        self.scripts_in_arrival_order.push_back(script);
        let mut dropped_older_scripts = false;
        while self.scripts_in_arrival_order.len() > MOST_DATA_SCRIPTS_AWAITING_THE_LOADED_DOCUMENT
            || (self.bytes_held > MOST_DATA_SCRIPT_BYTES_AWAITING_THE_LOADED_DOCUMENT
                && self.scripts_in_arrival_order.len() > 1)
        {
            if let Some(oldest) = self.scripts_in_arrival_order.pop_front() {
                self.bytes_held -= oldest.len();
                dropped_older_scripts = true;
            }
        }
        dropped_older_scripts
    }

    /// Every held script, oldest first, leaving none held.
    pub fn take_in_arrival_order(&mut self) -> std::collections::VecDeque<String> {
        self.bytes_held = 0;
        std::mem::take(&mut self.scripts_in_arrival_order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_data_scripts_come_back_in_arrival_order_and_once() {
        let mut awaiting = PageDataScriptsAwaitingTheLoadedDocument::default();
        assert!(!awaiting.hold("first".to_string()));
        assert!(!awaiting.hold("second".to_string()));

        assert_eq!(
            Vec::from(awaiting.take_in_arrival_order()),
            vec!["first".to_string(), "second".to_string()]
        );
        assert!(awaiting.take_in_arrival_order().is_empty());
    }

    #[test]
    fn a_full_hold_drops_its_oldest_script() {
        let mut awaiting = PageDataScriptsAwaitingTheLoadedDocument::default();
        for index in 0..MOST_DATA_SCRIPTS_AWAITING_THE_LOADED_DOCUMENT {
            assert!(!awaiting.hold(index.to_string()));
        }

        assert!(awaiting.hold("newest".to_string()));

        let held = awaiting.take_in_arrival_order();
        assert_eq!(held.len(), MOST_DATA_SCRIPTS_AWAITING_THE_LOADED_DOCUMENT);
        assert_eq!(held.front().map(String::as_str), Some("1"));
        assert_eq!(held.back().map(String::as_str), Some("newest"));
    }

    #[test]
    fn held_scripts_past_the_byte_bound_drop_the_oldest() {
        let mut awaiting = PageDataScriptsAwaitingTheLoadedDocument::default();
        let a_third_of_the_byte_bound = MOST_DATA_SCRIPT_BYTES_AWAITING_THE_LOADED_DOCUMENT / 3;
        for marker in ['a', 'b', 'c'] {
            assert!(!awaiting.hold(marker.to_string().repeat(a_third_of_the_byte_bound)));
        }

        assert!(awaiting.hold("d".repeat(a_third_of_the_byte_bound)));

        let held = awaiting.take_in_arrival_order();
        assert_eq!(held.len(), 3);
        assert!(held.front().is_some_and(|oldest| oldest.starts_with('b')));
    }
}
