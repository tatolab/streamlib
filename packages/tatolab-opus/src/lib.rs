// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of `tatolab-opus` — the module the wheel's two `@node`
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
pub mod hang_legacy_container_media_frame;
pub mod message_group_loss_recovery_gate;
pub mod opus_codec_refusal;
pub mod opus_identification_header;
pub mod opus_stream_layout;

use crate::audio_window::{AudioSampleDtype, AudioWindowToEncode, DecodedAudioBlock};
use crate::audio_window_to_encoded_packet_encoder::{
    AudioWindowToEncodedPacketEncoder, OpusEncoderApplication, OpusEncoderConfig,
    OpusEncoderPortAudioEntry,
};
use crate::encoded_packet_to_audio_block_decoder::{
    ArrivingEncodedAudioMessage, EncodedPacketToAudioBlockDecoder, OPUS_DECODER_PROCESSOR_NAME,
    UpstreamCatalogEntry,
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

/// Encodes 20 ms windows of audio into Opus media frames.
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

    /// One media frame stamped at the window's first sample, or `None` while
    /// the encoder discards a source it already refused.
    #[pyo3(signature = (
        interleaved_sample_bytes,
        sample_rate,
        channels,
        sample_count,
        dtype,
        first_sample_timestamp_ns,
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
        first_sample_timestamp_ns: i64,
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
            first_sample_timestamp_ns,
        };
        let media_frame = self.encoder.encode_one_window(&window)?;
        Ok(media_frame.map(|media_frame| PyBytes::new(python, &media_frame)))
    }

    /// The catalog entry the last mint owes the output port, once.
    fn take_audio_entry_owed_to_the_port(&mut self) -> Option<OpusEncoderPortAudioEntryForPython> {
        self.encoder
            .take_audio_entry_owed_to_the_port()
            .map(|audio_entry| OpusEncoderPortAudioEntryForPython { audio_entry })
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

/// The audio catalog entry an encoder's output port carries for the instance
/// minted.
#[pyclass(name = "OpusEncoderPortAudioEntry", frozen)]
struct OpusEncoderPortAudioEntryForPython {
    audio_entry: OpusEncoderPortAudioEntry,
}

#[pymethods]
impl OpusEncoderPortAudioEntryForPython {
    #[getter]
    fn number_of_channels(&self) -> u32 {
        self.audio_entry.number_of_channels
    }

    #[getter]
    fn sample_rate(&self) -> u32 {
        self.audio_entry.sample_rate
    }

    /// The `OpusHead`, hex-encoded, as the entry's `description`.
    #[getter]
    fn hex_encoded_opus_identification_header(&self) -> &str {
        &self.audio_entry.hex_encoded_opus_identification_header
    }
}

/// Decodes Opus media frames into audio blocks.
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

    /// One arriving message decoded in the format the upstream port's catalog
    /// entry names — `{"section": ..., "entry": {...}}` as
    /// `inbound_link_catalog_entry` returns it — or `None` when it is
    /// discarded or was all encoder priming.
    #[pyo3(signature = (
        payload_bytes,
        timestamp_ns,
        starts_group,
        lost_since_last_read,
        upstream_catalog_entry,
    ))]
    fn decode_one_arriving_message(
        &mut self,
        payload_bytes: PyBuffer<u8>,
        timestamp_ns: i64,
        starts_group: bool,
        lost_since_last_read: u64,
        upstream_catalog_entry: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Option<DecodedAudioBlockForPython>> {
        let upstream_catalog_entry = upstream_catalog_entry
            .map(upstream_catalog_entry_from_python)
            .transpose()?;
        let decoded = self.decoder.decode_one_arriving_message(
            &ArrivingEncodedAudioMessage {
                payload_bytes: bytes_of_a_contiguous_buffer(&payload_bytes, "payload_bytes")?,
                timestamp_ns,
                starts_group,
                lost_since_last_read,
            },
            upstream_catalog_entry.as_ref(),
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

/// The fields of hang's audio entry the decoder reads, refusing by name a
/// document that is not one `inbound_link_catalog_entry` writes.
fn upstream_catalog_entry_from_python(
    catalog_entry: &Bound<'_, PyDict>,
) -> PyResult<UpstreamCatalogEntry> {
    let refused = |what: &str| {
        PyValueError::new_err(format!(
            "{OPUS_DECODER_PROCESSOR_NAME}: the upstream port's catalog entry {what}"
        ))
    };
    let section: String = catalog_entry
        .get_item("section")?
        .ok_or_else(|| refused("names no section"))?
        .extract()
        .map_err(|_| refused("names a section that is not a string"))?;
    let fields = catalog_entry
        .get_item("entry")?
        .ok_or_else(|| refused("carries no `entry`"))?;
    let fields = fields
        .cast::<PyDict>()
        .map_err(|_| refused("carries an `entry` that is not a mapping"))?;
    let optional_field = |name: &str| -> PyResult<Option<Bound<'_, PyAny>>> {
        Ok(fields.get_item(name)?.filter(|value| !value.is_none()))
    };
    Ok(UpstreamCatalogEntry {
        section,
        codec: optional_field("codec")?
            .map(|codec| codec.extract())
            .transpose()
            .map_err(|_| refused("names a `codec` that is not a string"))?,
        number_of_channels: optional_field("numberOfChannels")?
            .map(|channels| channels.extract())
            .transpose()
            .map_err(|_| refused("names a `numberOfChannels` that is not a count"))?,
        description: optional_field("description")?
            .map(|description| description.extract())
            .transpose()
            .map_err(|_| refused("carries a `description` that is not a string"))?,
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
    module.add_class::<OpusEncoderPortAudioEntryForPython>()?;
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
