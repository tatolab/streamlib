// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::color::{
    parse_app0, parse_app14_adobe, parse_app1_exif_color_space, parse_app2_icc_fragment,
    IccAccumulator, JpegColorInfo,
};
use crate::error::{JpegError, JpegResult};
use crate::header::{
    FrameComponent, FrameHeader, HuffmanTables, QuantizationTable, ScanComponent, ScanHeader,
};
use crate::huffman::{HuffmanClass, HuffmanTable};
use crate::marker;

/// Header-section result: everything parsed up to and including the SOS
/// segment, plus the byte offset where the entropy-coded data starts.
pub(crate) struct ParsedHeaders<'a> {
    pub frame: FrameHeader,
    pub quant_tables: Vec<QuantizationTable>,
    pub huffman: HuffmanTables,
    pub scan: ScanHeader,
    pub restart_interval: u16,
    pub entropy_data: &'a [u8],
    pub color_info: JpegColorInfo,
}

/// Walk a JPEG bitstream from SOI through SOS, returning the parsed
/// headers and a slice over the entropy-coded segment.
pub(crate) fn parse_headers(bytes: &[u8]) -> JpegResult<ParsedHeaders<'_>> {
    let mut parser = Parser::new(bytes);
    parser.expect_soi()?;

    let mut quant_tables: Vec<QuantizationTable> = Vec::new();
    let mut huffman = HuffmanTables::default();
    let mut frame: Option<FrameHeader> = None;
    let mut restart_interval: u16 = 0;
    let mut color_info = JpegColorInfo::default();
    let mut icc_accum = IccAccumulator::default();

    loop {
        let marker_byte = parser.next_marker()?;
        match marker_byte {
            marker::DQT => parser.parse_dqt(&mut quant_tables)?,
            marker::DHT => parser.parse_dht(&mut huffman)?,
            marker::SOF0 => {
                if frame.is_some() {
                    return Err(JpegError::InvalidScan("duplicate SOF segment"));
                }
                frame = Some(parser.parse_sof0()?);
            }
            marker::DRI => restart_interval = parser.parse_dri()?,
            marker::SOS => {
                let frame = frame.take().ok_or(JpegError::MissingSof)?;
                let scan = parser.parse_sos(&frame)?;
                let entropy_data = &bytes[parser.cursor..];
                // Cache the finished ICC profile (if any) before
                // handing the result to the caller. Incomplete
                // multi-segment ICC data is discarded silently —
                // the spec allows partial profiles but the kernel
                // can't act on them.
                color_info.icc_profile = icc_accum.try_finish();
                return Ok(ParsedHeaders {
                    frame,
                    quant_tables,
                    huffman,
                    scan,
                    restart_interval,
                    entropy_data,
                    color_info,
                });
            }
            marker::EOI => return Err(JpegError::MissingSos),
            marker::COM => parser.skip_segment(marker_byte)?,
            marker::APP0 => {
                let payload = parser.read_segment_payload(marker::APP0)?;
                if let Some(jfif) = parse_app0(payload)? {
                    color_info.jfif = Some(jfif);
                }
            }
            marker::APP1 => {
                let payload = parser.read_segment_payload(marker::APP1)?;
                if let Some(cs) = parse_app1_exif_color_space(payload)? {
                    color_info.exif_color_space = Some(cs);
                }
            }
            marker::APP2 => {
                let payload = parser.read_segment_payload(marker::APP2)?;
                if let Some(fragment) = parse_app2_icc_fragment(payload)? {
                    icc_accum.ingest(fragment)?;
                }
            }
            marker::APP14 => {
                let payload = parser.read_segment_payload(marker::APP14)?;
                if let Some(adobe) = parse_app14_adobe(payload)? {
                    color_info.adobe = Some(adobe);
                }
            }
            other if marker::is_app(other) => parser.skip_segment(other)?,
            other => {
                if let Some(reason) = marker::is_unsupported_sof(other) {
                    return Err(JpegError::UnsupportedSof {
                        marker: other,
                        reason,
                    });
                }
                if marker::is_standalone(other) {
                    return Err(JpegError::UnexpectedMarker {
                        marker: other,
                        offset: parser.cursor,
                    });
                }
                // Unknown but length-prefixed — skip it defensively.
                parser.skip_segment(other)?;
            }
        }
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Parser<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn expect_soi(&mut self) -> JpegResult<()> {
        if self.bytes.len() < 2 {
            return Err(JpegError::UnexpectedEof { offset: 0 });
        }
        if self.bytes[0] != marker::PREFIX || self.bytes[1] != marker::SOI {
            return Err(JpegError::MissingSoi {
                first: self.bytes[0],
                second: self.bytes[1],
            });
        }
        self.cursor = 2;
        Ok(())
    }

    /// Read the next marker byte (the byte after `0xFF`). JPEG allows
    /// fill `0xFF` bytes before a marker; we consume them.
    fn next_marker(&mut self) -> JpegResult<u8> {
        loop {
            let b = self.read_byte()?;
            if b != marker::PREFIX {
                return Err(JpegError::UnexpectedMarker {
                    marker: b,
                    offset: self.cursor - 1,
                });
            }
            let m = self.read_byte()?;
            if m == marker::PREFIX {
                // Fill byte (0xFF followed by 0xFF) — keep scanning.
                continue;
            }
            if m == 0 {
                return Err(JpegError::UnexpectedMarker {
                    marker: 0,
                    offset: self.cursor - 1,
                });
            }
            return Ok(m);
        }
    }

    fn read_byte(&mut self) -> JpegResult<u8> {
        if self.cursor >= self.bytes.len() {
            return Err(JpegError::UnexpectedEof {
                offset: self.cursor,
            });
        }
        let b = self.bytes[self.cursor];
        self.cursor += 1;
        Ok(b)
    }

    fn read_u16(&mut self) -> JpegResult<u16> {
        let high = self.read_byte()?;
        let low = self.read_byte()?;
        Ok(((high as u16) << 8) | (low as u16))
    }

    fn read_segment_payload(&mut self, marker_byte: u8) -> JpegResult<&'a [u8]> {
        let length = self.read_u16()? as usize;
        if length < 2 {
            return Err(JpegError::InvalidSegmentLength {
                marker: marker_byte,
                length,
            });
        }
        let payload_len = length - 2;
        let start = self.cursor;
        let end = start
            .checked_add(payload_len)
            .ok_or(JpegError::UnexpectedEof { offset: start })?;
        if end > self.bytes.len() {
            return Err(JpegError::UnexpectedEof { offset: start });
        }
        self.cursor = end;
        Ok(&self.bytes[start..end])
    }

    fn skip_segment(&mut self, marker_byte: u8) -> JpegResult<()> {
        self.read_segment_payload(marker_byte)?;
        Ok(())
    }

    fn parse_dqt(&mut self, out: &mut Vec<QuantizationTable>) -> JpegResult<()> {
        let payload = self.read_segment_payload(marker::DQT)?;
        let mut cursor = 0;
        while cursor < payload.len() {
            if cursor + 1 > payload.len() {
                return Err(JpegError::MalformedQuantizationTable(
                    "missing precision/id byte",
                ));
            }
            let pq_tq = payload[cursor];
            cursor += 1;
            let precision = pq_tq >> 4;
            let id = pq_tq & 0x0F;
            if precision > 1 {
                return Err(JpegError::MalformedQuantizationTable(
                    "precision must be 0 (8-bit) or 1 (16-bit)",
                ));
            }
            if id > 3 {
                return Err(JpegError::MalformedQuantizationTable("id must be 0..=3"));
            }
            let bytes_per_value = if precision == 0 { 1 } else { 2 };
            let needed = 64 * bytes_per_value;
            if cursor + needed > payload.len() {
                return Err(JpegError::MalformedQuantizationTable(
                    "segment too short for 64 values",
                ));
            }
            let mut values = [0u16; 64];
            for value in values.iter_mut() {
                if precision == 0 {
                    *value = payload[cursor] as u16;
                    cursor += 1;
                } else {
                    *value = ((payload[cursor] as u16) << 8) | (payload[cursor + 1] as u16);
                    cursor += 2;
                }
            }
            if let Some(existing) = out.iter_mut().find(|t| t.id == id) {
                *existing = QuantizationTable {
                    id,
                    precision,
                    values,
                };
            } else {
                out.push(QuantizationTable {
                    id,
                    precision,
                    values,
                });
            }
        }
        Ok(())
    }

    fn parse_dht(&mut self, out: &mut HuffmanTables) -> JpegResult<()> {
        let payload = self.read_segment_payload(marker::DHT)?;
        let mut cursor = 0;
        while cursor < payload.len() {
            if cursor + 17 > payload.len() {
                return Err(JpegError::MalformedHuffmanTable(
                    "DHT segment too short for class/id + BITS",
                ));
            }
            let tc_th = payload[cursor];
            cursor += 1;
            let class = HuffmanClass::from_class_byte(tc_th >> 4)?;
            let id = tc_th & 0x0F;
            if id > 3 {
                return Err(JpegError::MalformedHuffmanTable("id must be 0..=3"));
            }
            let mut bits = [0u8; 16];
            bits.copy_from_slice(&payload[cursor..cursor + 16]);
            cursor += 16;
            let total: usize = bits.iter().map(|&b| b as usize).sum();
            if cursor + total > payload.len() {
                return Err(JpegError::MalformedHuffmanTable(
                    "DHT segment too short for HUFFVAL",
                ));
            }
            let huffval = &payload[cursor..cursor + total];
            cursor += total;
            let table = HuffmanTable::build(class, id, &bits, huffval)?;
            let slot = match class {
                HuffmanClass::Dc => &mut out.dc[id as usize],
                HuffmanClass::Ac => &mut out.ac[id as usize],
            };
            *slot = Some(table);
        }
        Ok(())
    }

    fn parse_sof0(&mut self) -> JpegResult<FrameHeader> {
        let payload = self.read_segment_payload(marker::SOF0)?;
        if payload.len() < 6 {
            return Err(JpegError::InvalidScan("SOF0 payload too short"));
        }
        let precision = payload[0];
        if precision != 8 {
            return Err(JpegError::UnsupportedSof {
                marker: marker::SOF0,
                reason: "baseline JPEG requires 8-bit sample precision",
            });
        }
        let height = ((payload[1] as u16) << 8) | (payload[2] as u16);
        let width = ((payload[3] as u16) << 8) | (payload[4] as u16);
        let num_components = payload[5] as usize;
        if num_components == 0 || num_components > 4 {
            return Err(JpegError::InvalidScan("SOF0 component count out of range"));
        }
        if width == 0 || height == 0 {
            return Err(JpegError::InvalidScan("SOF0 width/height is zero"));
        }
        if payload.len() < 6 + 3 * num_components {
            return Err(JpegError::InvalidScan(
                "SOF0 payload too short for component descriptors",
            ));
        }
        let mut components = Vec::with_capacity(num_components);
        let mut max_h_sampling = 0u8;
        let mut max_v_sampling = 0u8;
        for i in 0..num_components {
            let base = 6 + i * 3;
            let id = payload[base];
            let sampling = payload[base + 1];
            let h_sampling = sampling >> 4;
            let v_sampling = sampling & 0x0F;
            let quant_table_id = payload[base + 2];
            if !(1..=4).contains(&h_sampling) || !(1..=4).contains(&v_sampling) {
                return Err(JpegError::InvalidScan(
                    "SOF0 sampling factor out of range (must be 1..=4)",
                ));
            }
            if quant_table_id > 3 {
                return Err(JpegError::InvalidScan(
                    "SOF0 quantization table id out of range",
                ));
            }
            if components.iter().any(|c: &FrameComponent| c.id == id) {
                return Err(JpegError::InvalidScan("SOF0 duplicate component id"));
            }
            max_h_sampling = max_h_sampling.max(h_sampling);
            max_v_sampling = max_v_sampling.max(v_sampling);
            components.push(FrameComponent {
                id,
                h_sampling,
                v_sampling,
                quant_table_id,
            });
        }
        Ok(FrameHeader {
            precision,
            height,
            width,
            components,
            max_h_sampling,
            max_v_sampling,
        })
    }

    fn parse_dri(&mut self) -> JpegResult<u16> {
        let payload = self.read_segment_payload(marker::DRI)?;
        if payload.len() != 2 {
            return Err(JpegError::InvalidSegmentLength {
                marker: marker::DRI,
                length: payload.len() + 2,
            });
        }
        Ok(((payload[0] as u16) << 8) | (payload[1] as u16))
    }

    fn parse_sos(&mut self, frame: &FrameHeader) -> JpegResult<ScanHeader> {
        let payload = self.read_segment_payload(marker::SOS)?;
        if payload.is_empty() {
            return Err(JpegError::InvalidScan("SOS payload empty"));
        }
        let ns = payload[0] as usize;
        if ns == 0 || ns > frame.components.len() {
            return Err(JpegError::InvalidScan(
                "SOS component count outside frame components",
            ));
        }
        if payload.len() < 1 + 2 * ns + 3 {
            return Err(JpegError::InvalidScan("SOS payload too short"));
        }
        let mut components = Vec::with_capacity(ns);
        for i in 0..ns {
            let base = 1 + i * 2;
            let component_id = payload[base];
            let tdta = payload[base + 1];
            let dc_table_id = tdta >> 4;
            let ac_table_id = tdta & 0x0F;
            if dc_table_id > 3 || ac_table_id > 3 {
                return Err(JpegError::InvalidScan("SOS table id out of range"));
            }
            if !frame.components.iter().any(|c| c.id == component_id) {
                return Err(JpegError::InvalidScan(
                    "SOS references component not in SOF",
                ));
            }
            components.push(ScanComponent {
                component_id,
                dc_table_id,
                ac_table_id,
            });
        }
        let trailer = &payload[1 + 2 * ns..];
        let spectral_start = trailer[0];
        let spectral_end = trailer[1];
        let succ = trailer[2];
        let successive_high = succ >> 4;
        let successive_low = succ & 0x0F;
        if spectral_start != 0 || spectral_end != 63 || successive_high != 0 || successive_low != 0
        {
            return Err(JpegError::Unsupported(
                "progressive / spectral-selection / successive-approximation scans",
            ));
        }
        Ok(ScanHeader {
            components,
            spectral_start,
            spectral_end,
            successive_high,
            successive_low,
        })
    }
}
