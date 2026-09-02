// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The window contract an audio input port may declare beside its delivery
//! profile — the rate, dtype, window size and hop it wants, and the channel
//! count it wants only if it needs a particular one.
//!
//! Declaration only: this type is what an author writes and what `graph`
//! renders. Resampling, mixdown and framing are the read-side stage's, and it
//! is driven by exactly this.
//!
//! The integers and the dtype string are deliberately plain rather than the
//! engine's `AudioStreamFormat` / `AudioSampleFormat`: the meanings match, but
//! this crate is engine-free and the authoring chain must not link the engine
//! to declare a port.

use serde::{Deserialize, Serialize};

/// The values an `audio_window` declaration's `dtype` may take — the two
/// `AudioBlock` legalises.
///
/// The single Rust-side list: [`AudioWindowContractDeclaredValues::refuse_if_unhonourable`]
/// renders its refusal from it, and that is the one validator both the
/// `#[processor]` grammar and the wheel's declaration bridge call.
pub const AUDIO_WINDOW_DTYPE_DECLARATION_VALUES: [&str; 2] = ["f32", "i16"];

/// How an undeclared channel count renders wherever a contract is written
/// down — `graph`, the parent→child wiring envelope, and the dict a Python
/// author's declaration becomes.
///
/// Spelled rather than omitted. A missing key and a `null` both read as
/// "nothing was said here", which is indistinguishable from a writer that
/// forgot the field; `"source"` says the count follows whatever the source
/// sends, which is the whole of what an absent count means.
pub const AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE: &str = "source";

/// A window contract on an audio input port: either the values the port
/// declared, or the sentinel that resolves them from the device.
///
/// All-or-nothing but for the channel count, which is the one value a port may
/// leave to its source. Everything else has no partial form, because a
/// half-declared contract leaves the read-side stage guessing at exactly the
/// values a model asserts on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(tag = "resolved_from", deny_unknown_fields)]
pub enum AudioWindowContract {
    /// The values as the port itself declared them.
    #[serde(rename = "declaration")]
    Declaration(AudioWindowContractDeclaredValues),
    /// `audio_window = match_device`: the five values resolve at `setup()`
    /// from the format of the device stream the declaring processor opened.
    /// Only a processor that opens one can satisfy it.
    ///
    /// The empty braces are load-bearing. Under an internally-tagged enum a
    /// *unit* variant accepts and silently discards sibling keys, so
    /// `{"resolved_from": "match_device", "window_size": 512}` would read back
    /// as a bare sentinel; an empty struct variant refuses it.
    #[serde(rename = "match_device")]
    MatchDevice {},
    /// The five values a [`AudioWindowContract::MatchDevice`] port settled to,
    /// taken from the format of the device stream its processor opened.
    ///
    /// Rendering only — no authoring surface produces it. The `#[processor]`
    /// grammar and the wheel's declaration bridge each yield
    /// [`AudioWindowContract::Declaration`] or
    /// [`AudioWindowContract::MatchDevice`] and nothing else, so an author
    /// cannot write it. It exists because a port that resolved 48 kHz stereo
    /// from *this machine's* speaker and one whose author wrote those numbers
    /// down are not the same fact, and `graph` is where the difference is
    /// readable: a reader round-tripping the rendering back into a declaration
    /// would otherwise pin a machine-specific format as if it had been chosen.
    #[serde(rename = "device")]
    Device(AudioWindowContractDeclaredValues),
}

/// The values an `audio_window` declaration states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct AudioWindowContractDeclaredValues {
    /// Output sample rate in hertz.
    pub sample_rate: u32,
    /// Output channel count every emitted window is interleaved by, or `None`
    /// — the source's own count, whatever it is.
    ///
    /// The one value a contract may leave unstated. Absent, the stage skips
    /// channel conversion and every emitted window carries the count the block
    /// arrived with, so a consumer reads `channels` off the block rather than
    /// assuming it. A consumer that needs a particular count — a model trained
    /// on mono — declares one and is converted to it.
    #[serde(
        default,
        with = "channel_count_declared_or_following_the_source",
    )]
    #[schemars(schema_with = "audio_window_channels_json_schema")]
    pub channels: Option<u32>,
    /// How to read the scalars an emitted window carries — `"f32"` or
    /// `"i16"`.
    pub dtype: String,
    /// Per-channel samples in one emitted window, the unit
    /// `AudioBlock.sample_count` uses: a window carries
    /// `window_size × channels` scalars.
    pub window_size: u32,
    /// Per-channel samples between the starts of consecutive windows. Equal to
    /// `window_size` for contiguous windows, below it for a rolling one, and
    /// never above it.
    pub hop: u32,
}

