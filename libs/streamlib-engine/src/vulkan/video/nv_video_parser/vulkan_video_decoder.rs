// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Faithful port of:
//!   - VulkanVideoDecoder.h   (include/VulkanVideoDecoder.h)
//!   - VulkanVideoDecoder.cpp (src/VulkanVideoDecoder.cpp)
//!   - NextStartCodeC.cpp     (src/NextStartCodeC.cpp, plain C fallback)
//!   - ByteStreamParser.h     (include/ByteStreamParser.h, ParseByteStreamSimd template)
//!
//! from nvpro-samples/vk_video_decoder/libs/NvVideoParser.
//!
//! Key Rust divergences:
//!   - C++ virtual dispatch (VulkanVideoDecoder base class + derived) is modeled as:
//!       * `VideoDecoderCodec` trait for codec-specific hooks (pure virtuals in C++)
//!       * `VulkanVideoDecoder<C: VideoDecoderCodec>` struct holding common state
//!   - C++ `VkParserVideoDecodeClient` virtual interface becomes `VideoDecodeClient` trait.
//!   - Exp-Golomb helpers (`ue`, `se`) are methods on the decoder (matching C++) but also
//!     exposed as free functions for standalone unit testing.
//!   - SIMD start-code search is omitted; only the plain C fallback is ported inline.
//!   - `VulkanBitstreamBufferStream` is represented by a `BitstreamData` abstraction that
//!     wraps a `Vec<u8>` plus stream markers, since we do not share GPU bitstream buffers
//!     in the parser layer itself. The client is responsible for GPU buffer management.

use vulkanalia::vk;
use std::sync::atomic::{AtomicI32, Ordering};

// ---------------------------------------------------------------------------
// Constants (mirrors C++ enums on VulkanVideoDecoder)
// ---------------------------------------------------------------------------

/// Up to 8K slices per picture.
pub const MAX_SLICES: usize = 8192;
/// Maximum frame delay between decode and display.
pub const MAX_DELAY: usize = 32;
/// Size of PTS queue.
pub const MAX_QUEUED_PTS: usize = 16;

/// NAL unit disposition returned by `VideoDecoderCodec::parse_nal_unit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum NalUnitType {
    /// Discard this NAL unit.
    Discard = 0,
    /// This NALU contains picture data (keep).
    Slice = 1,
    /// This NALU type is not supported (callback client).
    Unknown = 2,
}

/// Codec error state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum NvCodecError {
    NoError = 0,
    NonCompliantStream = 1,
}

// ---------------------------------------------------------------------------
// NV_VULKAN_VIDEO_PARSER_API_VERSION — mirrors VulkanVideoParserIf.h
// ---------------------------------------------------------------------------

/// Packed version `(0, 9, 9)` matching `VK_MAKE_VIDEO_STD_VERSION(0, 9, 9)`.
pub const NV_VULKAN_VIDEO_PARSER_API_VERSION: u32 = vk_make_video_std_version(0, 9, 9);

const fn vk_make_video_std_version(major: u32, minor: u32, patch: u32) -> u32 {
    (major << 22) | (minor << 12) | patch
}

// ---------------------------------------------------------------------------
// Frame-rate helpers (mirrors nvVulkanVideoUtils.h macros)
// ---------------------------------------------------------------------------

/// Pack a frame rate from numerator and denominator into 18+14 bits.
pub fn pack_frame_rate(mut numerator: u32, mut denominator: u32) -> u32 {
    while numerator >= (1 << 18) || denominator >= (1 << 14) {
        if numerator % 5 == 0 && denominator % 5 == 0 {
            numerator /= 5;
            denominator /= 5;
        } else if (numerator | denominator) & 1 != 0
            && numerator % 3 == 0
            && denominator % 3 == 0
        {
            numerator /= 3;
            denominator /= 3;
        } else {
            numerator = (numerator + 1) >> 1;
            denominator = (denominator + 1) >> 1;
        }
    }
    make_frame_rate(numerator, denominator)
}

#[inline]
pub const fn make_frame_rate(num: u32, den: u32) -> u32 {
    (num << 14) | den
}

#[inline]
pub const fn frame_rate_num(rate: u32) -> u32 {
    rate >> 14
}

#[inline]
pub const fn frame_rate_den(rate: u32) -> u32 {
    rate & 0x3fff
}

// ---------------------------------------------------------------------------
// NvVkNalUnit — mirrors the C struct
// ---------------------------------------------------------------------------

/// NAL unit reading state.
#[derive(Debug, Clone, Default)]
pub struct NvVkNalUnit {
    /// Start offset in byte stream buffer.
    pub start_offset: i64,
    /// End offset in byte stream buffer.
    pub end_offset: i64,
    /// Current read pointer in this NALU.
    pub get_offset: i64,
    /// Zero byte count (for emulation prevention).
    pub get_zerocnt: i32,
    /// Bit buffer for reading.
    pub get_bfr: u32,
    /// Offset in bit buffer.
    pub get_bfroffs: u32,
    /// Emulation prevention byte count.
    pub get_emulcnt: u32,
}

// ---------------------------------------------------------------------------
// NvVkPresentationInfo — mirrors the C struct
// ---------------------------------------------------------------------------

/// Presentation information stored with every decoded frame.
#[derive(Debug, Clone)]
pub struct NvVkPresentationInfo {
    /// Opaque picture buffer identifier (index or handle).
    pub pic_buf: Option<usize>,
    /// Number of displayed fields (to compute frame duration).
    pub num_fields: i32,
    /// True if frame was not decoded (skip display).
    pub skipped: bool,
    /// Frame has an associated PTS.
    pub pts_valid: bool,
    /// Picture Order Count (for initial PTS interpolation).
    pub poc: i32,
    /// Frame presentation time.
    pub pts: i64,
    /// Discontinuity before this PTS — do not check for out of order.
    pub discontinuity: bool,
}

impl Default for NvVkPresentationInfo {
    fn default() -> Self {
        Self {
            pic_buf: None,
            num_fields: 0,
            skipped: false,
            pts_valid: false,
            poc: 0,
            pts: 0,
            discontinuity: false,
        }
    }
}

// ---------------------------------------------------------------------------
// PTS queue entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct PtsQueueEntry {
    pub pts_valid: bool,
    pub pts: i64,
    pub pts_pos: i64,
    pub discontinuity: bool,
}

// ---------------------------------------------------------------------------
// Sequence info — mirrors VkParserSequenceInfo (subset needed here)
// ---------------------------------------------------------------------------

/// Sequence information.  Mirrors `VkParserSequenceInfo` from the C++ header.
///
/// Fields match the C struct 1-to-1 so that codec parsers (H.264, H.265, etc.)
/// can populate them directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VkParserSequenceInfo {
    pub codec: vk::VideoCodecOperationFlagsKHR,
    pub is_svc: bool,
    pub frame_rate: u32,
    pub prog_seq: bool,
    pub display_width: i32,
    pub display_height: i32,
    pub coded_width: i32,
    pub coded_height: i32,
    pub display_offset_x: i32,
    pub display_offset_y: i32,
    pub max_width: i32,
    pub max_height: i32,
    pub chroma_format: u8,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
    pub video_full_range: u8,
    pub bitrate: i32,
    pub dar_width: i32,
    pub dar_height: i32,
    pub video_format: i32,
    pub color_primaries: i32,
    pub transfer_characteristics: i32,
    pub matrix_coefficients: i32,
    pub cb_sequence_header: i32,
    pub min_num_dpb_slots: i32,
    pub min_num_decode_surfaces: i32,
    pub sequence_header_data: [u8; 1024],
    pub codec_profile: u32,
    pub has_film_grain: bool,
    pub can_use_fields: bool,
}

