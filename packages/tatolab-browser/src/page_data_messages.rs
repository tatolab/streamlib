// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The two message shapes that cross the page boundary: a msgpack map on
//! `data` becomes the JSON detail of a DOM event, and the JSON text a page
//! publishes becomes a msgpack map on `events`.

use rmpv::Value as MsgpackValue;
use serde_json::Value as JsonValue;

/// The JSON text of the `detail` one `data` message reaches the page with.
///
/// The payload is one msgpack map. Binary values become arrays of byte
/// values, extension values are refused, and a float that JSON cannot hold
/// (NaN, an infinity) becomes `null`.
pub fn page_data_detail_json_of_msgpack_map(payload: &[u8]) -> Result<String, String> {
    let mut remaining = payload;
    let value = rmpv::decode::read_value(&mut remaining)
        .map_err(|undecodable| format!("a data message is not msgpack: {undecodable}"))?;
    if !remaining.is_empty() {
        return Err(format!(
            "a data message is one msgpack map; {} bytes follow it",
            remaining.len()
        ));
    }
    if !value.is_map() {
        return Err(format!("a data message is one msgpack map, not {value}"));
    }
    Ok(json_of_msgpack(&value)?.to_string())
}

fn json_of_msgpack(value: &MsgpackValue) -> Result<JsonValue, String> {
    Ok(match value {
        MsgpackValue::Nil => JsonValue::Null,
        MsgpackValue::Boolean(boolean) => JsonValue::Bool(*boolean),
        MsgpackValue::Integer(integer) => match (integer.as_i64(), integer.as_u64()) {
            (Some(signed), _) => JsonValue::from(signed),
            (None, Some(unsigned)) => JsonValue::from(unsigned),
            (None, None) => JsonValue::Null,
        },
        MsgpackValue::F32(float) => float_json(f64::from(*float)),
        MsgpackValue::F64(float) => float_json(*float),
        MsgpackValue::String(text) => JsonValue::String(
            text.as_str()
                .ok_or_else(|| "a data message holds a string that is not UTF-8".to_string())?
                .to_string(),
        ),
        MsgpackValue::Binary(bytes) => {
            JsonValue::Array(bytes.iter().map(|byte| JsonValue::from(*byte)).collect())
        }
        MsgpackValue::Array(items) => JsonValue::Array(
            items
                .iter()
                .map(json_of_msgpack)
                .collect::<Result<_, _>>()?,
        ),
        MsgpackValue::Map(entries) => {
            let mut object = serde_json::Map::with_capacity(entries.len());
            for (key, entry) in entries {
                let key = match key {
                    MsgpackValue::String(text) => text
                        .as_str()
                        .ok_or_else(|| "a data message holds a key that is not UTF-8".to_string())?
                        .to_string(),
                    MsgpackValue::Integer(integer) => integer.to_string(),
                    other => {
                        return Err(format!(
                            "a data message's map keys are strings or integers, not {other}"
                        ));
                    }
                };
                object.insert(key, json_of_msgpack(entry)?);
            }
            JsonValue::Object(object)
        }
        MsgpackValue::Ext(type_tag, _) => {
            return Err(format!(
                "a data message holds msgpack extension type {type_tag}, which a page cannot read"
            ));
        }
    })
}

fn float_json(float: f64) -> JsonValue {
    serde_json::Number::from_f64(float)
        .map(JsonValue::Number)
        .unwrap_or(JsonValue::Null)
}

/// The msgpack map one `window.tatolab.publish(object)` call is written to
/// `events` as.
pub fn events_msgpack_map_of_published_json(published_json: &str) -> Result<Vec<u8>, String> {
    let value: JsonValue = serde_json::from_str(published_json)
        .map_err(|unparsable| format!("a published object is not JSON: {unparsable}"))?;
    if !value.is_object() {
        return Err(format!(
            "a published object is one JSON object, not {value}"
        ));
    }
    let mut payload = Vec::with_capacity(published_json.len());
    rmpv::encode::write_value(&mut payload, &msgpack_of_json(&value))
        .map_err(|unwritable| format!("a published object did not encode: {unwritable}"))?;
    Ok(payload)
}