impl AudioWindowContractDeclaredValues {
    /// Refuse a declaration the read-side stage could not honour, naming the
    /// field and the value.
    ///
    /// The one validator both Rust entry paths call — the `#[processor]`
    /// grammar and the wheel's bridge from a Python declaration — so a
    /// declaration is refused in the same terms whichever language wrote it.
    /// Deserializing a contract or building one through the public fields
    /// runs no check; the two authoring surfaces are what this guards.
    pub fn refuse_if_unhonourable(&self) -> Result<(), String> {
        for (field_name, value) in [
            ("sample_rate", Some(self.sample_rate)),
            ("channels", self.channels),
            ("window_size", Some(self.window_size)),
            ("hop", Some(self.hop)),
        ]
        .into_iter()
        .filter_map(|(field_name, value)| value.map(|value| (field_name, value)))
        {
            if value == 0 {
                return Err(format!(
                    "`audio_window` field `{field_name}` is {value} — every numeric field \
                     is strictly positive"
                ));
            }
        }

        if !AUDIO_WINDOW_DTYPE_DECLARATION_VALUES.contains(&self.dtype.as_str()) {
            return Err(format!(
                "`audio_window` field `dtype` is `\"{}\"` — expected one of {}, the two an \
                 audio block legalises",
                self.dtype,
                render_declaration_values(&AUDIO_WINDOW_DTYPE_DECLARATION_VALUES),
            ));
        }

        if self.hop > self.window_size {
            return Err(format!(
                "`audio_window` declares `hop` {} above `window_size` {} — a hop above the \
                 window silently discards the samples between windows. A hop below it is a \
                 rolling window and is legal; omitting it makes windows contiguous",
                self.hop, self.window_size,
            ));
        }

        Ok(())
    }
}

/// `channels` as it crosses a wire: the declared count as an integer, or
/// [`AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE`] for a port that takes
/// whatever its source sends.
///
/// Hand-written rather than left to `Option<u32>`'s own rendering, which would
/// write `null` — a value every JSON writer also produces by accident. A reader
/// that sees `"source"` knows the absence was meant.
mod channel_count_declared_or_following_the_source {
    use serde::de::{Unexpected, Visitor};
    use serde::{Deserializer, Serializer};

    use super::AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE;

    pub(super) fn serialize<S: Serializer>(
        channels: &Option<u32>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match channels {
            Some(channels) => serializer.serialize_u32(*channels),
            None => serializer.serialize_str(AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<u32>, D::Error> {
        deserializer.deserialize_any(ChannelCountOrTheSourceSpelling)
    }

    struct ChannelCountOrTheSourceSpelling;

    impl Visitor<'_> for ChannelCountOrTheSourceSpelling {
        type Value = Option<u32>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                formatter,
                "a channel count, or `\"{AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE}\"` to \
                 take the source's own"
            )
        }

        fn visit_u64<E: serde::de::Error>(self, channels: u64) -> Result<Self::Value, E> {
            u32::try_from(channels)
                .map(Some)
                .map_err(|_| E::invalid_value(Unexpected::Unsigned(channels), &self))
        }

        fn visit_i64<E: serde::de::Error>(self, channels: i64) -> Result<Self::Value, E> {
            u32::try_from(channels)
                .map(Some)
                .map_err(|_| E::invalid_value(Unexpected::Signed(channels), &self))
        }

        fn visit_str<E: serde::de::Error>(self, spelling: &str) -> Result<Self::Value, E> {
            if spelling == AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE {
                return Ok(None);
            }
            Err(E::invalid_value(Unexpected::Str(spelling), &self))
        }
    }
}

