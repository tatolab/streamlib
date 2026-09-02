// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The values the read-side stage is driven by, and how a declaration becomes
//! them.
//!
//! [`AudioWindowContract`] is what an author writes; this is what the stage
//! runs on. The two differ in exactly one way: the declaration may carry the
//! `match_device` sentinel, and the stage cannot — a sentinel is settled from
//! the format of a device stream the declaring processor opened, and a port
//! left holding one when its processor finished `setup()` is a wiring error,
//! not a default.

use streamlib_processor_schema::ProcessorClassImportPath;

use super::audio_block_bag_wire_codec::AudioBlockSampleDtype;
use super::device_matched_audio_window_contracts::AudioWindowContractMatchingADeviceStream;
use crate::core::context::AudioSampleFormat;
use crate::core::descriptors::{AudioWindowContract, AudioWindowContractDeclaredValues};
use crate::core::error::{Error, Result};
use crate::core::processors::PROCESSOR_REGISTRY;
use crate::iceoryx2::DeliveryProfile;

/// A window contract with every value settled — what the stage reads.
///
/// Built only through [`ResolvedAudioWindowContract::from_declared_values`],
/// which refuses a declaration the stage could not honour: there is no way to
/// hand the stage a hop above its window or a zero rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedAudioWindowContract {
    /// Rate every emitted window is at.
    pub(crate) sample_rate: u32,
    /// Channel count every emitted window is interleaved by, or `None` — the
    /// count the source's own blocks arrive in, whatever it is.
    ///
    /// Absent, the stage skips channel conversion entirely and a window
    /// carries what arrived. The count is then a property of the run rather
    /// than of the contract, which is why the stage reads it off each block
    /// and re-anchors when it changes.
    pub(crate) channels: Option<u32>,
    /// How an emitted window's scalars are written.
    pub(crate) dtype: AudioBlockSampleDtype,
    /// Per-channel samples in one emitted window.
    pub(crate) window_size: u32,
    /// Per-channel samples between the starts of consecutive windows.
    pub(crate) hop: u32,
}

/// The shortest source block the mailbox depth is sized to cover — an eighth of
/// a millisecond.
///
/// A wire-time depth cannot read the real block size: it arrives with the bags,
/// not with the declaration. What it must not do is assume a *sample count*,
/// because a count is not a duration — 128 samples is 2.7 ms at 48 kHz and
/// 0.67 ms at 192 kHz, and the mailbox has to span a window's worth of time
/// whichever rate the source runs at. So the assumption is stated as the
/// duration it actually is: 0.125 ms covers 24 samples at 192 kHz and 6 at
/// 48 kHz, below any device configuration in practice.
///
/// Sizing generously is close to free, which is why the bound sits well under
/// what a device really delivers. Depth costs queue slots, tens of bytes each,
/// held whether or not a bag ever arrives; the audio itself is bounded by the
/// window's duration rather than by the slot count, because a source with
/// shorter blocks fills more slots with proportionally smaller payloads. Being
/// too mean costs incomparably more: a port that fills its mailbox without ever
/// filling a window delivers nothing at all and evicts everything behind it.
const SHORTEST_SOURCE_BLOCK_THE_DEPTH_COVERS_NANOSECONDS: u64 = 125_000;

/// The most slots one windowed port may hold, whatever it declares.
///
/// At [`SHORTEST_SOURCE_BLOCK_THE_DEPTH_COVERS_NANOSECONDS`] this spans a
/// little over a second, so every window up to that length is covered even from
/// the shortest blocks, and a longer one is covered from the block sizes a
/// device really delivers. Past that the stage says so rather than stalling
/// silently — see the warning it raises when a full mailbox still cannot make
/// one window. The cap exists so a single declaration cannot ask the engine for
/// unbounded memory.
const MOST_SLOTS_ONE_WINDOWED_PORT_HOLDS: usize = 8_192;

/// Extra queued blocks beyond the window's own worth, so a burst that arrives
/// while the reader is mid-window does not evict the samples the window still
/// needs.
const WINDOWED_PORT_MAILBOX_DEPTH_MARGIN: usize = 4;

