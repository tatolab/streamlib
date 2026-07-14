// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use thiserror::Error;

/// Errors surfaced by the JPEG parser + Huffman entropy decoder.
#[derive(Debug, Error)]
pub enum JpegError {
    #[error("unexpected end of input at byte {offset}")]
    UnexpectedEof { offset: usize },

    #[error("missing SOI marker — first two bytes were {first:#04x} {second:#04x}")]
    MissingSoi { first: u8, second: u8 },

    #[error("missing SOS marker before end of stream")]
    MissingSos,

    #[error("missing SOF marker before SOS")]
    MissingSof,

    #[error("missing quantization table id {id}")]
    MissingQuantizationTable { id: u8 },

    #[error("missing Huffman table (class {class}, id {id})")]
    MissingHuffmanTable { class: u8, id: u8 },

    #[error("unexpected marker {marker:#04x} at byte {offset}")]
    UnexpectedMarker { marker: u8, offset: usize },

    #[error("unsupported SOF marker {marker:#04x}: {reason}")]
    UnsupportedSof { marker: u8, reason: &'static str },

    #[error("invalid segment length {length} for marker {marker:#04x}")]
    InvalidSegmentLength { marker: u8, length: usize },

    #[error("malformed quantization table: {0}")]
    MalformedQuantizationTable(&'static str),

    #[error("malformed Huffman table: {0}")]
    MalformedHuffmanTable(&'static str),

    #[error("invalid Huffman code in entropy stream at byte {offset}")]
    InvalidHuffmanCode { offset: usize },

    #[error("invalid scan: {0}")]
    InvalidScan(&'static str),

    #[error("expected restart marker RST{expected} at byte {offset}, got {marker:#04x}")]
    UnexpectedRestartMarker {
        expected: u8,
        marker: u8,
        offset: usize,
    },

    #[error("unsupported feature: {0}")]
    Unsupported(&'static str),
}

pub type JpegResult<T> = Result<T, JpegError>;