/// The JSON Schema for `channels`, matching what the field actually writes.
///
/// The derive would render `Option<u32>` as an integer-or-null, which is not a
/// shape this field ever takes.
fn audio_window_channels_json_schema(
    _: &mut schemars::r#gen::SchemaGenerator,
) -> schemars::schema::Schema {
    use schemars::schema::{InstanceType, NumberValidation, Schema, SchemaObject, SubschemaValidation};

    let declared_count = Schema::Object(SchemaObject {
        instance_type: Some(InstanceType::Integer.into()),
        number: Some(Box::new(NumberValidation {
            minimum: Some(1.0),
            ..Default::default()
        })),
        ..Default::default()
    });
    let follows_the_source = Schema::Object(SchemaObject {
        instance_type: Some(InstanceType::String.into()),
        enum_values: Some(vec![serde_json::Value::String(
            AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE.into(),
        )]),
        ..Default::default()
    });

    Schema::Object(SchemaObject {
        metadata: Some(Box::new(schemars::schema::Metadata {
            description: Some(
                "Output channel count, or `source` to carry whatever count the source sends."
                    .into(),
            ),
            ..Default::default()
        })),
        subschemas: Some(Box::new(SubschemaValidation {
            one_of: Some(vec![declared_count, follows_the_source]),
            ..Default::default()
        })),
        ..Default::default()
    })
}

