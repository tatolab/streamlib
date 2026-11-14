// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The native half of `tatolab-opus` — the module the wheel's two `@node.process`
//! classes import as `tatolab.opus._native`.
//!
//! The engine never calls anything here: libopus runs in each node's own
//! processor interpreter, reached directly from the node's Python.

use pyo3::buffer::PyBuffer;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

pub mod audio_window;
pub mod audio_window_to_encoded_packet_encoder;
pub mod encoded_packet_to_audio_block_decoder;
pub mod message_group_loss_recovery_gate;
pub mod opus_codec_refusal;
pub mod opus_identification_header;
pub mod opus_stream_layout;

use crate::audio_window::{AudioSampleDtype, AudioWindowToEncode, DecodedAudioBlock};
use crate::audio_window_to_encoded_packet_encoder::{
    AudioWindowToEncodedPacketEncoder, OpusEncoderApplication, OpusEncoderConfig,
    OpusEncoderPortStreamFacts,
};
use crate::encoded_packet_to_audio_block_decoder::{
    ArrivingEncodedAudioMessage, EncodedPacketToAudioBlockDecoder, OPUS_DECODER_PROCESSOR_NAME,
    UpstreamPacketsStreamFacts,
};
use crate::opus_codec_refusal::OpusCodecNotice;

/// What a node logs, as `(level, message)` with `level` naming the
/// `tatolab.stream.log` function to log it through.
type OpusCodecNoticeForPython = (&'static str, String);

fn notices_for_python(notices: Vec<OpusCodecNotice>) -> Vec<OpusCodecNoticeForPython> {
    notices
        .into_iter()
        .map(|notice| (notice.level.as_log_function_name(), notice.message))
        .collect()
}

/// Encodes 20 ms windows of audio into Opus packets.
///
/// One thread drives a node's hooks, so the instance is never shared.
#[pyclass(name = "AudioWindowToEncodedPacketEncoder", unsendable)]
struct AudioWindowToEncodedPacketEncoderForPython {
    encoder: AudioWindowToEncodedPacketEncoder,
}

#[pymethods]
impl AudioWindowToEncodedPacketEncoderForPython {
    #[new]
    #[pyo3(signature = (bitrate_bps, application))]
    fn new(bitrate_bps: Option<u32>, application: &str) -> PyResult<Self> {
        Ok(Self {
            encoder: AudioWindowToEncodedPacketEncoder::new(OpusEncoderConfig {
                bitrate_bps,
                application: OpusEncoderApplication::from_config_spelling(application)?,
            }),
        })
    }

    /// One Opus packet, or `None` while the encoder discards a source it already refused.
    #[pyo3(signature = (
        interleaved_sample_bytes,
        sample_rate,
        channels,
        sample_count,
        dtype,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn encode_one_window<'py>(
        &mut self,
        python: Python<'py>,
        interleaved_sample_bytes: PyBuffer<u8>,
        sample_rate: u32,
        channels: u32,
        sample_count: u32,
        dtype: &str,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let window = AudioWindowToEncode {
            interleaved_sample_bytes: bytes_of_a_contiguous_buffer(
                &interleaved_sample_bytes,
                "interleaved_sample_bytes",
            )?,
            sample_rate,
            channels,
            sample_count,
            dtype: AudioSampleDtype::from_wire_spelling(dtype)?,
        };
        let opus_packet_bytes = self.encoder.encode_one_window(&window)?;
        Ok(opus_packet_bytes.map(|opus_packet_bytes| PyBytes::new(python, &opus_packet_bytes)))
    }

    /// The stream facts the last mint owes the output port, once.
    fn take_stream_facts_owed_to_the_port(
        &mut self,
    ) -> Option<OpusEncoderPortStreamFactsForPython> {
        self.encoder
            .take_stream_facts_owed_to_the_port()
            .map(|stream_facts| OpusEncoderPortStreamFactsForPython { stream_facts })
    }

    /// What the encoder did since the last call, for the node to log.
    fn take_notices(&mut self) -> Vec<OpusCodecNoticeForPython> {
        notices_for_python(self.encoder.take_notices())
    }

    #[getter]
    fn packets_encoded(&self) -> u64 {
        self.encoder.packets_encoded()
    }
}

/// The Packets stream facts an encoder's output port carries for the instance
/// minted.
#[pyclass(name = "OpusEncoderPortStreamFacts", frozen)]
struct OpusEncoderPortStreamFactsForPython {
    stream_facts: OpusEncoderPortStreamFacts,
}

#[pymethods]
impl OpusEncoderPortStreamFactsForPython {
    #[getter]
    fn channel_count(&self) -> u32 {
        self.stream_facts.channel_count
    }

    #[getter]
    fn sample_rate(&self) -> u32 {
        self.stream_facts.sample_rate
    }

    /// The `OpusHead`, hex-encoded.
    #[getter]
    fn codec_configuration_hex(&self) -> &str {
        &self.stream_facts.codec_configuration_hex
    }
}

/// Decodes Opus packets into audio blocks.
///
/// One thread drives a node's hooks, so the instance is never shared.
#[pyclass(name = "EncodedPacketToAudioBlockDecoder", unsendable)]
struct EncodedPacketToAudioBlockDecoderForPython {
    decoder: EncodedPacketToAudioBlockDecoder,
}