impl Default for VkParserSequenceInfo {
    fn default() -> Self {
        Self {
            codec: vk::VideoCodecOperationFlagsKHR::empty(),
            is_svc: false,
            frame_rate: 0,
            prog_seq: false,
            display_width: 0,
            display_height: 0,
            coded_width: 0,
            coded_height: 0,
            display_offset_x: 0,
            display_offset_y: 0,
            max_width: 0,
            max_height: 0,
            chroma_format: 0,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            video_full_range: 0,
            bitrate: 0,
            dar_width: 0,
            dar_height: 0,
            video_format: 0,
            color_primaries: 0,
            transfer_characteristics: 0,
            matrix_coefficients: 0,
            cb_sequence_header: 0,
            min_num_dpb_slots: 0,
            min_num_decode_surfaces: 0,
            sequence_header_data: [0u8; 1024],
            codec_profile: 0,
            has_film_grain: false,
            can_use_fields: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Display mastering info
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct VkParserDisplayMasteringInfo {
    pub display_primaries_x: [u16; 3],
    pub display_primaries_y: [u16; 3],
    pub white_point_x: u16,
    pub white_point_y: u16,
    pub max_display_mastering_luminance: u32,
    pub min_display_mastering_luminance: u32,
}

// ---------------------------------------------------------------------------
// Bitstream packet (input to ParseByteStream)
// ---------------------------------------------------------------------------

/// Input packet for `parse_byte_stream`.  Mirrors `VkParserBitstreamPacket`.
#[derive(Debug, Clone)]
pub struct BitstreamPacket<'a> {
    pub byte_stream: &'a [u8],
    pub pts: i64,
    pub eos: bool,
    pub pts_valid: bool,
    pub discontinuity: bool,
    pub partial_parsing: bool,
    pub eop: bool,
}

// ---------------------------------------------------------------------------
// Init parameters
// ---------------------------------------------------------------------------

/// Initialization parameters.  Mirrors `VkParserInitDecodeParameters`.
pub struct InitDecodeParameters<'a, C: VideoDecodeClient> {
    pub interface_version: u32,
    pub client: &'a mut C,
    pub default_min_buffer_size: u32,
    pub buffer_offset_alignment: u32,
    pub buffer_size_alignment: u32,
    pub reference_clock_rate: u64,
    pub error_threshold: i32,
    pub external_seq_info: Option<VkParserSequenceInfo>,
    pub out_of_band_picture_parameters: bool,
}

// ---------------------------------------------------------------------------
// Picture data — simplified Rust version of VkParserPictureData
// ---------------------------------------------------------------------------

/// Codec-specific picture data (union in C++).
///
/// This is intentionally left as an opaque blob that each codec fills in.
/// The client's `decode_picture` receives a reference to the full
/// `PictureData` and can downcast `codec_specific` based on the codec.
#[derive(Debug, Clone, Default)]
pub struct PictureData {
    pub pic_width_in_mbs: i32,
    pub frame_height_in_mbs: i32,
    /// Index of current picture buffer.
    pub curr_pic: Option<usize>,
    pub field_pic_flag: bool,
    pub bottom_field_flag: bool,
    pub second_field: bool,
    pub progressive_frame: bool,
    pub top_field_first: bool,
    pub repeat_first_field: u8,
    pub ref_pic_flag: bool,
    pub intra_pic_flag: bool,
    pub chroma_format: i32,
    pub picture_order_count: i32,
    pub current_dpb_id: i8,

    // Bitstream data
    pub first_slice_index: u32,
    pub num_slices: u32,
    pub bitstream_data_offset: usize,
    pub bitstream_data_len: usize,
    /// Snapshot of the bitstream data for this picture.
    pub bitstream_data: Vec<u8>,
    /// Slice offset markers.
    pub slice_offsets: Vec<u32>,
}

// ---------------------------------------------------------------------------
// VideoDecodeClient trait — mirrors VkParserVideoDecodeClient
// ---------------------------------------------------------------------------

/// Trait that the application implements to receive decoded pictures.
///
/// Mirrors `VkParserVideoDecodeClient` from the C++ code.
pub trait VideoDecodeClient {
    /// Called when a new sequence is detected.  Returns max number of
    /// reference frames the client can manage, or 0 on failure.
    fn begin_sequence(&mut self, seq_info: &VkParserSequenceInfo) -> i32;

    /// Allocate a new picture buffer.  Returns `Some(id)` on success.
    fn alloc_picture_buffer(&mut self) -> Option<usize>;

    /// Decode a picture.  Returns `true` on success.
    fn decode_picture(&mut self, picture_data: &PictureData) -> bool;

    /// Display a decoded picture at the given PTS.
    fn display_picture(&mut self, pic_buf: usize, pts: i64) -> bool;

    /// Called for NALUs the parser does not handle.
    fn unhandled_nalu(&mut self, data: &[u8]);

    /// Return decode capability flags.
    fn get_decode_caps(&self) -> u32 {
        0
    }
}

// ---------------------------------------------------------------------------
// VideoDecoderCodec trait — mirrors the pure virtual methods on the C++ class
// ---------------------------------------------------------------------------

/// Codec-specific hooks that derived parsers (H.264, H.265, AV1, VP9)
/// must implement.  This replaces the C++ pure virtual methods on
/// `VulkanVideoDecoder`.
pub trait VideoDecoderCodec {
    /// Initialize codec-specific parser state.  Called during `Initialize`.
    fn init_parser(&mut self);

    /// Returns `true` if the current NAL unit belongs to a new picture.
    fn is_picture_boundary(&mut self, decoder: &VulkanVideoDecoder, rbsp_size: i32) -> bool;

    /// Parse the current NAL unit.  Must return a `NalUnitType`.
    fn parse_nal_unit(&mut self, decoder: &mut VulkanVideoDecoder) -> NalUnitType;

    /// Fill in picture data.  Return `true` if picture should be sent to client.
    fn begin_picture(&mut self, decoder: &VulkanVideoDecoder, picture_data: &mut PictureData) -> bool;

    /// Called after a picture has been decoded.
    fn end_picture(&mut self) {}

    /// Called to reset parser at end of stream.
    fn end_of_stream(&mut self) {}

    /// Create codec-specific private context.
    fn create_private_context(&mut self);

    /// Free codec-specific context.
    fn free_context(&mut self);

    /// Get display mastering info (optional).
    fn get_display_mastering_info(&self, _info: &mut VkParserDisplayMasteringInfo) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// BitstreamData — simplified port of VulkanBitstreamBufferStream
// ---------------------------------------------------------------------------

/// In-memory bitstream buffer with slice markers.  This replaces the C++
/// `VulkanBitstreamBufferStream` + `VulkanBitstreamBuffer` pair for the
/// parser layer.  Actual GPU buffer management is the client's responsibility.
#[derive(Debug, Clone, Default)]
pub struct BitstreamData {
    data: Vec<u8>,
    stream_markers: Vec<u32>,
}

impl BitstreamData {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: vec![0u8; capacity],
            stream_markers: Vec::new(),
        }
    }

    #[inline]
    pub fn is_valid(&self) -> bool {
        !self.data.is_empty()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    #[inline]
    pub fn get(&self, offset: i64) -> u8 {
        self.data[offset as usize]
    }

    #[inline]
    pub fn data_ptr(&self) -> &[u8] {
        &self.data
    }

    pub fn copy_from_slice(&mut self, src: &[u8], dst_offset: usize, count: usize) {
        let end = dst_offset + count;
        if end > self.data.len() {
            self.data.resize(end, 0);
        }
        self.data[dst_offset..end].copy_from_slice(&src[..count]);
    }

    /// Set start code `[0, 0, 1]` at the given offset.
    pub fn set_slice_start_code_at_offset(&mut self, offset: i64) {
        let off = offset as usize;
        if off + 3 > self.data.len() {
            self.data.resize(off + 3, 0);
        }
        self.data[off] = 0;
        self.data[off + 1] = 0;
        self.data[off + 2] = 1;
    }

    /// Check whether `[0, 0, 1]` is at the given offset.
    pub fn has_slice_start_code_at_offset(&self, offset: i64) -> bool {
        let off = offset as usize;
        if off + 3 > self.data.len() {
            return false;
        }
        self.data[off] == 0 && self.data[off + 1] == 0 && self.data[off + 2] == 1
    }

    pub fn add_stream_marker(&mut self, offset: u32) {
        self.stream_markers.push(offset);
    }

    pub fn get_stream_markers_count(&self) -> usize {
        self.stream_markers.len()
    }

    pub fn reset_stream_markers(&mut self) {
        self.stream_markers.clear();
    }

    pub fn stream_markers(&self) -> &[u32] {
        &self.stream_markers
    }

    /// Resize the buffer, keeping existing data up to `copy_size`.
    pub fn resize(&mut self, new_size: usize) {
        if new_size > self.data.len() {
            self.data.resize(new_size, 0);
        }
    }

    /// Swap for a fresh buffer, optionally copying a range from the old one.
    pub fn swap_buffer(&mut self, copy_offset: usize, copy_size: usize) -> usize {
        if copy_size > 0 && copy_offset < self.data.len() {
            let end = (copy_offset + copy_size).min(self.data.len());
            let copied: Vec<u8> = self.data[copy_offset..end].to_vec();
            let capacity = self.data.len();
            self.data = vec![0u8; capacity];
            let len = copied.len();
            self.data[..len].copy_from_slice(&copied);
        } else {
            let capacity = self.data.len();
            self.data = vec![0u8; capacity];
        }
        self.data.len()
    }
}

// ---------------------------------------------------------------------------
// VulkanVideoDecoder — the base decoder state machine
// ---------------------------------------------------------------------------

/// Base video decoder state, holding all fields from the C++ class.
///
/// Codec-specific behavior is provided by a `VideoDecoderCodec` implementation
/// passed to methods that need it.
pub struct VulkanVideoDecoder {
    // Ref-counting (atomic, mirrors C++)
    ref_count: AtomicI32,

    /// Encoding standard.
    pub standard: vk::VideoCodecOperationFlagsKHR,

    // Bitfield flags
    pub h264_svc_enabled: bool,
    pub out_of_band_picture_parameters: bool,
    pub init_sequence_is_called: bool,

