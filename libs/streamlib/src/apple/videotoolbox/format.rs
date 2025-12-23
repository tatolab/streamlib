// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// H.264 Format Conversion Utilities
//
// Functions for converting between different H.264 NAL unit formats:
// - AVCC (length-prefixed) - VideoToolbox default output
// - Annex B (start-code-prefixed) - WebRTC/RTP requirement
// - NAL unit parsing (auto-detect format)

use super::ffi;

// ============================================================================
// H.264 NAL UNIT PARSER
// ============================================================================

/// Parse NAL units from AVCC format (length-prefixed, used by VideoToolbox)
///
/// AVCC format: [4-byte length][NAL unit][4-byte length][NAL unit]...
/// Each NAL unit length is a 4-byte big-endian integer.
pub fn parse_nal_units_avcc(data: &[u8]) -> Vec<Vec<u8>> {
    let mut nal_units = Vec::new();
    let mut i = 0;

    while i + 4 <= data.len() {
        // Read 4-byte big-endian length
        let nal_length =
            u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;

        i += 4; // Skip length prefix

        // Check bounds
        if i + nal_length > data.len() {
            tracing::warn!(
                "AVCC NAL unit length {} exceeds remaining data {} at offset {}",
                nal_length,
                data.len() - i,
                i - 4
            );
            break;
        }

        // Extract NAL unit
        let nal_unit = data[i..i + nal_length].to_vec();
        if !nal_unit.is_empty() {
            nal_units.push(nal_unit);
        }

        i += nal_length;
    }

    nal_units
}

/// Parse NAL units from Annex B format (start-code-prefixed)
///
/// Annex B format: [00 00 00 01 or 00 00 01][NAL unit][start code][NAL unit]...
pub fn parse_nal_units_annex_b(data: &[u8]) -> Vec<Vec<u8>> {
    let mut nal_units = Vec::new();
    let mut i = 0;

    while i < data.len() {
        // Look for start code (4-byte or 3-byte)
        let start_code_len = if i + 3 < data.len()
            && data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1
        {
            4
        } else if i + 2 < data.len() && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            3
        } else {
            i += 1;
            continue;
        };

        // Find next start code (or end of data)
        let mut nal_end = i + start_code_len;
        while nal_end < data.len() {
            if (nal_end + 3 < data.len()
                && data[nal_end] == 0
                && data[nal_end + 1] == 0
                && data[nal_end + 2] == 0
                && data[nal_end + 3] == 1)
                || (nal_end + 2 < data.len()
                    && data[nal_end] == 0
                    && data[nal_end + 1] == 0
                    && data[nal_end + 2] == 1)
            {
                break;
            }
            nal_end += 1;
        }

        // Extract NAL unit (without start code)
        let nal_unit = data[i + start_code_len..nal_end].to_vec();
        if !nal_unit.is_empty() {
            nal_units.push(nal_unit);
        }

        i = nal_end;
    }

    nal_units
}

/// Auto-detect format and parse NAL units
///
/// Checks first 4 bytes to determine if data is AVCC or Annex B format:
/// - AVCC: First 4 bytes = big-endian length (typically < 100KB for a frame)
/// - Annex B: Starts with 00 00 00 01 or 00 00 01
pub fn parse_nal_units(data: &[u8]) -> Vec<Vec<u8>> {
    if data.len() < 4 {
        return Vec::new();
    }

    // Check for Annex B start codes
    let is_annex_b = (data[0] == 0 && data[1] == 0 && data[2] == 0 && data[3] == 1)
        || (data[0] == 0 && data[1] == 0 && data[2] == 1);

    if is_annex_b {
        tracing::info!("🔍 [NAL Parser] Detected Annex B format H.264 (start code present)");
        parse_nal_units_annex_b(data)
    } else {
        // Assume AVCC format (VideoToolbox default on macOS)
        let length = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;

        // Sanity check: length should be reasonable (< 1MB for a single NAL unit)
        if length > 0 && length < 1_000_000 && length + 4 <= data.len() {
            tracing::info!("🔍 [NAL Parser] Detected AVCC format H.264 (first NAL length: {}, total data: {} bytes)", length, data.len());
            parse_nal_units_avcc(data)
        } else {
            tracing::error!(
                "❌ [NAL Parser] UNKNOWN H.264 FORMAT! First 4 bytes = {:02x} {:02x} {:02x} {:02x} (interpreted length={}, total data={})",
                data[0], data[1], data[2], data[3], length, data.len()
            );
            tracing::error!(
                "❌ [NAL Parser] This will result in NO NAL units parsed - stream will fail!"
            );
            Vec::new()
        }
    }
}

// ============================================================================
// H.264 FORMAT CONVERSION
// ============================================================================

/// Convert AVCC format to Annex B format.
pub fn avcc_to_annex_b(avcc_data: &[u8]) -> Vec<u8> {
    const START_CODE: &[u8] = &[0x00, 0x00, 0x00, 0x01];
    let mut annex_b = Vec::with_capacity(avcc_data.len() + 128); // Extra space for start codes

    let mut pos = 0;
    while pos + 4 <= avcc_data.len() {
        // Read 4-byte length prefix (big-endian)
        let nal_length = u32::from_be_bytes([
            avcc_data[pos],
            avcc_data[pos + 1],
            avcc_data[pos + 2],
            avcc_data[pos + 3],
        ]) as usize;

        pos += 4;

        if pos + nal_length > avcc_data.len() {
            tracing::error!(
                "Invalid AVCC data: NAL length {} exceeds remaining data {}",
                nal_length,
                avcc_data.len() - pos
            );
            break;
        }

        // Add start code
        annex_b.extend_from_slice(START_CODE);

        // Add NAL unit
        annex_b.extend_from_slice(&avcc_data[pos..pos + nal_length]);

        pos += nal_length;
    }

    annex_b
}

