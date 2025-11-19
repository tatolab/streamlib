// H.264 Format Conversion Utilities
//
// Functions for converting between different H.264 NAL unit formats:
// - AVCC (length-prefixed) - VideoToolbox default output
// - Annex B (start-code-prefixed) - WebRTC/RTP requirement

use super::ffi;

/// Convert AVCC format to Annex B format.
///
/// AVCC format (used by VideoToolbox):
/// - [4-byte length][NAL unit][4-byte length][NAL unit]...
/// - Length is big-endian u32
///
/// Annex B format (required by WebRTC/RTP):
/// - [00 00 00 01][NAL unit][00 00 00 01][NAL unit]...
/// - Start codes replace length prefixes
///
/// # Arguments
/// * `avcc_data` - H.264 data in AVCC format
///
/// # Returns
/// H.264 data in Annex B format
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
            tracing::error!("Invalid AVCC data: NAL length {} exceeds remaining data {}",
                nal_length, avcc_data.len() - pos);
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

/// Extract SPS and PPS parameter sets from CMFormatDescription and prepend to frame data.
///
/// For H.264 keyframes, decoders need SPS (Sequence Parameter Set) and PPS (Picture Parameter Set)
/// to initialize properly. VideoToolbox stores these in the CMFormatDescription.
///
/// # Arguments
/// * `sample_buffer` - CMSampleBuffer containing the format description
/// * `frame_data` - Encoded frame data (will be appended after parameter sets)
///
/// # Returns
/// Annex-B formatted data with: `[SPS][PPS][original frame data]`
///
/// # Safety
/// This function uses unsafe FFI calls to VideoToolbox/CoreMedia APIs.
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

    tracing::info!("[SPS/PPS] Found {} parameter sets (NAL header length: {})",
        parameter_set_count, nal_unit_header_length);

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

        tracing::info!("[SPS/PPS] Parameter set {}: {} ({} bytes, NAL type={})",
            i, param_name, param_size, nal_type);

        // Add start code + parameter set
        result.extend_from_slice(START_CODE);
        result.extend_from_slice(param_data);
    }

    // Append original frame data (already in Annex-B format after conversion)
    result.extend_from_slice(&frame_data);

    tracing::info!("[SPS/PPS] Total Annex-B data: {} bytes (SPS/PPS: {}, frame: {})",
        result.len(), result.len() - frame_data.len(), frame_data.len());

    result
}