    /// Minimum default buffer size the parser allocates.
    pub default_min_buffer_size: u32,
    pub buffer_offset_alignment: u32,
    pub buffer_size_alignment: u32,

    /// Bitstream data for the current picture.
    pub bitstream_data: BitstreamData,
    pub bitstream_data_len: usize,

    /// Bit buffer for start code parsing.
    pub bit_bfr: u32,
    pub emul_bytes_present: bool,
    pub no_start_codes: bool,
    pub filter_timestamps: bool,
    pub max_frame_buffers: i32,

    /// Current NAL unit being filled / read.
    pub nalu: NvVkNalUnit,

    pub min_bytes_for_boundary_detection: usize,
    pub clock_rate: i64,
    pub frame_duration: i64,
    pub expected_pts: i64,
    pub parsed_bytes: i64,
    pub nalu_start_location: i64,
    pub frame_start_location: i64,
    pub error_threshold: i32,
    pub first_pts: bool,
    pub pts_pos: usize,
    pub callback_event_count: u32,

    pub prev_seq_info: VkParserSequenceInfo,
    pub ext_seq_info: VkParserSequenceInfo,

    pub disp_info: [NvVkPresentationInfo; MAX_DELAY],
    pub pts_queue: [PtsQueueEntry; MAX_QUEUED_PTS],

    pub discontinuity_reported: bool,

    /// Picture data for the current picture being decoded.
    pub picture_data: PictureData,

    pub target_layer: i32,
    pub decoder_init_failed: bool,
    pub check_pts: i32,
    pub error: NvCodecError,
}

impl VulkanVideoDecoder {
    /// Create a new decoder for the given codec standard.
    /// Mirrors `VulkanVideoDecoder::VulkanVideoDecoder(VkVideoCodecOperationFlagBitsKHR std)`.
    pub fn new(standard: vk::VideoCodecOperationFlagsKHR) -> Self {
        Self {
            ref_count: AtomicI32::new(0),
            standard,
            h264_svc_enabled: false,
            out_of_band_picture_parameters: false,
            init_sequence_is_called: false,
            default_min_buffer_size: 2 * 1024 * 1024,
            buffer_offset_alignment: 256,
            buffer_size_alignment: 256,
            bitstream_data: BitstreamData::default(),
            bitstream_data_len: 0,
            bit_bfr: 0,
            emul_bytes_present: false,
            no_start_codes: false,
            filter_timestamps: false,
            max_frame_buffers: 0,
            nalu: NvVkNalUnit::default(),
            min_bytes_for_boundary_detection: 256,
            clock_rate: 0,
            frame_duration: 0,
            expected_pts: 0,
            parsed_bytes: 0,
            nalu_start_location: 0,
            frame_start_location: 0,
            error_threshold: 0,
            first_pts: false,
            pts_pos: 0,
            callback_event_count: 0,
            prev_seq_info: VkParserSequenceInfo::default(),
            ext_seq_info: VkParserSequenceInfo::default(),
            disp_info: std::array::from_fn(|_| NvVkPresentationInfo::default()),
            pts_queue: std::array::from_fn(|_| PtsQueueEntry::default()),
            discontinuity_reported: false,
            picture_data: PictureData::default(),
            target_layer: 0,
            decoder_init_failed: false,
            check_pts: 0,
            error: NvCodecError::NoError,
        }
    }

    // -----------------------------------------------------------------------
    // Ref counting (mirrors C++ AddRef / Release)
    // -----------------------------------------------------------------------