impl ResolvedAudioWindowContract {
    /// Read the values a declaration states, refusing one the stage could not
    /// honour.
    ///
    /// The refusal is returned bare so each caller frames it in its own terms:
    /// the engine names the processor and the port, a helper child names the
    /// port the parent wired it for.
    pub fn from_declared_values(
        values: &AudioWindowContractDeclaredValues,
    ) -> std::result::Result<Self, String> {
        values.refuse_if_unhonourable()?;

        // `refuse_if_unhonourable` already rejects every dtype outside the
        // declaration vocabulary, so this cannot fail — but the stage reads the
        // parsed value rather than the string, and an unwrap here would be a
        // panic in the wiring path if that ever stopped being true.
        let dtype = AudioBlockSampleDtype::from_wire_str(&values.dtype).ok_or_else(|| {
            format!(
                "`audio_window` dtype `\"{}\"` names no encoding the stage can read",
                values.dtype
            )
        })?;

        Ok(Self {
            sample_rate: values.sample_rate,
            channels: values.channels,
            dtype,
            window_size: values.window_size,
            hop: values.hop,
        })
    }

    /// Read the format a processor's own device stream opened at as the
    /// contract its port declaring `match_device` asked for.
    ///
    /// The refusal is returned bare for the same reason
    /// [`Self::from_declared_values`]'s is: only the caller knows which
    /// processor and which port to name.
    pub(crate) fn from_a_device_stream_format(
        matching: &AudioWindowContractMatchingADeviceStream,
    ) -> std::result::Result<Self, String> {
        let format = matching.device_stream_format;
        Self::from_declared_values(&AudioWindowContractDeclaredValues {
            sample_rate: format.sample_rate,
            // A device stream resolves a count, so a settled contract always
            // states one: `match_device` is unchanged by the source-following
            // default.
            channels: Some(format.channels),
            dtype: audio_window_dtype_of(format.sample_format).to_string(),
            window_size: matching.window_size_in_per_channel_samples,
            hop: matching.hop_in_per_channel_samples,
        })
    }

    /// Read a declaration the author wrote, refusing one the stage could not
    /// honour. A sentinel is not a declaration the stage can run on, so it
    /// comes back as [`AudioWindowDeclarationOfAnInputPort::MatchesItsProcessorsDeviceStream`]
    /// rather than as values.
    pub(crate) fn read_declaration(
        contract: &AudioWindowContract,
        processor_type: &ProcessorClassImportPath,
        port_name: &str,
    ) -> Result<AudioWindowDeclarationOfAnInputPort> {
        match contract {
            AudioWindowContract::Declaration(values) => Self::from_declared_values(values)
                .map(AudioWindowDeclarationOfAnInputPort::StatedOutright)
                .map_err(|refusal| {
                    Error::Configuration(format!(
                        "input port '{port_name}' on '{processor_type}' declared {refusal}"
                    ))
                }),
            AudioWindowContract::MatchDevice {} => {
                Ok(AudioWindowDeclarationOfAnInputPort::MatchesItsProcessorsDeviceStream)
            }
            // Rendering-only, and this reads declarations: `graph` uses it to
            // say five values came from a device rather than from an author.
            // A registered port carrying one would mean a descriptor built from
            // a rendering, which is a wiring error and not a contract the stage
            // may quietly run on.
            AudioWindowContract::Device(_) => Err(Error::Configuration(format!(
                "input port '{port_name}' on '{processor_type}' declares an `audio_window` \
                 resolved from a device, which is how `graph` renders a settled contract \
                 and not something a port may declare. State the values, or \
                 `match_device` to take them from the device this processor opens"
            ))),
        }
    }

    /// The values as a declaration states them, for the parent→child wiring
    /// envelope.
    pub(crate) fn as_declared_values(&self) -> AudioWindowContractDeclaredValues {
        AudioWindowContractDeclaredValues {
            sample_rate: self.sample_rate,
            channels: self.channels,
            dtype: self.dtype.as_wire_str().to_string(),
            window_size: self.window_size,
            hop: self.hop,
        }
    }

