// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The five values the read-side stage is driven by, and how a declaration
//! becomes them.
//!
//! [`AudioWindowContract`] is what an author writes; this is what the stage
//! runs on. The two differ in exactly one way: the declaration may carry the
//! `match_device` sentinel, and the stage cannot — a sentinel that has not
//! been resolved from a device stream is a wiring error, not a default.

use streamlib_processor_schema::ProcessorClassImportPath;

use super::audio_block_bag_wire_codec::AudioBlockSampleDtype;
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
    /// Channel count every emitted window is interleaved by.
    pub(crate) channels: u32,
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
    /// Read the five values a declaration states, refusing one the stage
    /// could not honour.
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

    /// Read a declaration the author wrote, refusing one the stage could not
    /// honour and one whose sentinel nothing has resolved.
    pub(crate) fn resolve(
        contract: &AudioWindowContract,
        processor_type: &ProcessorClassImportPath,
        port_name: &str,
    ) -> Result<Self> {
        match contract {
            AudioWindowContract::Declaration(values) => {
                Self::from_declared_values(values).map_err(|refusal| {
                    Error::Configuration(format!(
                        "input port '{port_name}' on '{processor_type}' declared {refusal}"
                    ))
                })
            }
            AudioWindowContract::MatchDevice {} => Err(Error::Configuration(format!(
                "input port '{port_name}' on '{processor_type}' declares \
                 `audio_window = match_device`, which resolves at `setup()` from the format \
                 of the device stream the declaring processor opened — and nothing has \
                 resolved it. Only a processor that opens a device stream can satisfy the \
                 sentinel; declare the five values outright, or give this port a processor \
                 that opens one"
            ))),
        }
    }

    /// The five values as a declaration states them, for the parent→child
    /// wiring envelope.
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

    /// Scalars one emitted window carries: `window_size × channels`.
    pub(crate) fn scalars_per_window(&self) -> usize {
        self.window_size as usize * self.channels as usize
    }
}

/// Resolve the window contract a destination input port declares, if it
/// declares one.
///
/// The single window-contract resolution primitive, beside
/// [`delivery_profile_for_input_port`]. `Ok(None)` is a port with no contract,
/// which is unchanged in every respect.
///
/// Falls back to `None` when the destination processor type isn't registered
/// or the named port doesn't exist — defensive, and the same fallback the
/// delivery-profile resolution takes: a Wired link always resolves both, and
/// the wiring path itself reports a missing processor.
///
/// [`delivery_profile_for_input_port`]: crate::iceoryx2::delivery_profile_for_input_port
pub(crate) fn audio_window_contract_for_input_port(
    processor_type: &ProcessorClassImportPath,
    port_name: &str,
) -> Result<Option<ResolvedAudioWindowContract>> {
    let Some((inputs, _outputs)) = PROCESSOR_REGISTRY.port_info(processor_type) else {
        return Ok(None);
    };
    let Some(port) = inputs.iter().find(|port| port.name == port_name) else {
        return Ok(None);
    };
    let Some(contract) = port.audio_window.as_ref() else {
        return Ok(None);
    };

    ResolvedAudioWindowContract::resolve(contract, processor_type, port_name).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared_values() -> AudioWindowContractDeclaredValues {
        AudioWindowContractDeclaredValues {
            sample_rate: 16_000,
            channels: 1,
            dtype: "f32".to_string(),
            window_size: 512,
            hop: 512,
        }
    }

    fn a_processor_type() -> ProcessorClassImportPath {
        ProcessorClassImportPath::new("tests.WindowedConsumer").expect("a legal import path")
    }

    fn resolve(contract: &AudioWindowContract) -> Result<ResolvedAudioWindowContract> {
        ResolvedAudioWindowContract::resolve(contract, &a_processor_type(), "audio")
    }

    #[test]
    fn a_declared_contract_resolves_to_the_five_values_it_declared() {
        let resolved = resolve(&AudioWindowContract::Declaration(declared_values()))
            .expect("a declared contract resolves");

        assert_eq!(resolved.sample_rate, 16_000);
        assert_eq!(resolved.channels, 1);
        assert_eq!(resolved.dtype, AudioBlockSampleDtype::F32);
        assert_eq!(resolved.window_size, 512);
        assert_eq!(resolved.hop, 512);
    }

    #[test]
    fn an_unresolved_sentinel_is_a_wiring_error_naming_the_resolution_mechanism() {
        let refusal = resolve(&AudioWindowContract::MatchDevice {})
            .expect_err("nothing has resolved the sentinel");
        let rendered = refusal.to_string();
        assert!(
            rendered.contains("match_device")
                && rendered.contains("setup()")
                && rendered.contains("audio"),
            "the refusal must name the sentinel, where it resolves, and the port; \
             got {rendered}"
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