    pub fn add_ref(&self) -> i32 {
        self.ref_count.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Decrement ref count.  Returns new count.
    /// NOTE: Unlike C++, Rust doesn't `delete this`; the caller is responsible
    /// for dropping when the count reaches zero.
    pub fn release(&self) -> i32 {
        self.ref_count.fetch_sub(1, Ordering::SeqCst) - 1
    }

    // -----------------------------------------------------------------------
    // Initialize / Deinitialize
    // -----------------------------------------------------------------------

    /// Mirrors `VulkanVideoDecoder::Initialize`.
    pub fn initialize<C: VideoDecodeClient>(
        &mut self,
        params: &InitDecodeParameters<'_, C>,
        codec: &mut impl VideoDecoderCodec,
    ) -> vk::Result {
        if params.interface_version != NV_VULKAN_VIDEO_PARSER_API_VERSION {
            return vk::Result::ERROR_INCOMPATIBLE_DRIVER;
        }

        codec.free_context();
        self.bitstream_data = BitstreamData::default();

        self.default_min_buffer_size = params.default_min_buffer_size;
        self.buffer_offset_alignment = params.buffer_offset_alignment;
        self.buffer_size_alignment = params.buffer_size_alignment;
        self.out_of_band_picture_parameters = params.out_of_band_picture_parameters;
        self.clock_rate = if params.reference_clock_rate > 0 {
            params.reference_clock_rate as i64
        } else {
            10_000_000 // 10 MHz default
        };
        self.error_threshold = params.error_threshold;
        self.discontinuity_reported = false;
        self.frame_duration = 0;
        self.expected_pts = 0;
        self.no_start_codes = false;
        self.filter_timestamps = false;
        self.check_pts = 16;
        self.emul_bytes_present = false;
        self.first_pts = true;

        if let Some(ref ext) = params.external_seq_info {
            self.ext_seq_info = ext.clone();
        } else {
            self.ext_seq_info = VkParserSequenceInfo::default();
        }

        self.bitstream_data_len = self.default_min_buffer_size as usize;
        self.bitstream_data = BitstreamData::new(self.bitstream_data_len);

        codec.create_private_context();

        self.nalu = NvVkNalUnit::default();
        self.prev_seq_info = VkParserSequenceInfo::default();
        self.disp_info = std::array::from_fn(|_| NvVkPresentationInfo::default());
        self.pts_queue = std::array::from_fn(|_| PtsQueueEntry::default());
        self.bitstream_data.reset_stream_markers();
        self.bit_bfr = !0u32;
        self.max_frame_buffers = 0;
        self.decoder_init_failed = false;
        self.parsed_bytes = 0;
        self.nalu_start_location = 0;
        self.frame_start_location = 0;
        self.pts_pos = 0;

        codec.init_parser();

        // Reset nalu again (in case parser used init_dbits during initialization)
        self.nalu = NvVkNalUnit::default();

        vk::Result::SUCCESS
    }

    /// Mirrors `VulkanVideoDecoder::Deinitialize`.
    pub fn deinitialize(&mut self, codec: &mut impl VideoDecoderCodec) {
        codec.free_context();
        self.bitstream_data = BitstreamData::default();
    }

    // -----------------------------------------------------------------------
    // Bitstream reading primitives
    // -----------------------------------------------------------------------

    /// Initialize the bit reader for the current NAL unit.
    /// Mirrors `VulkanVideoDecoder::init_dbits`.
    pub fn init_dbits(&mut self) {
        self.nalu.get_offset =
            self.nalu.start_offset + if self.no_start_codes { 0 } else { 3 };
        self.nalu.get_zerocnt = 0;
        self.nalu.get_emulcnt = 0;
        self.nalu.get_bfr = 0;
        self.nalu.get_bfroffs = 32;
        self.skip_bits(0);
    }

    /// Number of bits remaining in the current NAL unit.
    /// Mirrors `VulkanVideoDecoder::available_bits`.
    ///
    /// NOTE: `get_offset` can advance past `end_offset` because `skip_bits`
    /// primes the 32-bit buffer by reading ahead.  The `(32 - get_bfroffs)`
    /// term compensates for the over-read, so the result is correct even when
    /// `end_offset - get_offset` is negative.  We clamp to zero only when the
    /// final result would be negative (truly exhausted).
    #[inline]
    pub fn available_bits(&self) -> i32 {
        let diff = self.nalu.end_offset - self.nalu.get_offset;
        let bits = (diff as i32) * 8 + (32 - self.nalu.get_bfroffs as i32);
        if bits < 0 { 0 } else { bits }
    }

    /// Number of bits consumed so far in the current NAL unit.
    /// Mirrors `VulkanVideoDecoder::consumed_bits`.
    #[inline]
    pub fn consumed_bits(&self) -> i32 {
        let bytes =
            self.nalu.get_offset - self.nalu.start_offset - self.nalu.get_emulcnt as i64;
        (bytes as i32) * 8 - (32 - self.nalu.get_bfroffs as i32)
    }

    /// Peek at the next `n` bits without consuming them.
    /// NOTE: `n` must be in `[1..=25]`.
    /// Mirrors `VulkanVideoDecoder::next_bits`.
    #[inline]
    pub fn next_bits(&self, n: u32) -> u32 {
        (self.nalu.get_bfr << self.nalu.get_bfroffs) >> (32 - n)
    }

    /// Advance bitstream position by `n` bits.
    /// Mirrors `VulkanVideoDecoder::skip_bits`.
    pub fn skip_bits(&mut self, n: u32) {
        self.nalu.get_bfroffs += n;
        while self.nalu.get_bfroffs >= 8 {
            self.nalu.get_bfr <<= 8;
            if self.nalu.get_offset < self.nalu.end_offset {
                let mut c = self.bitstream_data.get(self.nalu.get_offset) as u32;
                self.nalu.get_offset += 1;
                if self.emul_bytes_present {
                    // Detect / discard emulation_prevention_three_byte
                    if self.nalu.get_zerocnt == 2 {
                        if c == 3 {
                            self.nalu.get_zerocnt = 0;
                            c = if self.nalu.get_offset < self.nalu.end_offset {
                                self.bitstream_data.get(self.nalu.get_offset) as u32
                            } else {
                                0
                            };
                            self.nalu.get_offset += 1;
                            self.nalu.get_emulcnt += 1;
                        }
                    }
                    if c != 0 {
                        self.nalu.get_zerocnt = 0;
                    } else if self.nalu.get_zerocnt < 2 {
                        self.nalu.get_zerocnt += 1;
                    }
                }
                self.nalu.get_bfr |= c;
            } else {
                self.nalu.get_offset += 1;
            }
            self.nalu.get_bfroffs -= 8;
        }
    }

    /// Read next `n` bits (up to 32), advancing the bitstream.
    /// Mirrors `VulkanVideoDecoder::u`.
    pub fn u(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        if n + self.nalu.get_bfroffs <= 32 {
            let bits = self.next_bits(n);
            self.skip_bits(n);
            bits
        } else {
            // n == 26..32
            let bits = self.next_bits(n - 25) << 25;
            self.skip_bits(n - 25);
            let lo = self.next_bits(25);
            self.skip_bits(25);
            bits | lo
        }
    }

    /// Read a single flag bit.
    /// Mirrors `VulkanVideoDecoder::flag`.
    #[inline]
    pub fn flag(&mut self) -> bool {
        self.u(1) != 0
    }

    /// Read a 16-bit little-endian value.
    #[inline]
    pub fn u16_le(&mut self) -> u32 {
        let lo = self.u(8);
        let hi = self.u(8);
        lo | (hi << 8)
    }

    /// Read a 24-bit little-endian value.
    #[inline]
    pub fn u24_le(&mut self) -> u32 {
        let lo = self.u16_le();
        lo | (self.u(8) << 16)
    }

    /// Read a 32-bit little-endian value.
    #[inline]
    pub fn u32_le(&mut self) -> u32 {
        let lo = self.u16_le();
        lo | (self.u16_le() << 16)
    }

    /// Unsigned Exp-Golomb coded value (Section 9.1).
    /// Mirrors `VulkanVideoDecoder::ue`.
    pub fn ue(&mut self) -> u32 {
        let mut leading_zero_bits: i32 = -1;
        let mut b: u32 = 0;
        while b == 0 && leading_zero_bits < 32 {
            leading_zero_bits += 1;
            b = self.u(1);
        }
        if leading_zero_bits < 32 {
            (1u32 << leading_zero_bits).wrapping_sub(1).wrapping_add(self.u(leading_zero_bits as u32))
        } else {
            0xFFFF_FFFFu32.wrapping_add(self.u(leading_zero_bits as u32))
        }
    }

    /// Signed Exp-Golomb coded value (Section 9.1.1, Table 9-3).
    /// Mirrors `VulkanVideoDecoder::se`.
    pub fn se(&mut self) -> i32 {
        let eg = self.ue();
        if eg & 1 != 0 {
            ((eg >> 1) + 1) as i32
        } else {
            -((eg >> 1) as i32)
        }
    }

    /// Fixed-length code (syntax shorthand).
    /// Mirrors `VulkanVideoDecoder::f`.
    #[inline]
    pub fn f(&mut self, n: u32, _expected: u32) -> u32 {
        self.u(n)
    }

    /// Returns `true` if the current bit position is byte-aligned.
    #[inline]
    pub fn byte_aligned(&self) -> bool {
        (self.nalu.get_bfroffs & 7) == 0
    }

    /// Consume bits until byte-aligned.
    pub fn byte_alignment(&mut self) {
        while !self.byte_aligned() {
            self.u(1);
        }
    }

    /// RBSP trailing bits: stop bit + alignment zero bits.
    pub fn rbsp_trailing_bits(&mut self) {
        self.f(1, 1); // rbsp_stop_one_bit
        while !self.byte_aligned() {
            self.f(1, 0); // rbsp_alignment_zero_bit
        }
    }

    /// Returns `true` if the NAL unit read pointer is past the end.
    #[inline]
    pub fn end(&self) -> bool {
        self.nalu.get_offset >= self.nalu.end_offset
    }

    /// Returns `true` if there is more RBSP data remaining.
    /// Mirrors `VulkanVideoDecoder::more_rbsp_data`.
    pub fn more_rbsp_data(&self) -> bool {
        (self.nalu.get_bfr << (self.nalu.get_bfroffs + 1)) != 0 || !self.end()
    }

    // -----------------------------------------------------------------------
    // Start code search (plain C fallback — NextStartCodeC.cpp)
    // -----------------------------------------------------------------------

    /// Scan `data` for the next `00 00 01` start code, updating `self.bit_bfr`.
    /// Returns the number of bytes consumed.
    ///
    /// Mirrors `VulkanVideoDecoder::next_start_code<SIMD_ISA::NOSIMD>`.
    pub fn next_start_code(&mut self, data: &[u8]) -> (usize, bool) {
        let mut bfr = self.bit_bfr;
        let mut i = 0usize;
        loop {
            bfr = (bfr << 8) | data[i] as u32;
            i += 1;
            if (bfr & 0x00FF_FFFF) == 1 {
                break;
            }
            if i >= data.len() {
                break;
            }
        }
        self.bit_bfr = bfr;
        let found = (bfr & 0x00FF_FFFF) == 1;
        (i, found)
    }

    // -----------------------------------------------------------------------
    // Bitstream buffer management
    // -----------------------------------------------------------------------

    /// Resize the bitstream buffer, adding at least `extra_bytes`.
    /// Mirrors `VulkanVideoDecoder::resizeBitstreamBuffer`.
    pub fn resize_bitstream_buffer(&mut self, extra_bytes: usize) -> bool {
        let min_extra = 2 * 1024 * 1024;
        let new_len = self.bitstream_data_len + extra_bytes.max(min_extra);
        self.bitstream_data.resize(new_len);
        self.bitstream_data_len = self.bitstream_data.len();
        true
    }

    /// Swap the current bitstream buffer for a fresh one, optionally copying a
    /// region.  Returns the new buffer capacity.
    /// Mirrors `VulkanVideoDecoder::swapBitstreamBuffer`.
    pub fn swap_bitstream_buffer(
        &mut self,
        copy_offset: usize,
        copy_size: usize,
    ) -> usize {
        self.bitstream_data.swap_buffer(copy_offset, copy_size);
        self.bitstream_data.len()
    }

    // -----------------------------------------------------------------------
    // Sequence change detection
    // -----------------------------------------------------------------------

    /// Mirrors `VulkanVideoDecoder::IsSequenceChange`.
    pub fn is_sequence_change(&self, seq_info: &VkParserSequenceInfo) -> bool {
        *seq_info != self.prev_seq_info
    }

    /// Must be called by derived codecs to initialize a sequence.
    /// Mirrors `VulkanVideoDecoder::init_sequence`.
    pub fn init_sequence(
        &mut self,
        seq_info: &VkParserSequenceInfo,
        client: &mut impl VideoDecodeClient,
    ) -> i32 {
        if *seq_info != self.prev_seq_info {
            self.prev_seq_info = seq_info.clone();
            self.max_frame_buffers = client.begin_sequence(&self.prev_seq_info);
            if self.max_frame_buffers == 0 {
                self.decoder_init_failed = true;
                return 0;
            }
            let numerator = frame_rate_num(seq_info.frame_rate);
            let denominator = frame_rate_den(seq_info.frame_rate);
            if self.clock_rate > 0 && numerator > 0 && denominator > 0 {
                self.frame_duration =
                    (denominator as u64 * self.clock_rate as u64 / numerator as u64) as i64;
            } else if self.frame_duration <= 0 {
                tracing::warn!("Unknown frame rate");
                self.frame_duration = self.clock_rate / 30;
            }
        }
        self.max_frame_buffers
    }

    // -----------------------------------------------------------------------
    // NAL unit processing
    // -----------------------------------------------------------------------

    /// Process the current NAL unit.
    /// Mirrors `VulkanVideoDecoder::nal_unit`.
    pub fn nal_unit(
        &mut self,
        codec: &mut impl VideoDecoderCodec,
        client: &mut impl VideoDecodeClient,
    ) {
        if (self.nalu.end_offset - self.nalu.start_offset) > 3
            && self.bitstream_data.has_slice_start_code_at_offset(self.nalu.start_offset)
        {
            self.init_dbits();
            let rbsp_size = self.available_bits() >> 3;
            if codec.is_picture_boundary(self, rbsp_size) {
                if self.nalu.start_offset > 0 {
                    self.end_of_picture(codec, client);

                    let copy_offset = self.nalu.start_offset as usize;
                    let copy_size = (self.nalu.end_offset - self.nalu.start_offset) as usize;
                    self.bitstream_data_len =
                        self.swap_bitstream_buffer(copy_offset, copy_size);
                    self.nalu.end_offset -= self.nalu.start_offset;
                    self.nalu.start_offset = 0;
                    self.bitstream_data.reset_stream_markers();
                    self.nalu_start_location =
                        self.parsed_bytes - self.nalu.end_offset;
                }
            }

            self.init_dbits();
            let nal_type = codec.parse_nal_unit(self);

            match nal_type {
                NalUnitType::Slice => {
                    if self.bitstream_data.get_stream_markers_count() < MAX_SLICES {
                        if self.bitstream_data.get_stream_markers_count() == 0 {
                            self.frame_start_location = self.nalu_start_location;
                        }
                        self.bitstream_data
                            .add_stream_marker(self.nalu.start_offset as u32);
                    }
                }
                NalUnitType::Unknown => {
                    let start = (self.nalu.start_offset + 3) as usize;
                    let end = self.nalu.end_offset as usize;
                    if end > start {
                        let data = self.bitstream_data.data_ptr()[start..end].to_vec();
                        client.unhandled_nalu(&data);
                    }
                    self.nalu.end_offset = self.nalu.start_offset;
                }
                NalUnitType::Discard => {
                    self.nalu.end_offset = self.nalu.start_offset;
                }
            }
        } else {
            // Discard invalid NALU
            self.nalu.end_offset = self.nalu.start_offset;
        }
        self.nalu.start_offset = self.nalu.end_offset;
    }

    // -----------------------------------------------------------------------
    // End of picture
    // -----------------------------------------------------------------------

    /// Mirrors `VulkanVideoDecoder::end_of_picture`.
    pub fn end_of_picture(
        &mut self,
        codec: &mut impl VideoDecoderCodec,
        client: &mut impl VideoDecodeClient,
    ) {
        if self.nalu.end_offset <= 3
            || self.bitstream_data.get_stream_markers_count() == 0
        {
            return;
        }

        // Build picture data
        self.picture_data = PictureData::default();
        self.picture_data.bitstream_data_offset = 0;
        self.picture_data.first_slice_index = 0;
        self.picture_data.bitstream_data = self.bitstream_data.data_ptr()
            [..self.nalu.start_offset as usize]
            .to_vec();
        self.picture_data.bitstream_data_len = self.nalu.start_offset as usize;
        self.picture_data.num_slices =
            self.bitstream_data.get_stream_markers_count() as u32;
        self.picture_data.slice_offsets =
            self.bitstream_data.stream_markers().to_vec();

        if codec.begin_picture(self, &mut self.picture_data.clone()) {
            // Re-read the picture_data that begin_picture filled
            let target = self.target_layer as usize;
            let _ = target; // target_layer indexing only used in SVC

            if let Some(curr_pic) = self.picture_data.curr_pic {
                // Find a slot in disp_info
                let mut disp = 0usize;
                for i in 0..MAX_DELAY {
                    if self.disp_info[i].pic_buf == Some(curr_pic) {
                        disp = i;
                        break;
                    }
                    if self.disp_info[i].pic_buf.is_none()
                        || (self.disp_info[disp].pic_buf.is_some()
                            && self.disp_info[i].pts.wrapping_sub(self.disp_info[disp].pts)
                                < 0)
                    {
                        disp = i;
                    }
                }

                self.disp_info[disp].pic_buf = Some(curr_pic);
                self.disp_info[disp].skipped = false;
                self.disp_info[disp].discontinuity = false;
                self.disp_info[disp].poc = self.picture_data.picture_order_count;

                if self.picture_data.field_pic_flag && !self.picture_data.second_field {
                    self.disp_info[disp].num_fields = 1;
                } else {
                    self.disp_info[disp].num_fields =
                        2 + self.picture_data.repeat_first_field as i32;
                }

                if !self.picture_data.second_field || !self.disp_info[disp].pts_valid {
                    // Find a PTS in the list
                    let mut ndx = self.pts_pos;
                    self.disp_info[disp].pts_valid = false;
                    self.disp_info[disp].pts = self.expected_pts;

                    for _ in 0..MAX_QUEUED_PTS {
                        if self.pts_queue[ndx].pts_valid
                            && self.pts_queue[ndx]
                                .pts_pos
                                .wrapping_sub(self.frame_start_location)
                                <= if self.no_start_codes { 0 } else { 3 }
                        {
                            self.disp_info[disp].pts_valid = true;
                            self.disp_info[disp].pts = self.pts_queue[ndx].pts;
                            self.disp_info[disp].discontinuity =
                                self.pts_queue[ndx].discontinuity;
                            self.pts_queue[ndx].pts_valid = false;
                        }
                        ndx = (ndx + 1) % MAX_QUEUED_PTS;
                    }
                }

                // Client callback — decode
                if !client.decode_picture(&self.picture_data) {
                    self.disp_info[disp].skipped = true;
                    tracing::warn!("skipped decoding current picture");
                } else {
                    self.callback_event_count += 1;
                }
            } else {
                tracing::warn!("no valid render target for current picture");
            }

            codec.end_picture();
        }
    }

    // -----------------------------------------------------------------------
    // Display picture
    // -----------------------------------------------------------------------

    /// Mirrors `VulkanVideoDecoder::display_picture`.
    pub fn display_picture(
        &mut self,
        pic_buf: usize,
        evict: bool,
        client: &mut impl VideoDecodeClient,
    ) {
        let mut disp: Option<usize> = None;
        for i in 0..MAX_DELAY {
            if self.disp_info[i].pic_buf == Some(pic_buf) {
                disp = Some(i);
                break;
            }
        }

        let disp = match disp {
            Some(d) => d,
            None => {
                tracing::warn!(
                    "Attempting to display a picture that was not decoded ({})",
                    pic_buf
                );
                return;
            }
        };

        let pts = if self.disp_info[disp].pts_valid {
            let mut pts = self.disp_info[disp].pts;

            if self.filter_timestamps
                || (self.check_pts > 0 && !self.disp_info[disp].discontinuity)
            {
                let mut earliest = disp;
                for i in 0..MAX_DELAY {
                    if self.disp_info[i].pts_valid
                        && self.disp_info[i].pic_buf.is_some()
                        && (self.disp_info[i].pts - self.disp_info[earliest].pts) < 0
                    {
                        earliest = i;
                    }
                }
                if earliest != disp {
                    if self.check_pts > 0 {
                        self.filter_timestamps = true;
                    }
                    tracing::warn!("Input timestamps do not match display order");
                    pts = self.disp_info[earliest].pts;
                    self.disp_info[earliest].pts = self.disp_info[disp].pts;
                    self.disp_info[disp].pts = pts;
                }
                if self.check_pts > 0 {
                    self.check_pts -= 1;
                }
            }
            pts
        } else {
            let mut pts = self.expected_pts;
            if self.first_pts {
                for i in 0..MAX_DELAY {
                    if self.disp_info[i].pic_buf.is_some() && self.disp_info[i].pts_valid {
                        let mut poc_diff =
                            self.disp_info[i].poc - self.disp_info[disp].poc;
                        if poc_diff < self.disp_info[disp].num_fields {
                            poc_diff = self.disp_info[disp].num_fields;
                        }
                        pts = self.disp_info[i].pts
                            - ((poc_diff as i64 * self.frame_duration) >> 1);
                        break;
                    }
                }
            }
            pts
        };

        if !self.disp_info[disp].skipped {
            client.display_picture(pic_buf, pts);
            self.callback_event_count += 1;
        }

        if evict {
            self.disp_info[disp].pic_buf = None;
        }

        self.expected_pts = pts
            + ((self.frame_duration as u32 as u64 * self.disp_info[disp].num_fields as u32 as u64)
                >> 1) as i64;
        self.first_pts = false;
    }

    // -----------------------------------------------------------------------
    // End of stream
    // -----------------------------------------------------------------------

    /// Mirrors `VulkanVideoDecoder::end_of_stream`.
    pub fn end_of_stream(&mut self, codec: &mut impl VideoDecoderCodec) {
        codec.end_of_stream();
        self.nalu = NvVkNalUnit::default();
        self.prev_seq_info = VkParserSequenceInfo::default();
        self.pts_queue = std::array::from_fn(|_| PtsQueueEntry::default());
        self.bitstream_data.reset_stream_markers();
        self.bit_bfr = !0u32;
        self.parsed_bytes = 0;
        self.nalu_start_location = 0;
        self.frame_start_location = 0;
        self.frame_duration = 0;
        self.expected_pts = 0;
        self.first_pts = true;
        self.pts_pos = 0;
        for i in 0..MAX_DELAY {
            self.disp_info[i].pic_buf = None;
            self.disp_info[i].pts_valid = false;
        }
    }

    // -----------------------------------------------------------------------
    // ParseByteStream — the main entry point (C fallback path only)
    // -----------------------------------------------------------------------

    /// Parse a bitstream packet.  This is the main entry point corresponding to
    /// `VulkanVideoDecoder::ParseByteStreamSimd<NOSIMD>` (the C fallback).
    ///
    /// Returns `(success, parsed_bytes)`.
    #[allow(unused_assignments)]
    pub fn parse_byte_stream(
        &mut self,
        pck: &BitstreamPacket,
        codec: &mut impl VideoDecoderCodec,
        client: &mut impl VideoDecodeClient,
    ) -> (bool, usize) {
        let total_len = pck.byte_stream.len();
        let mut curr_data = pck.byte_stream;
        let mut frames_in_pkt: u32 = 0;

        if !self.bitstream_data.is_valid() {
            return (false, 0);
        }

        self.error = NvCodecError::NoError;
        self.callback_event_count = 0;

        // Handle discontinuity
        if pck.discontinuity {
            if !self.no_start_codes {
                if self.nalu.start_offset == 0 {
                    self.nalu_start_location =
                        self.parsed_bytes - self.nalu.end_offset;
                }

                // Pad data after NAL unit with start_code_prefix
                let needed = (self.nalu.end_offset + 3) as usize;
                if needed > self.bitstream_data_len {
                    if !self.resize_bitstream_buffer(needed - self.bitstream_data_len) {
                        return (false, 0);
                    }
                }
                self.bitstream_data
                    .set_slice_start_code_at_offset(self.nalu.end_offset);

                // Complete the current NAL unit (if not empty)
                self.nal_unit(codec, client);
                // Decode the current picture (may be truncated)
                self.end_of_picture(codec, client);
                frames_in_pkt += 1;

                let start_off = self.nalu.start_offset as usize;
                let size = (self.nalu.end_offset - self.nalu.start_offset) as usize;
                self.bitstream_data_len = self.swap_bitstream_buffer(start_off, size);
            }
            // Reset PTS queue
            self.pts_queue = std::array::from_fn(|_| PtsQueueEntry::default());
            self.discontinuity_reported = true;
        }

        // Remember the packet PTS
        if pck.pts_valid {
            self.pts_queue[self.pts_pos].pts_valid = true;
            self.pts_queue[self.pts_pos].pts = pck.pts;
            self.pts_queue[self.pts_pos].pts_pos = self.parsed_bytes;
            self.pts_queue[self.pts_pos].discontinuity = self.discontinuity_reported;
            self.discontinuity_reported = false;
            self.pts_pos = (self.pts_pos + 1) % MAX_QUEUED_PTS;
        }

        // No start codes: input always contains a single frame
        if self.no_start_codes {
            let data_size = curr_data.len();
            if data_size > self.bitstream_data_len - 4 {
                if !self.resize_bitstream_buffer(data_size - (self.bitstream_data_len - 4)) {
                    return (false, 0);
                }
            }
            if data_size > 0 {
                self.nalu.start_offset = 0;
                self.nalu.end_offset = data_size as i64;
                self.bitstream_data.copy_from_slice(curr_data, 0, data_size);
                self.nalu_start_location = self.parsed_bytes;
                self.parsed_bytes += data_size as i64;
                self.bitstream_data.reset_stream_markers();
                self.init_dbits();
                if codec.parse_nal_unit(self) == NalUnitType::Slice {
                    self.frame_start_location = self.nalu_start_location;
                    self.bitstream_data.add_stream_marker(0);
                    self.nalu.start_offset = self.nalu.end_offset;
                    if !pck.eop || (pck.eop && frames_in_pkt < 1) {
                        self.end_of_picture(codec, client);
                        frames_in_pkt += 1;
                        let start_off = self.nalu.start_offset as usize;
                        let size =
                            (self.nalu.end_offset - self.nalu.start_offset) as usize;
                        self.bitstream_data_len =
                            self.swap_bitstream_buffer(start_off, size);
                    }
                }
            }
            self.nalu.start_offset = 0;
            self.nalu.end_offset = 0;
            if pck.eos {
                self.end_of_stream(codec);
            }
            return (self.error == NvCodecError::NoError, total_len);
        }

        // Parse start codes
        while !curr_data.is_empty() {
            // If partial parsing, return once we decoded/displayed a frame
            if pck.partial_parsing && self.callback_event_count != 0 {
                break;
            }

            let mut buflen = curr_data.len();
            if self.nalu.start_offset > 0
                && (self.nalu.end_offset - self.nalu.start_offset)
                    < self.min_bytes_for_boundary_detection as i64
            {
                let remaining = self.min_bytes_for_boundary_detection
                    - (self.nalu.end_offset - self.nalu.start_offset) as usize;
                buflen = buflen.min(remaining);
            }

            let (start_offset, found_start_code) =
                self.next_start_code(&curr_data[..buflen]);
            let data_used = if found_start_code {
                start_offset
            } else {
                buflen
            };

            if data_used > 0 {
                let space = self.bitstream_data_len - self.nalu.end_offset as usize;
                if data_used > space {
                    self.resize_bitstream_buffer(data_used - space);
                }
                let bytes = data_used
                    .min(self.bitstream_data_len - self.nalu.end_offset as usize);
                if bytes > 0 {
                    self.bitstream_data.copy_from_slice(
                        curr_data,
                        self.nalu.end_offset as usize,
                        bytes,
                    );
                }
                self.nalu.end_offset += bytes as i64;
                self.parsed_bytes += bytes as i64;
                curr_data = &curr_data[data_used..];

                // Check for picture boundaries early
                if self.nalu.start_offset > 0
                    && self.nalu.end_offset
                        == self.nalu.start_offset
                            + self.min_bytes_for_boundary_detection as i64
                {
                    self.init_dbits();
                    let rbsp_size = self.available_bits() >> 3;
                    if codec.is_picture_boundary(self, rbsp_size) {
                        if !pck.eop || (pck.eop && frames_in_pkt < 1) {
                            self.end_of_picture(codec, client);
                            frames_in_pkt += 1;
                        }
                        let copy_off = self.nalu.start_offset as usize;
                        let copy_size =
                            (self.nalu.end_offset - self.nalu.start_offset) as usize;
                        self.bitstream_data_len =
                            self.swap_bitstream_buffer(copy_off, copy_size);
                        self.nalu.end_offset -= self.nalu.start_offset;
                        self.nalu.start_offset = 0;
                        self.bitstream_data.reset_stream_markers();
                        self.nalu_start_location =
                            self.parsed_bytes - self.nalu.end_offset;
                    }
                }
            }

            // Did we find a start code?
            if found_start_code {
                if self.nalu.start_offset == 0 {
                    self.nalu_start_location =
                        self.parsed_bytes - self.nalu.end_offset;
                }
                // Remove trailing 00.00.01
                self.nalu.end_offset = if self.nalu.end_offset >= 3 {
                    self.nalu.end_offset - 3
                } else {
                    0
                };
                self.nal_unit(codec, client);
                if self.decoder_init_failed {
                    return (false, total_len - curr_data.len());
                }
                // Add start code prefix for next NAL unit
                self.bitstream_data
                    .set_slice_start_code_at_offset(self.nalu.end_offset);
                self.nalu.end_offset += 3;
            }
        }

        let parsed_bytes = total_len - curr_data.len();

        if pck.eop || pck.eos {
            if self.nalu.start_offset == 0 {
                self.nalu_start_location =
                    self.parsed_bytes - self.nalu.end_offset;
            }
            // Remove trailing 00.00.01
            if self.bitstream_data.is_valid()
                && self.nalu.end_offset >= 3
                && self
                    .bitstream_data
                    .has_slice_start_code_at_offset(self.nalu.end_offset - 3)
            {
                self.nalu.end_offset -= 3;
            }
            // Complete the current NAL unit
            self.nal_unit(codec, client);

            // Pad with start_code_prefix
            let needed = (self.nalu.end_offset + 3) as usize;
            if needed > self.bitstream_data_len {
                if !self.resize_bitstream_buffer(needed - self.bitstream_data_len) {
                    return (false, parsed_bytes);
                }
            }
            self.bitstream_data
                .set_slice_start_code_at_offset(self.nalu.end_offset);
            self.nalu.end_offset += 3;

            // Decode the current picture
            if !pck.eop || (pck.eop && frames_in_pkt < 1) {
                self.end_of_picture(codec, client);
                self.bitstream_data_len = self.swap_bitstream_buffer(0, 0);
            }
            self.nalu.end_offset = 0;
            self.nalu.start_offset = 0;
            self.bitstream_data.reset_stream_markers();
            self.nalu_start_location = self.parsed_bytes;
            if pck.eos {
                self.end_of_stream(codec);
            }
        }

        (self.error == NvCodecError::NoError, parsed_bytes)
    }

    /// Get display mastering info (delegates to codec).
    pub fn get_display_mastering_info(
        &self,
        info: &mut VkParserDisplayMasteringInfo,
        codec: &impl VideoDecoderCodec,
    ) -> bool {
        codec.get_display_mastering_info(info)
    }
}

// ---------------------------------------------------------------------------
// Free-standing Exp-Golomb helpers (for unit testing without a full decoder)
// ---------------------------------------------------------------------------

/// Decode an unsigned Exp-Golomb code from a byte slice starting at the given
/// bit offset.  Returns `(value, new_bit_offset)`.
pub fn exp_golomb_ue(data: &[u8], bit_offset: usize) -> (u32, usize) {
    let mut pos = bit_offset;

    // Count leading zero bits
    let mut leading_zeros: u32 = 0;
    loop {
        if pos / 8 >= data.len() {
            return (0, pos);
        }
        let byte = data[pos / 8];
        let bit = (byte >> (7 - (pos % 8))) & 1;
        pos += 1;
        if bit != 0 {
            break;
        }
        leading_zeros += 1;
        if leading_zeros >= 32 {
            break;
        }
    }

    if leading_zeros >= 32 {
        return (u32::MAX, pos);
    }

    // Read `leading_zeros` more bits
    let mut suffix: u32 = 0;
    for _ in 0..leading_zeros {
        if pos / 8 >= data.len() {
            break;
        }
        let byte = data[pos / 8];
        let bit = (byte >> (7 - (pos % 8))) & 1;
        suffix = (suffix << 1) | bit as u32;
        pos += 1;
    }

    ((1u32 << leading_zeros) - 1 + suffix, pos)
}

/// Decode a signed Exp-Golomb code from a byte slice starting at the given
/// bit offset.  Returns `(value, new_bit_offset)`.
pub fn exp_golomb_se(data: &[u8], bit_offset: usize) -> (i32, usize) {
    let (eg, new_offset) = exp_golomb_ue(data, bit_offset);
    let val = if eg & 1 != 0 {
        ((eg >> 1) + 1) as i32
    } else {
        -((eg >> 1) as i32)
    };
    (val, new_offset)
}

// ---------------------------------------------------------------------------
// Logging helpers (mirrors nvParserLog / nvParserErrorLog / nvParserVerboseLog)
// ---------------------------------------------------------------------------

/// General parser log.
#[macro_export]
macro_rules! nv_parser_log {
    ($($arg:tt)*) => {
        tracing::debug!($($arg)*);
    };
}

/// Verbose parser log.
#[macro_export]
macro_rules! nv_parser_verbose_log {
    ($($arg:tt)*) => {
        tracing::debug!($($arg)*);
    };
}

/// Error parser log.
#[macro_export]
macro_rules! nv_parser_error_log {
    ($($arg:tt)*) => {
        tracing::error!($($arg)*);
    };
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Frame-rate helpers
    // -----------------------------------------------------------------------

    #[test]
    fn test_make_frame_rate_roundtrip() {
        let fr = make_frame_rate(30000, 1001);
        assert_eq!(frame_rate_num(fr), 30000);
        assert_eq!(frame_rate_den(fr), 1001);
    }

    #[test]
    fn test_pack_frame_rate_small() {
        let fr = pack_frame_rate(24000, 1001);
        assert_eq!(frame_rate_num(fr), 24000);
        assert_eq!(frame_rate_den(fr), 1001);
    }

    #[test]
    fn test_pack_frame_rate_large_needs_reduction() {
        // Numerator too large for 18 bits — should be reduced
        let fr = pack_frame_rate(500_000, 1000);
        let num = frame_rate_num(fr);
        let den = frame_rate_den(fr);
        // The ratio should be preserved (approximately)
        assert!(num < (1 << 18));
        assert!(den < (1 << 14));
        // 500_000 / 1000 = 500, so num/den should ~ 500
        let ratio = num as f64 / den as f64;
        assert!((ratio - 500.0).abs() < 1.0);
    }

    // -----------------------------------------------------------------------
    // Exp-Golomb free functions
    // -----------------------------------------------------------------------

    #[test]
    fn test_exp_golomb_ue_zero() {
        // ue(0) = 1 (single '1' bit)
        let data = [0b1000_0000];
        let (val, pos) = exp_golomb_ue(&data, 0);
        assert_eq!(val, 0);
        assert_eq!(pos, 1);
    }

    #[test]
    fn test_exp_golomb_ue_one() {
        // ue(1) = 010 (one leading zero, then 1, then 0)
        let data = [0b0100_0000];
        let (val, pos) = exp_golomb_ue(&data, 0);
        assert_eq!(val, 1);
        assert_eq!(pos, 3);
    }

    #[test]
    fn test_exp_golomb_ue_two() {
        // ue(2) = 011
        let data = [0b0110_0000];
        let (val, pos) = exp_golomb_ue(&data, 0);
        assert_eq!(val, 2);
        assert_eq!(pos, 3);
    }

    #[test]
    fn test_exp_golomb_ue_three() {
        // ue(3) = 00100
        let data = [0b0010_0000];
        let (val, pos) = exp_golomb_ue(&data, 0);
        assert_eq!(val, 3);
        assert_eq!(pos, 5);
    }

    #[test]
    fn test_exp_golomb_ue_four() {
        // ue(4) = 00101
        let data = [0b0010_1000];
        let (val, pos) = exp_golomb_ue(&data, 0);
        assert_eq!(val, 4);
        assert_eq!(pos, 5);
    }

    #[test]
    fn test_exp_golomb_ue_seven() {
        // ue(7) = 0001000
        let data = [0b0001_0000];
        let (val, pos) = exp_golomb_ue(&data, 0);
        assert_eq!(val, 7);
        assert_eq!(pos, 7);
    }

    #[test]
    fn test_exp_golomb_ue_with_offset() {
        // 3 padding bits, then ue(0) = 1
        // Bits: xxx1....
        let data = [0b0001_0000];
        let (val, pos) = exp_golomb_ue(&data, 3);
        assert_eq!(val, 0);
        assert_eq!(pos, 4);
    }

    #[test]
    fn test_exp_golomb_se_zero() {
        // se(0) maps from ue(0) = 0
        let data = [0b1000_0000];
        let (val, pos) = exp_golomb_se(&data, 0);
        assert_eq!(val, 0);
        assert_eq!(pos, 1);
    }

    #[test]
    fn test_exp_golomb_se_positive() {
        // se(1) maps from ue(1) = 010. eg=1, odd -> +1
        let data = [0b0100_0000];
        let (val, pos) = exp_golomb_se(&data, 0);
        assert_eq!(val, 1);
        assert_eq!(pos, 3);
    }

    #[test]
    fn test_exp_golomb_se_negative() {
        // se(-1) maps from ue(2) = 011. eg=2, even -> -(2/2) = -1
        let data = [0b0110_0000];
        let (val, pos) = exp_golomb_se(&data, 0);
        assert_eq!(val, -1);
        assert_eq!(pos, 3);
    }

    #[test]
    fn test_exp_golomb_se_positive_two() {
        // se(2) maps from ue(3) = 00100. eg=3, odd -> +2
        let data = [0b0010_0000];
        let (val, pos) = exp_golomb_se(&data, 0);
        assert_eq!(val, 2);
        assert_eq!(pos, 5);
    }

    #[test]
    fn test_exp_golomb_se_negative_two() {
        // se(-2) maps from ue(4) = 00101. eg=4, even -> -2
        let data = [0b0010_1000];
        let (val, pos) = exp_golomb_se(&data, 0);
        assert_eq!(val, -2);
        assert_eq!(pos, 5);
    }

    // -----------------------------------------------------------------------
    // Start code search
    // -----------------------------------------------------------------------

    #[test]
    fn test_next_start_code_found() {
        let mut dec = VulkanVideoDecoder::new(vk::VideoCodecOperationFlagsKHR::DECODE_H264);
        dec.bit_bfr = !0u32;
        let data = [0x00, 0x00, 0x00, 0x01, 0x65];
        let (consumed, found) = dec.next_start_code(&data);
        assert!(found);
        assert_eq!(consumed, 4); // consumed through the 0x01
    }

    #[test]
    fn test_next_start_code_not_found() {
        let mut dec = VulkanVideoDecoder::new(vk::VideoCodecOperationFlagsKHR::DECODE_H264);
        dec.bit_bfr = !0u32;
        let data = [0xAA, 0xBB, 0xCC, 0xDD];
        let (consumed, found) = dec.next_start_code(&data);
        assert!(!found);
        assert_eq!(consumed, 4);
    }

    #[test]
    fn test_next_start_code_immediate() {
        let mut dec = VulkanVideoDecoder::new(vk::VideoCodecOperationFlagsKHR::DECODE_H264);
        // Pre-load bit_bfr with 0x0000__XX so that after one byte we get 00 00 01
        dec.bit_bfr = 0x00000000;
        let data = [0x01, 0x65, 0x88];
        let (consumed, found) = dec.next_start_code(&data);
        assert!(found);
        assert_eq!(consumed, 1);
    }

    // -----------------------------------------------------------------------
    // NvVkNalUnit default
    // -----------------------------------------------------------------------

    #[test]
    fn test_nal_unit_default() {
        let nalu = NvVkNalUnit::default();
        assert_eq!(nalu.start_offset, 0);
        assert_eq!(nalu.end_offset, 0);
        assert_eq!(nalu.get_offset, 0);
        assert_eq!(nalu.get_zerocnt, 0);
        assert_eq!(nalu.get_bfr, 0);
        assert_eq!(nalu.get_bfroffs, 0);
        assert_eq!(nalu.get_emulcnt, 0);
    }

    // -----------------------------------------------------------------------
    // Bitstream reading (u, ue, se via the decoder)
    // -----------------------------------------------------------------------

    /// Helper: create a decoder with bitstream data loaded and init_dbits called.
    fn decoder_with_bitstream(data: &[u8]) -> VulkanVideoDecoder {
        let mut dec = VulkanVideoDecoder::new(vk::VideoCodecOperationFlagsKHR::DECODE_H264);
        // Prepend start code so init_dbits skips 3 bytes
        let mut buf = vec![0x00, 0x00, 0x01];
        buf.extend_from_slice(data);
        let len = buf.len();
        dec.bitstream_data = BitstreamData::new(len);
        dec.bitstream_data.copy_from_slice(&buf, 0, len);
        dec.bitstream_data_len = len;
        dec.nalu.start_offset = 0;
        dec.nalu.end_offset = len as i64;
        dec.emul_bytes_present = false;
        dec.no_start_codes = false;
        dec.init_dbits();
        dec
    }

    #[test]
    fn test_u_read_8_bits() {
        let mut dec = decoder_with_bitstream(&[0xAB]);
        let val = dec.u(8);
        assert_eq!(val, 0xAB);
    }

    #[test]
    fn test_u_read_1_bit() {
        let mut dec = decoder_with_bitstream(&[0x80]); // bit 7 is set
        assert_eq!(dec.u(1), 1);
        assert_eq!(dec.u(1), 0);
    }

    #[test]
    fn test_flag() {
        let mut dec = decoder_with_bitstream(&[0xC0]); // bits: 1 1 0 0 ...
        assert!(dec.flag());
        assert!(dec.flag());
        assert!(!dec.flag());
    }

    #[test]
    fn test_ue_via_decoder() {
        // ue(0) = single '1' bit = 0x80 after start code
        let mut dec = decoder_with_bitstream(&[0x80]);
        assert_eq!(dec.ue(), 0);
    }

    #[test]
    fn test_se_via_decoder() {
        // se(1) from ue(1) = 010 = 0x40 after start code
        let mut dec = decoder_with_bitstream(&[0x40]);
        assert_eq!(dec.se(), 1);
    }

    #[test]
    fn test_available_bits() {
        let mut dec = decoder_with_bitstream(&[0xAB, 0xCD]);
        // 2 data bytes = 16 bits
        assert_eq!(dec.available_bits(), 16);
        dec.u(4);
        assert_eq!(dec.available_bits(), 12);
    }

    #[test]
    fn test_byte_aligned() {
        let mut dec = decoder_with_bitstream(&[0xFF]);
        assert!(dec.byte_aligned());
        dec.u(1);
        assert!(!dec.byte_aligned());
        dec.byte_alignment();
        assert!(dec.byte_aligned());
    }

    #[test]
    fn test_more_rbsp_data_with_data() {
        let dec = decoder_with_bitstream(&[0xFF, 0x80]);
        assert!(dec.more_rbsp_data());
    }

    #[test]
    fn test_end_at_nalu_boundary() {
        let mut dec = decoder_with_bitstream(&[0xAB]);
        // Read all 8 bits
        dec.u(8);
        assert!(dec.end());
    }

    // -----------------------------------------------------------------------
    // Emulation prevention byte handling
    // -----------------------------------------------------------------------

    #[test]
    fn test_emulation_prevention_bytes() {
        // In H.264/H.265, 00 00 03 XX sequences have the 03 removed.
        // Input after start code: 00 00 03 01 -> should read as 00 00 01
        let mut dec = VulkanVideoDecoder::new(vk::VideoCodecOperationFlagsKHR::DECODE_H264);
        let buf = vec![0x00, 0x00, 0x01, 0x00, 0x00, 0x03, 0x01];
        let len = buf.len();
        dec.bitstream_data = BitstreamData::new(len);
        dec.bitstream_data.copy_from_slice(&buf, 0, len);
        dec.bitstream_data_len = len;
        dec.nalu.start_offset = 0;
        dec.nalu.end_offset = len as i64;
        dec.emul_bytes_present = true;
        dec.no_start_codes = false;
        dec.init_dbits();

        assert_eq!(dec.u(8), 0x00);
        assert_eq!(dec.u(8), 0x00);
        // The 0x03 should be consumed as an emulation prevention byte
        assert_eq!(dec.u(8), 0x01);
        assert_eq!(dec.nalu.get_emulcnt, 1);
    }

    // -----------------------------------------------------------------------
    // BitstreamData
    // -----------------------------------------------------------------------

    #[test]
    fn test_bitstream_data_start_code() {
        let mut bs = BitstreamData::new(16);
        bs.set_slice_start_code_at_offset(4);
        assert!(bs.has_slice_start_code_at_offset(4));
        assert!(!bs.has_slice_start_code_at_offset(0));
    }

    #[test]
    fn test_bitstream_data_stream_markers() {
        let mut bs = BitstreamData::new(16);
        assert_eq!(bs.get_stream_markers_count(), 0);
        bs.add_stream_marker(10);
        bs.add_stream_marker(20);
        assert_eq!(bs.get_stream_markers_count(), 2);
        assert_eq!(bs.stream_markers(), &[10, 20]);
        bs.reset_stream_markers();
        assert_eq!(bs.get_stream_markers_count(), 0);
    }

    #[test]
    fn test_bitstream_data_swap_buffer() {
        let mut bs = BitstreamData::new(32);
        bs.copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD], 4, 4);
        let new_len = bs.swap_buffer(4, 4);
        assert_eq!(new_len, 32);
        // First 4 bytes should be what was at offset 4
        assert_eq!(bs.get(0), 0xAA);
        assert_eq!(bs.get(1), 0xBB);
        assert_eq!(bs.get(2), 0xCC);
        assert_eq!(bs.get(3), 0xDD);
    }