    /// The mailbox depth a port declaring this contract is sized to.
    ///
    /// A window is `window_size / sample_rate` seconds of audio, and the port
    /// must be able to hold that much queued before the accumulator can emit
    /// once. `ORDERED_DEPTH` is 16, which a one-second rolling window's ~47
    /// quanta overruns immediately, so the contract sizes the mailbox and the
    /// profile's depth becomes the floor rather than the answer. Still
    /// engine-chosen, still not authorable: the contract is a declaration, not
    /// a depth dial.
    pub(crate) fn windowed_port_mailbox_depth(&self) -> usize {
        let window_nanoseconds =
            u64::from(self.window_size) * 1_000_000_000 / u64::from(self.sample_rate);
        let blocks_per_window =
            window_nanoseconds.div_ceil(SHORTEST_SOURCE_BLOCK_THE_DEPTH_COVERS_NANOSECONDS);
        let derived = usize::try_from(blocks_per_window)
            .unwrap_or(MOST_SLOTS_ONE_WINDOWED_PORT_HOLDS)
            .saturating_add(WINDOWED_PORT_MAILBOX_DEPTH_MARGIN)
            .min(MOST_SLOTS_ONE_WINDOWED_PORT_HOLDS);
        derived.max(DeliveryProfile::ORDERED_DEPTH)
    }
}

/// How a wire dtype is spelled for a device stream's scalar encoding.
///
/// The two vocabularies are the same two encodings under different names, and
/// this is the one place they meet — a declaration's `dtype` string is the
/// contract, and a device format is what a `match_device` port resolves from.
fn audio_window_dtype_of(sample_format: AudioSampleFormat) -> &'static str {
    match sample_format {
        AudioSampleFormat::F32 => AudioBlockSampleDtype::F32.as_wire_str(),
        AudioSampleFormat::I16 => AudioBlockSampleDtype::I16.as_wire_str(),
    }
}

/// What one input port's author wrote, as the wiring path reads it.
///
/// The declaration, never the runtime state — its sibling
/// [`InstalledInputPortAudioWindowing`] is what a wired port actually does, and
/// the two are deliberately spelled apart. This one has no "not windowed" arm
/// at all: a port with no contract never produces one of these.
///
/// The sentinel is the reason this is not just `Option<ResolvedAudioWindowContract>`:
/// a port declaring it windows, but its values are not knowable until the
/// declaring processor opens its device in `setup()` — which the compiler runs
/// *after* it has wired every link. So the wiring path installs a port that
/// windows nothing yet and hands a reader nothing, and `setup()` completes it.
///
/// [`InstalledInputPortAudioWindowing`]: crate::iceoryx2::InputMailboxesInner
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioWindowDeclarationOfAnInputPort {
    /// The author wrote the values, and these are they.
    StatedOutright(ResolvedAudioWindowContract),
    /// The author wrote `audio_window = match_device`, so the five values come
    /// from the device stream the declaring processor opens.
    MatchesItsProcessorsDeviceStream,
}

/// Refuse a port still holding an unsettled `match_device` sentinel, naming
/// where it would have been settled.
///
/// One spelling for both places that can raise it: a helper-placed destination,
/// which can never settle one (its wiring envelope carries resolved values
/// only), refused at wire time; and an app-process processor whose `setup()`
/// returned without settling it, refused there.
pub(crate) fn refuse_an_unsettled_match_device_sentinel(
    processor_type: &ProcessorClassImportPath,
    port_name: &str,
) -> Error {
    Error::Configuration(format!(
        "input port '{port_name}' on '{processor_type}' declares \
         `audio_window = match_device`, which resolves at `setup()` from the format \
         of the device stream the declaring processor opened — and nothing has \
         resolved it. Only a processor that opens a device stream can satisfy the \
         sentinel; declare the values outright, or give this port a processor \
         that opens one"
    ))
}

