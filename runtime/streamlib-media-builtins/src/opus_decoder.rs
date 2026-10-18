// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in Opus decoder: Opus media frames in, audio blocks out.
//!
//! The state machine is [`EncodedPacketToAudioBlockDecoder`]. What lives
//! here is the port surface and the registration name.
//!
//! The input port declares no window contract: an encoded link carries
//! whole packets, and resampling or reframing a compressed bitstream is not
//! a thing the stage could do. It declares `ordered` because a decoder that
//! passed over packets would break its own stream.

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::Result;
use streamlib::sdk::processors::ReactiveProcessor;

use crate::encoded_packet_to_audio_block_decoder::{
    EncodedPacketToAudioBlockDecoder, OPUS_DECODER_PROCESSOR_NAME,
};

#[streamlib::sdk::processor(
    built_in_node,
    description = "Decodes Opus media frames to audio blocks via libopus.\n\n\
                  `encoded_audio` takes media frames in the layout `OpusEncoder` publishes and \
                  the moq-dev players read — a QUIC varint timestamp, then one Opus packet — \
                  and declares no window contract: an encoded link carries whole packets, and \
                  reframing a compressed bitstream is not a thing the stage could do. The \
                  channel count and the encoder's lookahead come from the `OpusHead` in the \
                  upstream port's catalog entry; with none there, the packet's TOC names one or \
                  two channels and nothing is trimmed. A message that is not a media frame, an \
                  `OpusHead` that does not read, and a `pre_skip` past the 5 760 samples one \
                  Opus packet can span are refused by name. `audio` publishes ordinary \
                  `AudioBlock` bags — `f32` at 48 000 Hz in the stream's own channel count.\n\n\
                  It enters the stream at any packet, since every Opus packet opens a group, \
                  and trims the encoder's `pre_skip` lookahead at entry — so the first block \
                  after entry is short by exactly that many samples. Stamps are derived from \
                  the entry packet's own stamp plus the samples emitted since, so the first \
                  emitted sample is the stamped instant rather than one lookahead later.\n\n\
                  A read reporting lost packets resets the decoder, re-enters at that packet, \
                  and logs how many it did not see. Nothing is invented to bridge the loss — \
                  no concealment, no FEC decode — so it stays derivable from the stamps either \
                  side.",
    execution = reactive,
    scheduling = high,
    input(
        "encoded_audio",
        delivery_profile = "ordered",
        description = "Opus media frames to decode"
    ),
    output(
        "audio",
        description = "Decoded audio blocks at 48 kHz in the stream's own channel count",
        catalog = BinaryEntry { mode: Snapshot, mime: "application/msgpack" }
    ),
)]
pub struct OpusDecoder {
    decode_body: EncodedPacketToAudioBlockDecoder,
}

impl ReactiveProcessor for OpusDecoder::Processor {
    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            blocks_published = self.decode_body.blocks_published(),
            packets_lost_on_the_link = self.decode_body.packets_lost_on_the_link(),
            "{OPUS_DECODER_PROCESSOR_NAME}: teardown"
        );
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("encoded_audio") {
            return Ok(());
        }
        while let Some(message) = self.inputs.read_raw("encoded_audio")? {
            let upstream_catalog_entry = self
                .inputs
                .inbound_link_catalog_entry("encoded_audio", None)?;
            if let Some(block) = self
                .decode_body
                .decode_one_arriving_message(&message, upstream_catalog_entry.as_ref())?
            {
                let block_timestamp_ns = block.first_sample_timestamp_ns;
                self.outputs
                    .write_msgpack_with_timestamp("audio", &block, block_timestamp_ns)?;
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