    // -----------------------------------------------------------------------
    // u16_le, u24_le, u32_le
    // -----------------------------------------------------------------------

    #[test]
    fn test_u16_le() {
        // Little-endian: first byte is low, second is high
        let mut dec = decoder_with_bitstream(&[0x34, 0x12]);
        assert_eq!(dec.u16_le(), 0x1234);
    }

    #[test]
    fn test_u32_le() {
        let mut dec = decoder_with_bitstream(&[0x78, 0x56, 0x34, 0x12]);
        assert_eq!(dec.u32_le(), 0x12345678);
    }

    // -----------------------------------------------------------------------
    // VkParserSequenceInfo equality (used for sequence change detection)
    // -----------------------------------------------------------------------

    #[test]
    fn test_sequence_info_equality() {
        let a = VkParserSequenceInfo::default();
        let b = VkParserSequenceInfo::default();
        assert_eq!(a, b);

        let mut c = VkParserSequenceInfo::default();
        c.coded_width = 1920;
        assert_ne!(a, c);
    }

    #[test]
    fn test_is_sequence_change() {
        let dec = VulkanVideoDecoder::new(vk::VideoCodecOperationFlagsKHR::DECODE_H264);
        let info = VkParserSequenceInfo::default();
        // prev_seq_info is default, so no change
        assert!(!dec.is_sequence_change(&info));

        let mut changed = VkParserSequenceInfo::default();
        changed.coded_width = 1920;
        assert!(dec.is_sequence_change(&changed));
    }

    // -----------------------------------------------------------------------
    // API version constant
    // -----------------------------------------------------------------------

    #[test]
    fn test_api_version() {
        // (0 << 22) | (9 << 12) | 9 = 36873
        assert_eq!(NV_VULKAN_VIDEO_PARSER_API_VERSION, (9 << 12) | 9);
    }

    // -----------------------------------------------------------------------
    // Large Exp-Golomb values
    // -----------------------------------------------------------------------

    #[test]
    fn test_exp_golomb_ue_large() {
        // ue(14) = 0001111 (3 leading zeros, 1, then 110)
        // Binary: 000 1 111 -> 0b0001_1110 at bit 0
        let data = [0b0001_1110];
        let (val, pos) = exp_golomb_ue(&data, 0);
        assert_eq!(val, 14);
        assert_eq!(pos, 7);
    }

    #[test]
    fn test_exp_golomb_se_negative_large() {
        // se(-3) from ue(6) = 00111.  eg=6, even -> -(6/2) = -3
        // 00111 = 0b0011_1000
        let data = [0b0011_1000];
        let (val, pos) = exp_golomb_se(&data, 0);
        assert_eq!(val, -3);
        assert_eq!(pos, 5);
    }
}
