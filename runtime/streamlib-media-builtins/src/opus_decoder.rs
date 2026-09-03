// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in Opus decoder: encoded-audio-packet bags in, audio blocks out.
//!
//! The state machine is [`EncodedPacketToAudioBlockDecoder`]. What lives
//! here is the port surface and the registration name.
//!
//! The input port declares no window contract: an encoded link carries
//! whole packets, and resampling or reframing a compressed bitstream is not
//! a thing the stage could do. It declares `ordered` because a decoder that
//! passed over packets would break its own stream.

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ReactiveProcessor;

use crate::encoded_audio_packet::read_encoded_audio_packet_bag;
use crate::encoded_packet_to_audio_block_decoder::{
    EncodedPacketToAudioBlockDecoder, OPUS_DECODER_PROCESSOR_NAME,
};

#[streamlib::sdk::processor(
    description = "Decodes Opus encoded-audio-packet bags to audio blocks via libopus",
    execution = reactive,
    scheduling = high,
    input(
        "encoded_audio",
        delivery_profile = "ordered",
        description = "Opus encoded-audio-packet bags to decode"
    ),
    output("audio", description = "Decoded audio blocks at 48 kHz in the packet's own channel count"),
)]
pub struct OpusDecoder {
    decode_body: EncodedPacketToAudioBlockDecoder,
}

impl ReactiveProcessor for OpusDecoder::Processor {
    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            blocks_published = self.decode_body.blocks_published(),
            packets_lost_to_gaps = self.decode_body.packets_lost_to_gaps(),
            "{OPUS_DECODER_PROCESSOR_NAME}: teardown"
        );
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("encoded_audio") {
            return Ok(());
        }
        while let Some((bag_bytes, frame_header_timestamp_ns)) =
            self.inputs.read_raw("encoded_audio")?
        {
            // A bag this reader cannot read is refused by name at the read,
            // never reshaped into a plausible-looking wrong answer.
            let packet = read_encoded_audio_packet_bag(&bag_bytes).map_err(|refusal| {
                Error::Runtime(format!("{OPUS_DECODER_PROCESSOR_NAME}: {refusal}"))
            })?;

            if let Some(block) = self
                .decode_body
                .decode_one_arriving_packet(&packet, frame_header_timestamp_ns)?
            {
                let block_timestamp_ns = block.first_sample_timestamp_ns;
                self.outputs
                    .write_with_timestamp("audio", &block, block_timestamp_ns)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib::sdk::processors::GeneratedProcessor;

    #[test]
    fn the_encoded_input_is_ordered_and_declares_no_window_contract() {
        let descriptor = <OpusDecoder::Processor as GeneratedProcessor>::descriptor()
            .expect("the macro emits a descriptor");
        let encoded_audio = descriptor
            .inputs
            .iter()
            .find(|port| port.name == "encoded_audio")
            .expect("the encoded_audio port is in the descriptor");

        assert_eq!(encoded_audio.delivery_profile.as_deref(), Some("ordered"));
        assert!(
            encoded_audio.audio_window.is_none(),
            "an encoded link carries whole packets — there is nothing for the stage to reframe"
        );
        assert!(
            descriptor.outputs.iter().any(|port| port.name == "audio"),
            "the decoder publishes audio blocks"
        );
        assert_eq!(descriptor.inputs.len(), 1);
        assert_eq!(descriptor.outputs.len(), 1);
    }
}
