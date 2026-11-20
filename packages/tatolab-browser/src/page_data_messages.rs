// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The one message shape that crosses the page boundary: a `PageMessage`, one
//! JSON object in UTF-8. A message on `data` becomes the detail of a DOM event,
//! and the object a page publishes is written to `events` as one. The `data`
//! messages held while the browser host is away wait here too.

use std::collections::VecDeque;

use serde_json::Value as JsonValue;

/// The JSON text of the `detail` one `data` message reaches the page with.
///
/// The payload is one `PageMessage`: a JSON object, nothing after it.
pub fn page_data_detail_json_of_a_page_message(payload: &[u8]) -> Result<String, String> {
    let value: JsonValue = serde_json::from_slice(payload)
        .map_err(|unreadable| format!("a data message is not one JSON object: {unreadable}"))?;
    if !value.is_object() {
        return Err(format!("a data message is one JSON object, not {value}"));
    }
    Ok(value.to_string())
}

/// The `PageMessage` one `window.tatolab.publish(object)` call is written to
/// `events` as.
pub fn events_page_message_of_published_json(published_json: &str) -> Result<Vec<u8>, String> {
    let value: JsonValue = serde_json::from_str(published_json)
        .map_err(|unparsable| format!("a published object is not JSON: {unparsable}"))?;
    if !value.is_object() {
        return Err(format!(
            "a published object is one JSON object, not {value}"
        ));
    }
    Ok(value.to_string().into_bytes())
}

/// How many `data` messages wait for a restarting browser host before the oldest is dropped.
const MOST_DATA_MESSAGES_HELD_WHILE_THE_BROWSER_HOST_IS_AWAY: usize = 256;
/// How many bytes of `data` JSON a node holds while the host is away.
const MOST_DATA_BYTES_HELD_WHILE_THE_BROWSER_HOST_IS_AWAY: usize = 64 * 1024 * 1024;

/// The newest `data` messages, as page JSON, waiting for the host: at most
/// [`MOST_DATA_MESSAGES_HELD_WHILE_THE_BROWSER_HOST_IS_AWAY`] of them and
/// [`MOST_DATA_BYTES_HELD_WHILE_THE_BROWSER_HOST_IS_AWAY`] bytes, the newest
/// one always kept.
#[derive(Default)]
pub(crate) struct DataMessagesHeldForTheBrowserHost {
    detail_jsons: VecDeque<String>,
    bytes_held: usize,
}

impl DataMessagesHeldForTheBrowserHost {
    /// Hold `detail_json`, dropping the oldest past either bound; returns how
    /// many were dropped.
    pub(crate) fn hold_newest(&mut self, detail_json: String) -> usize {
        self.bytes_held += detail_json.len();
        self.detail_jsons.push_back(detail_json);
        let mut dropped = 0;
        while self.detail_jsons.len() > 1
            && (self.detail_jsons.len() > MOST_DATA_MESSAGES_HELD_WHILE_THE_BROWSER_HOST_IS_AWAY
                || self.bytes_held > MOST_DATA_BYTES_HELD_WHILE_THE_BROWSER_HOST_IS_AWAY)
        {
            self.take_oldest();
            dropped += 1;
        }
        dropped
    }

    pub(crate) fn take_oldest(&mut self) -> Option<String> {
        let oldest = self.detail_jsons.pop_front()?;
        self.bytes_held -= oldest.len();
        Some(oldest)
    }

    pub(crate) fn put_back_as_oldest(&mut self, detail_json: String) {
        self.bytes_held += detail_json.len();
        self.detail_jsons.push_front(detail_json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_data_drops_the_oldest_past_either_bound_and_keeps_the_newest() {
        let mut held = DataMessagesHeldForTheBrowserHost::default();
        for index in 0..MOST_DATA_MESSAGES_HELD_WHILE_THE_BROWSER_HOST_IS_AWAY {
            assert_eq!(held.hold_newest(index.to_string()), 0);
        }
        assert_eq!(held.hold_newest("newest".to_string()), 1);
        assert_eq!(held.take_oldest().as_deref(), Some("1"));

        let mut held = DataMessagesHeldForTheBrowserHost::default();
        let half_the_bytes = "x".repeat(MOST_DATA_BYTES_HELD_WHILE_THE_BROWSER_HOST_IS_AWAY / 2);
        assert_eq!(held.hold_newest(half_the_bytes.clone()), 0);
        assert_eq!(held.hold_newest(half_the_bytes), 0);
        assert_eq!(held.hold_newest("past the bound".to_string()), 1);
        let larger_than_the_bound =
            "y".repeat(MOST_DATA_BYTES_HELD_WHILE_THE_BROWSER_HOST_IS_AWAY + 1);
        assert_eq!(held.hold_newest(larger_than_the_bound.clone()), 2);
        assert_eq!(held.take_oldest(), Some(larger_than_the_bound));
        assert_eq!(held.bytes_held, 0);
    }

    #[test]
    fn a_data_message_reaches_the_page_as_its_json_detail() {
        let data = serde_json::json!({
            "title": "Jane Doe",
            "variant": 2,
            "live": true,
            "ratio": 0.5,
            "tags": ["a", null],
        });
        let detail: JsonValue = serde_json::from_str(
            &page_data_detail_json_of_a_page_message(data.to_string().as_bytes()).unwrap(),
        )
        .unwrap();
        assert_eq!(detail, data);
    }

    #[test]
    fn a_data_message_that_is_not_one_object_is_refused_by_name() {
        let not_an_object = page_data_detail_json_of_a_page_message(br#""text""#)
            .expect_err("a string is not an object");
        assert!(not_an_object.contains("one JSON object"), "{not_an_object}");
        let two_objects =
            page_data_detail_json_of_a_page_message(b"{}{}").expect_err("two objects");
        assert!(two_objects.contains("not one JSON object"), "{two_objects}");
        let not_json = page_data_detail_json_of_a_page_message(&[0x81, 0xa1, b'a', 0x01])
            .expect_err("a msgpack map");
        assert!(not_json.contains("not one JSON object"), "{not_json}");
    }

    #[test]
    fn a_published_object_is_written_as_one_page_message() {
        let payload =
            events_page_message_of_published_json(r#"{"clicked":"cta","at":12.5,"n":-3}"#).unwrap();
        let written: JsonValue = serde_json::from_slice(&payload).unwrap();
        assert_eq!(
            written,
            serde_json::json!({"clicked": "cta", "at": 12.5, "n": -3})
        );
    }

    #[test]
    fn a_published_value_that_is_not_an_object_is_refused() {
        let refusal = events_page_message_of_published_json("[1,2]").expect_err("an array");
        assert!(refusal.contains("one JSON object"), "{refusal}");
    }
}