/// Read the window declaration a destination input port carries, if it carries
/// one.
///
/// The single window-contract declaration primitive, beside
/// [`delivery_profile_for_input_port`]. `Ok(None)` is a port with no contract,
/// which is unchanged in every respect.
///
/// Falls back to `None` when the destination processor type isn't registered
/// or the named port doesn't exist — defensive, and the same fallback the
/// delivery-profile resolution takes: a Wired link always resolves both, and
/// the wiring path itself reports a missing processor.
///
/// [`delivery_profile_for_input_port`]: crate::iceoryx2::delivery_profile_for_input_port
pub(crate) fn audio_windowing_declared_by_input_port(
    processor_type: &ProcessorClassImportPath,
    port_name: &str,
) -> Result<Option<AudioWindowDeclarationOfAnInputPort>> {
    let Some((inputs, _outputs)) = PROCESSOR_REGISTRY.port_info(processor_type) else {
        return Ok(None);
    };
    let Some(port) = inputs.iter().find(|port| port.name == port_name) else {
        return Ok(None);
    };
    let Some(contract) = port.audio_window.as_ref() else {
        return Ok(None);
    };

    ResolvedAudioWindowContract::read_declaration(contract, processor_type, port_name).map(Some)
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

    fn a_processor_type() -> ProcessorClassImportPath {
        ProcessorClassImportPath::new("tests.WindowedConsumer").expect("a legal import path")
    }

    fn resolve(contract: &AudioWindowContract) -> Result<ResolvedAudioWindowContract> {
        match ResolvedAudioWindowContract::read_declaration(contract, &a_processor_type(), "audio")?
        {
            AudioWindowDeclarationOfAnInputPort::StatedOutright(resolved) => Ok(resolved),
            AudioWindowDeclarationOfAnInputPort::MatchesItsProcessorsDeviceStream => {
                panic!("this declaration states its five values")
            }
        }
    }

    #[test]
    fn a_declared_contract_resolves_to_the_five_values_it_declared() {
        let resolved = resolve(&AudioWindowContract::Declaration(declared_values()))
            .expect("a declared contract resolves");

        assert_eq!(resolved.sample_rate, 16_000);
        assert_eq!(resolved.channels, Some(1));
        assert_eq!(resolved.dtype, AudioBlockSampleDtype::F32);
        assert_eq!(resolved.window_size, 512);
        assert_eq!(resolved.hop, 512);
    }

    /// The sentinel states no values, so reading the declaration cannot produce
    /// any: it says the port is waiting on the device stream its processor has
    /// not opened yet. The refusal belongs where it can know nothing ever will.
    #[test]
    fn the_sentinel_reads_as_a_port_awaiting_its_device_rather_than_as_five_values() {
        let windowing = ResolvedAudioWindowContract::read_declaration(
            &AudioWindowContract::MatchDevice {},
            &a_processor_type(),
            "audio",
        )
        .expect("reading a sentinel declaration is not itself an error");

        assert_eq!(
            windowing,
            AudioWindowDeclarationOfAnInputPort::MatchesItsProcessorsDeviceStream
        );
    }

    #[test]
    fn an_unsettled_sentinel_is_refused_naming_the_resolution_mechanism() {
        let rendered =
            refuse_an_unsettled_match_device_sentinel(&a_processor_type(), "audio").to_string();

        assert!(
            rendered.contains("match_device")
                && rendered.contains("setup()")
                && rendered.contains("audio"),
            "the refusal must name the sentinel, where it resolves, and the port; \
             got {rendered}"
        );
    }

    /// `graph` renders a settled contract as having come from a device. Read
    /// back as a declaration it is a wiring error, not a contract the stage may
    /// quietly run on — a descriptor built from a rendering.
    #[test]
    fn a_contract_rendered_as_a_devices_is_refused_when_read_as_a_declaration() {
        let refusal = ResolvedAudioWindowContract::read_declaration(
            &AudioWindowContract::Device(declared_values()),
            &a_processor_type(),
            "audio",
        )
        .expect_err("a rendering is not a declaration")
        .to_string();

        assert!(
            refusal.contains("audio") && refusal.contains("match_device"),
            "the refusal must name the port and offer the spelling that works; got {refusal}"
        );
    }

    #[test]
    fn a_device_stream_format_resolves_to_the_contract_that_plays_on_it() {
        let resolved = ResolvedAudioWindowContract::from_a_device_stream_format(
            &AudioWindowContractMatchingADeviceStream {
                device_stream_format: crate::core::context::AudioStreamFormat {
                    sample_rate: 48_000,
                    channels: 2,
                    sample_format: AudioSampleFormat::I16,
                },
                window_size_in_per_channel_samples: 512,
                hop_in_per_channel_samples: 512,
            },
        )
        .expect("a device format resolves");

        assert_eq!(resolved.sample_rate, 48_000);
        assert_eq!(resolved.channels, Some(2));
        assert_eq!(resolved.dtype, AudioBlockSampleDtype::I16);
        assert_eq!(resolved.window_size, 512);
        assert_eq!(resolved.hop, 512);
    }

    /// A device stream reaches the same validator a written declaration does:
    /// the stage has one set of values it can honour, whoever stated them.
    #[test]
    fn a_device_stream_format_the_stage_could_not_honour_is_refused_too() {
        let refusal = ResolvedAudioWindowContract::from_a_device_stream_format(
            &AudioWindowContractMatchingADeviceStream {
                device_stream_format: crate::core::context::AudioStreamFormat {
                    sample_rate: 48_000,
                    channels: 2,
                    sample_format: AudioSampleFormat::F32,
                },
                window_size_in_per_channel_samples: 0,
                hop_in_per_channel_samples: 0,
            },
        )
        .expect_err("a zero window is refused");

        assert!(
            refusal.contains("window_size") && refusal.contains(" is 0 "),
            "the refusal must name the field and the value; got {refusal}"
        );
    }

    #[test]
    fn a_declaration_the_stage_could_not_honour_is_refused_naming_the_port() {
        let refusal = resolve(&AudioWindowContract::Declaration(
            AudioWindowContractDeclaredValues {
                hop: 1_024,
                ..declared_values()
            },
        ))
        .expect_err("a hop above the window is refused");
        let rendered = refusal.to_string();
        assert!(
            rendered.contains("audio") && rendered.contains("1024") && rendered.contains("512"),
            "the refusal must name the port and both numbers; got {rendered}"
        );
    }

    /// The delivery profile's depth is a floor the contract may raise and never
    /// undercuts, so a windowed port is never shallower than an unwindowed one.
    #[test]
    fn the_profiles_depth_is_a_floor_no_contract_undercuts() {
        for window_size in [1u32, 16, 64, 512] {
            let depth = resolve(&AudioWindowContract::Declaration(
                AudioWindowContractDeclaredValues {
                    window_size,
                    hop: window_size,
                    ..declared_values()
                },
            ))
            .expect("resolves")
            .windowed_port_mailbox_depth();

            assert!(
                depth >= DeliveryProfile::ORDERED_DEPTH,
                "a {window_size}-sample window sized its port to {depth}, below the profile's \
                 own {}",
                DeliveryProfile::ORDERED_DEPTH
            );
        }
    }

    /// The invariant the depth exists for, asserted as itself rather than as a
    /// constant: whatever the window, the mailbox must hold one window's worth
    /// of source blocks at the quantum the engine assumes.
    ///
    /// A depth that cannot is a port which fills up without ever filling a
    /// window — it would deliver nothing at all and evict everything behind it.
    #[test]
    fn every_windows_depth_holds_a_windows_worth_of_the_assumed_quantum() {
        for (sample_rate, window_size) in [
            (16_000u32, 512u32),
            (16_000, 16_000),
            (48_000, 1_024),
            (8_000, 8_000),
        ] {
            let contract = resolve(&AudioWindowContract::Declaration(
                AudioWindowContractDeclaredValues {
                    sample_rate,
                    window_size,
                    hop: window_size,
                    ..declared_values()
                },
            ))
            .expect("resolves");

            let window_nanoseconds =
                u64::from(window_size) * 1_000_000_000 / u64::from(sample_rate);
            let depth_nanoseconds = contract.windowed_port_mailbox_depth() as u64
                * SHORTEST_SOURCE_BLOCK_THE_DEPTH_COVERS_NANOSECONDS;
            assert!(
                depth_nanoseconds >= window_nanoseconds,
                "a {window_size}-sample window at {sample_rate} Hz spans {window_nanoseconds} ns \
                 but its port holds only {depth_nanoseconds} ns of assumed quanta"
            );
        }
    }

    /// The case the change file names: a one-second rolling window needs far
    /// more than the profile's sixteen blocks.
    #[test]
    fn a_one_second_window_is_sized_past_the_profiles_depth_by_its_own_quanta() {
        let one_second_rolling =
            AudioWindowContract::Declaration(AudioWindowContractDeclaredValues {
                sample_rate: 16_000,
                window_size: 16_000,
                hop: 160,
                ..declared_values()
            });

        let depth = resolve(&one_second_rolling)
            .expect("resolves")
            .windowed_port_mailbox_depth();

        assert!(
            depth > DeliveryProfile::ORDERED_DEPTH * 4,
            "a one-second window must outgrow the profile's depth by a wide margin; got {depth}"
        );
    }
}