#[pymethods]
impl EncodedPacketToAudioBlockDecoderForPython {
    #[new]
    fn new() -> Self {
        Self {
            decoder: EncodedPacketToAudioBlockDecoder::default(),
        }
    }

    /// One arriving message decoded in the format the upstream port's stream
    /// facts name, in their wire form, or `None` when it is discarded or was
    /// all encoder priming.
    #[pyo3(signature = (
        payload_bytes,
        timestamp_ns,
        starts_group,
        lost_since_last_read,
        upstream_packets_stream_facts,
    ))]
    fn decode_one_arriving_message(
        &mut self,
        payload_bytes: PyBuffer<u8>,
        timestamp_ns: i64,
        starts_group: bool,
        lost_since_last_read: u64,
        upstream_packets_stream_facts: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Option<DecodedAudioBlockForPython>> {
        let upstream_packets_stream_facts = upstream_packets_stream_facts
            .map(upstream_packets_stream_facts_from_python)
            .transpose()?;
        let decoded = self.decoder.decode_one_arriving_message(
            &ArrivingEncodedAudioMessage {
                payload_bytes: bytes_of_a_contiguous_buffer(&payload_bytes, "payload_bytes")?,
                timestamp_ns,
                starts_group,
                lost_since_last_read,
            },
            upstream_packets_stream_facts.as_ref(),
        )?;
        Ok(decoded.map(|block| DecodedAudioBlockForPython { block }))
    }

    /// What the decoder did since the last call, for the node to log.
    fn take_notices(&mut self) -> Vec<OpusCodecNoticeForPython> {
        notices_for_python(self.decoder.take_notices())
    }

    #[getter]
    fn blocks_published(&self) -> u64 {
        self.decoder.blocks_published()
    }

    #[getter]
    fn packets_lost_on_the_link(&self) -> u64 {
        self.decoder.packets_lost_on_the_link()
    }
}

/// The Packets stream facts the decoder reads, from their wire form as
/// `PacketsStreamFacts.to_wire()` writes it, refusing by name a field of the
/// wrong type.
fn upstream_packets_stream_facts_from_python(
    stream_facts_wire: &Bound<'_, PyDict>,
) -> PyResult<UpstreamPacketsStreamFacts> {
    let refused = |what: &str| {
        PyValueError::new_err(format!(
            "{OPUS_DECODER_PROCESSOR_NAME}: the upstream port's stream facts {what}"
        ))
    };
    let optional_field = |name: &str| -> PyResult<Option<Bound<'_, PyAny>>> {
        Ok(stream_facts_wire
            .get_item(name)?
            .filter(|value| !value.is_none()))
    };
    Ok(UpstreamPacketsStreamFacts {
        rfc6381_codec: optional_field("rfc6381_codec")?
            .map(|codec| codec.extract())
            .transpose()
            .map_err(|_| refused("name an `rfc6381_codec` that is not a string"))?,
        channel_count: optional_field("channel_count")?
            .map(|channels| channels.extract())
            .transpose()
            .map_err(|_| refused("name a `channel_count` that is not a count"))?,
        codec_configuration_hex: optional_field("codec_configuration_hex")?
            .map(|configuration| configuration.extract())
            .transpose()
            .map_err(|_| refused("carry a `codec_configuration_hex` that is not a string"))?,
    })
}

/// One decoded block: interleaved little-endian `f32` at 48 kHz.
#[pyclass(name = "DecodedAudioBlock", frozen)]
struct DecodedAudioBlockForPython {
    block: DecodedAudioBlock,
}

#[pymethods]
impl DecodedAudioBlockForPython {
    #[getter]
    fn interleaved_sample_bytes<'py>(&self, python: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(python, &self.block.interleaved_sample_bytes)
    }

    #[getter]
    fn sample_rate(&self) -> u32 {
        self.block.sample_rate
    }

    #[getter]
    fn channels(&self) -> u32 {
        self.block.channels
    }

    #[getter]
    fn sample_count(&self) -> u32 {
        self.block.sample_count
    }

    #[getter]
    fn first_sample_timestamp_ns(&self) -> i64 {
        self.block.first_sample_timestamp_ns
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<AudioWindowToEncodedPacketEncoderForPython>()?;
    module.add_class::<OpusEncoderPortStreamFactsForPython>()?;
    module.add_class::<EncodedPacketToAudioBlockDecoderForPython>()?;
    module.add_class::<DecodedAudioBlockForPython>()?;
    Ok(())
}

/// The bytes of a C-contiguous buffer — `bytes`, a read's `PortPayloadBorrow`,
/// a `memoryview` — borrowed while `buffer` lives.
fn bytes_of_a_contiguous_buffer<'buffer>(
    buffer: &'buffer PyBuffer<u8>,
    argument_name: &str,
) -> PyResult<&'buffer [u8]> {
    if !buffer.is_c_contiguous() {
        return Err(PyValueError::new_err(format!(
            "{argument_name} must be a C-contiguous buffer"
        )));
    }
    // SAFETY: a C-contiguous buffer spans `len_bytes` bytes from `buf_ptr`,
    // and its exporter keeps them alive and unmoved while `buffer` lives.
    Ok(unsafe { std::slice::from_raw_parts(buffer.buf_ptr() as *const u8, buffer.len_bytes()) })
}