/// A declaration vocabulary as a quoted, comma-joined list, for the refusal
/// that offers it.
///
/// Shared by every such list — the delivery profiles and the window dtypes —
/// so one spelling of "here are the legal values" serves all of them.
pub fn render_declaration_values(values: &[&str]) -> String {
    values
        .iter()
        .map(|value| format!("`\"{value}\"`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Refuse a window contract beside `delivery_profile = "newest"`, naming both
/// knobs.
///
/// Shared by the grammar and the wheel's bridge for the same reason the value
/// validator is.
pub fn refuse_audio_window_beside_a_skipping_delivery_profile(
    delivery_profile: Option<&str>,
) -> Result<(), String> {
    if delivery_profile == Some("newest") {
        return Err(
            "a port declaring `audio_window` must declare `delivery_profile = \"ordered\"`, \
             not `\"newest\"` — `newest` skips to the latest bag by design, and an \
             accumulator that needs contiguous samples would flush on nearly every read"
                .to_string(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared_values() -> AudioWindowContractDeclaredValues {
        AudioWindowContractDeclaredValues {
            sample_rate: 16_000,
            channels: Some(1),
            dtype: "f32".to_string(),
            window_size: 512,
            hop: 512,
        }
    }

    #[test]
    fn a_declared_contract_renders_the_five_values_beside_where_they_came_from() {
        let json = serde_json::to_value(AudioWindowContract::Declaration(declared_values()))
            .expect("a declared contract serializes");

        assert_eq!(
            json,
            serde_json::json!({
                "resolved_from": "declaration",
                "sample_rate": 16_000,
                "channels": 1,
                "dtype": "f32",
                "window_size": 512,
                "hop": 512,
            })
        );
    }

    #[test]
    fn the_sentinel_renders_as_a_whole_contract_carrying_no_values() {
        let json = serde_json::to_value(AudioWindowContract::MatchDevice {})
            .expect("the sentinel serializes");

        assert_eq!(json, serde_json::json!({ "resolved_from": "match_device" }));
    }

    /// A port that resolved its numbers from this machine's device and one
    /// whose author wrote them down render differently, because they are
    /// different facts — the whole reason `match_device` exists.
    #[test]
    fn values_settled_from_a_device_render_as_the_device_rather_than_as_a_declaration() {
        let settled = serde_json::to_value(AudioWindowContract::Device(declared_values()))
            .expect("a settled contract serializes");

        assert_eq!(settled["resolved_from"], "device");
        assert_eq!(settled["sample_rate"], 16_000);
        assert_ne!(
            settled,
            serde_json::to_value(AudioWindowContract::Declaration(declared_values()))
                .expect("a declared contract serializes"),
            "the two carry the same five values and must still be told apart"
        );
    }

    #[test]
    fn every_arm_round_trips_through_its_rendering() {
        for contract in [
            AudioWindowContract::Declaration(declared_values()),
            AudioWindowContract::MatchDevice {},
            AudioWindowContract::Device(declared_values()),
        ] {
            let rendered = serde_json::to_string(&contract).expect("serializes");
            let read_back: AudioWindowContract =
                serde_json::from_str(&rendered).expect("deserializes");
            assert_eq!(read_back, contract);
        }
    }

    /// The sentinel is whole-contract, never per-field: there is no
    /// inhabitant carrying both, so no reader has to decide which wins.
    #[test]
    fn a_contract_carrying_the_sentinel_and_values_at_once_is_not_representable() {
        let both = serde_json::json!({
            "resolved_from": "match_device",
            "sample_rate": 16_000,
            "channels": 1,
            "dtype": "f32",
            "window_size": 512,
            "hop": 512,
        });

        serde_json::from_value::<AudioWindowContract>(both)
            .expect_err("the sentinel arm takes no values");
    }

    #[test]
    fn an_unknown_field_is_refused_rather_than_ignored() {
        let with_a_stray_field = serde_json::json!({
            "resolved_from": "declaration",
            "sample_rate": 16_000,
            "channels": 1,
            "dtype": "f32",
            "window_size": 512,
            "hop": 512,
            "overlap": 128,
        });

        serde_json::from_value::<AudioWindowContract>(with_a_stray_field)
            .expect_err("a declared contract denies unknown fields");
    }

    #[test]
    fn a_partial_contract_does_not_deserialize() {
        let missing_window_size = serde_json::json!({
            "resolved_from": "declaration",
            "sample_rate": 16_000,
            "channels": 1,
            "dtype": "f32",
            "hop": 512,
        });

        serde_json::from_value::<AudioWindowContract>(missing_window_size)
            .expect_err("every declared value is required");
    }

    #[test]
    fn every_numeric_field_is_refused_at_zero_naming_the_field_and_the_value() {
        for field_name in ["sample_rate", "channels", "window_size", "hop"] {
            let mut values = declared_values();
            match field_name {
                "sample_rate" => values.sample_rate = 0,
                "channels" => values.channels = Some(0),
                "window_size" => values.window_size = 0,
                _ => values.hop = 0,
            }

            let refusal = values
                .refuse_if_unhonourable()
                .expect_err("a zero numeric field is refused");
            assert!(
                refusal.contains(field_name) && refusal.contains(" is 0 "),
                "the refusal must name the field and the value; got {refusal}"
            );
        }
    }

    #[test]
    fn a_hop_above_the_window_is_refused_naming_both_numbers() {
        let values = AudioWindowContractDeclaredValues {
            hop: 1_024,
            ..declared_values()
        };

        let refusal = values
            .refuse_if_unhonourable()
            .expect_err("a hop above the window is refused");
        assert!(
            refusal.contains("1024") && refusal.contains("512"),
            "the refusal must name both numbers; got {refusal}"
        );
    }

    #[test]
    fn a_hop_below_the_window_is_a_rolling_window_and_is_accepted() {
        let values = AudioWindowContractDeclaredValues {
            hop: 160,
            ..declared_values()
        };

        values
            .refuse_if_unhonourable()
            .expect("a rolling window is legal");
    }

    #[test]
    fn an_unknown_dtype_is_refused_listing_the_legal_values() {
        let values = AudioWindowContractDeclaredValues {
            dtype: "f64".to_string(),
            ..declared_values()
        };

        let refusal = values
            .refuse_if_unhonourable()
            .expect_err("an unknown dtype is refused");
        assert!(
            refusal.contains("f64") && refusal.contains("f32") && refusal.contains("i16"),
            "the refusal must name the value and the legal ones; got {refusal}"
        );
    }

    #[test]
    fn both_legal_dtypes_are_accepted() {
        for dtype in AUDIO_WINDOW_DTYPE_DECLARATION_VALUES {
            let values = AudioWindowContractDeclaredValues {
                dtype: dtype.to_string(),
                ..declared_values()
            };
            values
                .refuse_if_unhonourable()
                .unwrap_or_else(|refusal| panic!("`{dtype}` is legal; got {refusal}"));
        }
    }


    /// The rendering an absent count takes, spelled rather than omitted: a
    /// reader of `graph` learns the count follows the source instead of
    /// learning nothing.
    #[test]
    fn an_undeclared_channel_count_renders_as_following_the_source() {
        let json = serde_json::to_value(AudioWindowContract::Declaration(
            AudioWindowContractDeclaredValues {
                channels: None,
                ..declared_values()
            },
        ))
        .expect("a contract with no declared count serializes");

        assert_eq!(
            json,
            serde_json::json!({
                "resolved_from": "declaration",
                "sample_rate": 16_000,
                "channels": "source",
                "dtype": "f32",
                "window_size": 512,
                "hop": 512,
            })
        );
    }

    #[test]
    fn a_contract_following_the_source_round_trips_through_its_rendering() {
        let following_the_source = AudioWindowContract::Declaration(
            AudioWindowContractDeclaredValues {
                channels: None,
                ..declared_values()
            },
        );

        let rendered = serde_json::to_string(&following_the_source).expect("serializes");
        let read_back: AudioWindowContract =
            serde_json::from_str(&rendered).expect("deserializes");

        assert_eq!(read_back, following_the_source);
    }

    /// A writer that omits the key entirely means the same thing as one that
    /// spells it, so a hand-written declaration is not refused for terseness.
    #[test]
    fn an_omitted_channels_key_reads_as_following_the_source() {
        let without_the_key = serde_json::json!({
            "resolved_from": "declaration",
            "sample_rate": 16_000,
            "dtype": "f32",
            "window_size": 512,
            "hop": 512,
        });

        let read: AudioWindowContract =
            serde_json::from_value(without_the_key).expect("an omitted count is legal");

        assert_eq!(
            read,
            AudioWindowContract::Declaration(AudioWindowContractDeclaredValues {
                channels: None,
                ..declared_values()
            })
        );
    }

    /// `channels` is the one value that may be left out; the rest are refused
    /// exactly as before, so relaxing one field did not relax the contract.
    #[test]
    fn every_value_but_the_channel_count_is_still_required() {
        for omitted in ["sample_rate", "dtype", "window_size"] {
            let mut declaration = serde_json::json!({
                "resolved_from": "declaration",
                "sample_rate": 16_000,
                "channels": 1,
                "dtype": "f32",
                "window_size": 512,
                "hop": 512,
            });
            declaration
                .as_object_mut()
                .expect("an object")
                .remove(omitted);

            serde_json::from_value::<AudioWindowContract>(declaration).expect_err(&format!(
                "`{omitted}` is required and its absence must be refused"
            ));
        }
    }

    /// A string that is not the one spelling is a writer that meant something
    /// else, and guessing which count it meant is exactly the reshaping the
    /// contract refuses everywhere else.
    #[test]
    fn a_channels_value_that_is_neither_a_count_nor_the_source_spelling_is_refused() {
        for stray in [
            serde_json::json!("stereo"),
            serde_json::json!("2"),
            serde_json::json!(null),
            serde_json::json!(-1),
        ] {
            let declaration = serde_json::json!({
                "resolved_from": "declaration",
                "sample_rate": 16_000,
                "channels": stray,
                "dtype": "f32",
                "window_size": 512,
                "hop": 512,
            });

            serde_json::from_value::<AudioWindowContract>(declaration)
                .expect_err(&format!("`{stray}` names no channel count"));
        }
    }

    /// The relaxation is about a count nobody stated, never about a count
    /// stated wrong: a declared zero is as unhonourable as it ever was.
    #[test]
    fn a_declared_zero_is_still_refused_while_an_absent_count_is_accepted() {
        AudioWindowContractDeclaredValues {
            channels: None,
            ..declared_values()
        }
        .refuse_if_unhonourable()
        .expect("a contract may leave its count to the source");

        let refusal = AudioWindowContractDeclaredValues {
            channels: Some(0),
            ..declared_values()
        }
        .refuse_if_unhonourable()
        .expect_err("a declared zero is refused");
        assert!(
            refusal.contains("channels") && refusal.contains(" is 0 "),
            "the refusal must name the field and the value; got {refusal}"
        );
    }

    #[test]
    fn a_contract_beside_a_skipping_profile_is_refused_naming_both_knobs() {
        let refusal = refuse_audio_window_beside_a_skipping_delivery_profile(Some("newest"))
            .expect_err("a contract beside `newest` is refused");
        assert!(
            refusal.contains("audio_window")
                && refusal.contains("newest")
                && refusal.contains("ordered"),
            "the refusal must name both knobs; got {refusal}"
        );
    }

    #[test]
    fn a_contract_beside_an_ordering_profile_is_accepted() {
        refuse_audio_window_beside_a_skipping_delivery_profile(Some("ordered"))
            .expect("`ordered` is what a contract requires");
    }
}