fn msgpack_of_json(value: &JsonValue) -> MsgpackValue {
    match value {
        JsonValue::Null => MsgpackValue::Nil,
        JsonValue::Bool(boolean) => MsgpackValue::Boolean(*boolean),
        JsonValue::Number(number) => match (number.as_i64(), number.as_u64(), number.as_f64()) {
            (Some(signed), _, _) => MsgpackValue::from(signed),
            (None, Some(unsigned), _) => MsgpackValue::from(unsigned),
            (None, None, Some(float)) => MsgpackValue::F64(float),
            (None, None, None) => MsgpackValue::Nil,
        },
        JsonValue::String(text) => MsgpackValue::from(text.as_str()),
        JsonValue::Array(items) => MsgpackValue::Array(items.iter().map(msgpack_of_json).collect()),
        JsonValue::Object(entries) => MsgpackValue::Map(
            entries
                .iter()
                .map(|(key, entry)| (MsgpackValue::from(key.as_str()), msgpack_of_json(entry)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgpack(value: &MsgpackValue) -> Vec<u8> {
        let mut payload = Vec::new();
        rmpv::encode::write_value(&mut payload, value).unwrap();
        payload
    }

    #[test]
    fn a_data_map_reaches_the_page_as_its_json_detail() {
        let data = MsgpackValue::Map(vec![
            ("title".into(), "Jane Doe".into()),
            ("variant".into(), 2.into()),
            ("live".into(), true.into()),
            ("ratio".into(), MsgpackValue::F64(0.5)),
            (
                "tags".into(),
                MsgpackValue::Array(vec!["a".into(), MsgpackValue::Nil]),
            ),
            ("raw".into(), MsgpackValue::Binary(vec![1, 255])),
            (7.into(), MsgpackValue::F64(f64::NAN)),
        ]);
        let detail: JsonValue =
            serde_json::from_str(&page_data_detail_json_of_msgpack_map(&msgpack(&data)).unwrap())
                .unwrap();
        assert_eq!(
            detail,
            serde_json::json!({
                "title": "Jane Doe",
                "variant": 2,
                "live": true,
                "ratio": 0.5,
                "tags": ["a", null],
                "raw": [1, 255],
                "7": null,
            })
        );
    }

    #[test]
    fn a_data_message_that_is_not_one_map_is_refused_by_name() {
        let not_a_map = page_data_detail_json_of_msgpack_map(&msgpack(&"text".into()))
            .expect_err("a string is not a map");
        assert!(not_a_map.contains("one msgpack map"), "{not_a_map}");
        let mut two_maps = msgpack(&MsgpackValue::Map(vec![]));
        two_maps.extend(msgpack(&MsgpackValue::Map(vec![])));
        let trailing = page_data_detail_json_of_msgpack_map(&two_maps).expect_err("two maps");
        assert!(trailing.contains("1 bytes follow"), "{trailing}");
        let extension = page_data_detail_json_of_msgpack_map(&msgpack(&MsgpackValue::Map(vec![(
            "when".into(),
            MsgpackValue::Ext(-1, vec![0; 4]),
        )])))
        .expect_err("an extension value");
        assert!(extension.contains("extension type -1"), "{extension}");
    }

    #[test]
    fn a_published_object_is_written_as_one_msgpack_map() {
        let payload =
            events_msgpack_map_of_published_json(r#"{"clicked":"cta","at":12.5,"n":-3}"#).unwrap();
        let written = rmpv::decode::read_value(&mut payload.as_slice()).unwrap();
        let entries = written.as_map().expect("a map");
        let entry = |key: &str| {
            entries
                .iter()
                .find(|(entry_key, _)| entry_key.as_str() == Some(key))
                .map(|(_, value)| value.clone())
                .unwrap()
        };
        assert_eq!(entry("clicked").as_str(), Some("cta"));
        assert_eq!(entry("at").as_f64(), Some(12.5));
        assert_eq!(entry("n").as_i64(), Some(-3));
    }

    #[test]
    fn a_published_value_that_is_not_an_object_is_refused() {
        let refusal = events_msgpack_map_of_published_json("[1,2]").expect_err("an array");
        assert!(refusal.contains("one JSON object"), "{refusal}");
    }
}