/// Convert Annex B format to AVCC format.
pub fn annex_b_to_avcc(annex_b_data: &[u8]) -> crate::core::Result<Vec<u8>> {
    use crate::core::StreamError;

    let mut avcc = Vec::with_capacity(annex_b_data.len());
    let mut pos = 0;

    while pos < annex_b_data.len() {
        // Look for start code (0x00000001 or 0x000001)
        let start_code_len = if pos + 4 <= annex_b_data.len()
            && annex_b_data[pos..pos + 4] == [0x00, 0x00, 0x00, 0x01]
        {
            4
        } else if pos + 3 <= annex_b_data.len() && annex_b_data[pos..pos + 3] == [0x00, 0x00, 0x01]
        {
            3
        } else if pos == 0 {
            // If we don't find a start code at the beginning, data might already be AVCC
            tracing::warn!(
                "[Annex B → AVCC] No start code found at position 0, data may already be AVCC"
            );
            return Err(StreamError::Runtime(
                "Invalid Annex B data: no start code at beginning".to_string(),
            ));
        } else {
            // Skip byte and continue looking
            pos += 1;
            continue;
        };

        pos += start_code_len;

        // Find next start code or end of data
        let nal_end = if let Some(next_pos) = find_next_start_code(&annex_b_data[pos..]) {
            pos + next_pos
        } else {
            annex_b_data.len()
        };

        let nal_length = nal_end - pos;

        // Write 4-byte length prefix (big-endian)
        avcc.extend_from_slice(&(nal_length as u32).to_be_bytes());

        // Write NAL unit data
        avcc.extend_from_slice(&annex_b_data[pos..nal_end]);

        pos = nal_end;
    }

    Ok(avcc)
}

/// Find the next start code (0x00000001 or 0x000001) in the data
fn find_next_start_code(data: &[u8]) -> Option<usize> {
    for i in 0..data.len() {
        if i + 4 <= data.len() && data[i..i + 4] == [0x00, 0x00, 0x00, 0x01] {
            return Some(i);
        }
        if i + 3 <= data.len() && data[i..i + 3] == [0x00, 0x00, 0x01] {
            return Some(i);
        }
    }
    None
}

/// Extract SPS and PPS parameter sets from CMFormatDescription and prepend to frame data.
///
/// # Safety
/// Uses unsafe FFI calls to VideoToolbox/CoreMedia APIs.
pub unsafe fn extract_h264_parameters(
    sample_buffer: ffi::CMSampleBufferRef,
    frame_data: Vec<u8>,
) -> Vec<u8> {
    const START_CODE: &[u8] = &[0x00, 0x00, 0x00, 0x01];

    // Get format description from sample buffer
    let format_desc = ffi::CMSampleBufferGetFormatDescription(sample_buffer);
    if format_desc.is_null() {
        tracing::warn!("[SPS/PPS] CMSampleBufferGetFormatDescription returned null");
        return frame_data;
    }

    let mut result = Vec::new();

    // Extract parameter sets (SPS and PPS)
    let mut parameter_set_count: usize = 0;
    let mut nal_unit_header_length: i32 = 0;

    // Get parameter set count
    let status = ffi::CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        format_desc,
        0,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut parameter_set_count,
        &mut nal_unit_header_length,
    );

    if status != ffi::NO_ERR {
        tracing::warn!("[SPS/PPS] Failed to get parameter set count: {}", status);
        return frame_data;
    }

    tracing::info!(
        "[SPS/PPS] Found {} parameter sets (NAL header length: {})",
        parameter_set_count,
        nal_unit_header_length
    );

    // Extract each parameter set (typically SPS at index 0, PPS at index 1)
    for i in 0..parameter_set_count {
        let mut param_ptr: *const u8 = std::ptr::null();
        let mut param_size: usize = 0;

        let status = ffi::CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            format_desc,
            i,
            &mut param_ptr,
            &mut param_size,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );

        if status != ffi::NO_ERR {
            tracing::warn!("[SPS/PPS] Failed to get parameter set {}: {}", i, status);
            continue;
        }

        if param_ptr.is_null() || param_size == 0 {
            tracing::warn!("[SPS/PPS] Parameter set {} is empty", i);
            continue;
        }

        // Copy parameter set data
        let param_data = std::slice::from_raw_parts(param_ptr, param_size);

        // Determine parameter set type from NAL unit type (bits 0-4 of first byte)
        let nal_type = param_data[0] & 0x1F;
        let param_name = match nal_type {
            7 => "SPS",
            8 => "PPS",
            _ => "Unknown",
        };

        tracing::info!(
            "[SPS/PPS] Parameter set {}: {} ({} bytes, NAL type={})",
            i,
            param_name,
            param_size,
            nal_type
        );

        // Add start code + parameter set
        result.extend_from_slice(START_CODE);
        result.extend_from_slice(param_data);
    }

    // Append original frame data (already in Annex-B format after conversion)
    result.extend_from_slice(&frame_data);

    tracing::info!(
        "[SPS/PPS] Total Annex-B data: {} bytes (SPS/PPS: {}, frame: {})",
        result.len(),
        result.len() - frame_data.len(),
        frame_data.len()
    );

    result
}
