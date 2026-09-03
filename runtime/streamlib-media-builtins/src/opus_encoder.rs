// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in Opus encoder: windowed audio blocks in, encoded-audio-packet
//! bags out.
//!
//! The state machine is [`AudioWindowToEncodedPacketEncoder`]. What lives
//! here is the port surface, the registration name and the window contract.
//!
//! The input port declares four of the contract's five values and leaves
//! `channels` unstated, so the engine resamples to 48 kHz, converts to
//! `f32` and frames into 20 ms windows while the channel count follows
//! whatever the source publishes. That is what lets one microphone and one
//! ambisonic rig reach the same encoder with no configuration between them.

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::Result;
use streamlib::sdk::processors::ReactiveProcessor;

use crate::audio_block::AudioBlock;
use crate::audio_window_to_encoded_packet_encoder::{
    AudioWindowToEncodedPacketEncoder, OPUS_ENCODER_PROCESSOR_NAME,
};

#[streamlib::sdk::processor(
    description = "Encodes 20 ms windows of audio to Opus encoded-audio-packet bags via libopus",
    execution = reactive,
    scheduling = high,
    config = crate::audio_window_to_encoded_packet_encoder::OpusEncoderConfig,
    input(
        "audio",
        delivery_profile = "ordered",
        audio_window(sample_rate = 48_000, dtype = "f32", window_size = 960, hop = 960),
        description = "Audio to encode, resampled and framed to 20 ms at Opus's own rate in the source's own channel count"
    ),
    output("encoded_audio", description = "Opus encoded-audio-packet bags"),
)]
pub struct OpusEncoder {
    encode_body: AudioWindowToEncodedPacketEncoder,
}

impl ReactiveProcessor for OpusEncoder::Processor {
    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            packets_encoded = self.encode_body.packets_encoded(),
            "{OPUS_ENCODER_PROCESSOR_NAME}: teardown"
        );
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("audio") {
            return Ok(());
        }
        // A windowed port reports data only when a full window can be
        // emitted, and the drain loop dispatches once per ready window, so
        // one read per dispatch is the whole of what is waiting.
        let window: AudioBlock = self.inputs.read("audio")?;
        let window_timestamp_ns = window.first_sample_timestamp_ns;

        let Some(packet) = self.encode_body.encode_one_window(&self.config, &window)? else {
            return Ok(());
        };
        // The timestamped write, never the implicit one: the packet names
        // the instant its first sample was captured, not the moment it was
        // published.
        self.outputs
            .write_with_timestamp("encoded_audio", &packet, window_timestamp_ns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib::sdk::descriptors::{AudioWindowContract, AudioWindowContractDeclaredValues};
    use streamlib::sdk::processors::GeneratedProcessor;

    #[test]
    fn the_input_declares_a_window_contract_that_follows_its_sources_channel_count() {
        let descriptor = <OpusEncoder::Processor as GeneratedProcessor>::descriptor()
            .expect("the macro emits a descriptor");
        let audio = descriptor
            .inputs
            .iter()
            .find(|port| port.name == "audio")
            .expect("the audio port is in the descriptor");

        assert_eq!(
            audio.audio_window,
            Some(AudioWindowContract::Declaration(
                AudioWindowContractDeclaredValues {
                    sample_rate: 48_000,
                    channels: None,
                    dtype: "f32".to_string(),
                    window_size: 960,
                    hop: 960,
                }
            )),
            "`channels` is left unstated so the count follows whatever the source publishes"
        );
        assert_eq!(audio.delivery_profile.as_deref(), Some("ordered"));
    }

    #[test]
    fn the_output_is_the_encoded_audio_port_and_takes_no_window_contract() {
        let descriptor = <OpusEncoder::Processor as GeneratedProcessor>::descriptor()
            .expect("the macro emits a descriptor");
        let encoded_audio = descriptor
            .outputs
            .iter()
            .find(|port| port.name == "encoded_audio")
            .expect("the encoded_audio port is in the descriptor");
        assert!(
            encoded_audio.audio_window.is_none(),
            "only a consumer states what it needs"
        );
        assert_eq!(descriptor.inputs.len(), 1);
        assert_eq!(descriptor.outputs.len(), 1);
    }
}
