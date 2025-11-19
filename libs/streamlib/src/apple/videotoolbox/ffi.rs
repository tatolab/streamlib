// VideoToolbox FFI Bindings
//
// Foreign function interface declarations for VideoToolbox, CoreMedia, and CoreFoundation.
// These are the low-level C APIs used for hardware-accelerated video encoding.

use std::ffi::c_void;

pub(super) type OSStatus = i32;
pub(super) type VTCompressionSessionRef = *mut c_void;
pub(super) type CVPixelBufferRef = *mut c_void;
pub(super) type CMSampleBufferRef = *mut c_void;
pub(super) type CMTimeValue = i64;
pub(super) type CMTimeScale = i32;
pub(super) type CMTimeFlags = u32;
pub(super) type CFStringRef = *const c_void;
pub(super) type CFNumberRef = *const c_void;
pub(super) type CFBooleanRef = *const c_void;
pub(super) type CMBlockBufferRef = *mut c_void;
pub(super) type CMFormatDescriptionRef = *mut c_void;
pub(super) type VTPixelTransferSessionRef = *mut c_void;
pub(super) type CFArrayRef = *const c_void;
pub(super) type CFDictionaryRef = *const c_void;

#[repr(C)]
pub(super) struct CMTime {
    pub value: CMTimeValue,
    pub timescale: CMTimeScale,
    pub flags: CMTimeFlags,
    pub epoch: i64,
}

impl CMTime {
    pub fn new(value: i64, timescale: i32) -> Self {
        Self {
            value,
            timescale,
            flags: 1, // kCMTimeFlags_Valid
            epoch: 0,
        }
    }

    pub fn invalid() -> Self {
        Self {
            value: 0,
            timescale: 0,
            flags: 0,
            epoch: 0,
        }
    }
}

pub(super) const K_CVRETURN_SUCCESS: OSStatus = 0;
pub(super) const NO_ERR: OSStatus = 0;

// Codec types
pub(super) const K_CMVIDEO_CODEC_TYPE_H264: u32 = 0x61766331; // 'avc1'

// VTCompressionSession callback type
pub(super) type VTCompressionOutputCallback = extern "C" fn(
    output_callback_ref_con: *mut c_void,
    source_frame_ref_con: *mut c_void,
    status: OSStatus,
    info_flags: u32,
    sample_buffer: CMSampleBufferRef,
);

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    pub(super) fn VTCompressionSessionCreate(
        allocator: *const c_void,
        width: i32,
        height: i32,
        codec_type: u32,
        encoder_specification: *const c_void,
        source_image_buffer_attributes: *const c_void,
        compressed_data_allocator: *const c_void,
        output_callback: VTCompressionOutputCallback,
        output_callback_ref_con: *mut c_void,
        compression_session_out: *mut VTCompressionSessionRef,
    ) -> OSStatus;

    pub(super) fn VTCompressionSessionEncodeFrame(
        session: VTCompressionSessionRef,
        image_buffer: CVPixelBufferRef,
        presentation_time_stamp: CMTime,
        duration: CMTime,
        frame_properties: *const c_void,
        source_frame_ref_con: *mut c_void,
        info_flags_out: *mut u32,
    ) -> OSStatus;

    pub(super) fn VTCompressionSessionCompleteFrames(
        session: VTCompressionSessionRef,
        complete_until_presentation_time_stamp: CMTime,
    ) -> OSStatus;

    pub(super) fn VTCompressionSessionInvalidate(
        session: VTCompressionSessionRef,
    );

    pub(super) fn VTSessionSetProperty(
        session: VTCompressionSessionRef,
        property_key: CFStringRef,
        property_value: *const c_void,
    ) -> OSStatus;

    // VTPixelTransferSession - GPU-accelerated format conversion
    pub(super) fn VTPixelTransferSessionCreate(
        allocator: *const c_void,
        pixel_transfer_session_out: *mut VTPixelTransferSessionRef,
    ) -> OSStatus;

    pub(super) fn VTPixelTransferSessionTransferImage(
        session: VTPixelTransferSessionRef,
        source_buffer: CVPixelBufferRef,
        destination_buffer: CVPixelBufferRef,
    ) -> OSStatus;

    pub(super) fn VTPixelTransferSessionInvalidate(
        session: VTPixelTransferSessionRef,
    );

    // For getting encoded data from CMSampleBuffer
    pub(super) fn CMSampleBufferGetDataBuffer(
        sbuf: CMSampleBufferRef,
    ) -> CMBlockBufferRef;

    pub(super) fn CMBlockBufferGetDataLength(
        the_buffer: CMBlockBufferRef,
    ) -> usize;

    pub(super) fn CMBlockBufferCopyDataBytes(
        the_buffer: CMBlockBufferRef,
        offset_to_data: usize,
        data_length: usize,
        destination: *mut u8,
    ) -> OSStatus;

    pub(super) fn CMSampleBufferGetFormatDescription(
        sbuf: CMSampleBufferRef,
    ) -> CMFormatDescriptionRef;

    // For extracting SPS/PPS parameter sets from format description
    pub(super) fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        video_desc: CMFormatDescriptionRef,
        parameter_set_index: usize,
        parameter_set_pointer_out: *mut *const u8,
        parameter_set_size_out: *mut usize,
        parameter_set_count_out: *mut usize,
        nal_unit_header_length_out: *mut i32,
    ) -> OSStatus;

    // For checking keyframe status via sample attachments
    pub(super) fn CMSampleBufferGetSampleAttachmentsArray(
        sbuf: CMSampleBufferRef,
        create_if_necessary: bool,
    ) -> CFArrayRef;

    // Sample attachment keys
    pub(super) static kCMSampleAttachmentKey_NotSync: CFStringRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub(super) fn CFNumberCreate(
        allocator: *const c_void,
        the_type: i32,
        value_ptr: *const c_void,
    ) -> CFNumberRef;

    pub(super) fn CFRelease(cf: *const c_void);

    // CFArray functions for accessing sample attachments
    pub(super) fn CFArrayGetCount(the_array: CFArrayRef) -> isize;
    pub(super) fn CFArrayGetValueAtIndex(the_array: CFArrayRef, idx: isize) -> *const c_void;

    // CFDictionary functions for checking attachment keys
    pub(super) fn CFDictionaryGetValue(
        the_dict: CFDictionaryRef,
        key: *const c_void,
    ) -> *const c_void;

    // Boolean constants
    pub(super) static kCFBooleanTrue: CFBooleanRef;
    pub(super) static kCFBooleanFalse: CFBooleanRef;
}

// CFNumber types
pub(super) const K_CFNUMBER_SINT32_TYPE: i32 = 3;

// VideoToolbox property keys and values
#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    // Profile/Level property key
    pub(super) static kVTCompressionPropertyKey_ProfileLevel: CFStringRef;

    // H.264 Baseline Profile Level 3.1 (matches 42e01f in SDP)
    // This is the most compatible profile for WebRTC streaming
    pub(super) static kVTProfileLevel_H264_Baseline_3_1: CFStringRef;

    // Real-time encoding properties
    pub(super) static kVTCompressionPropertyKey_RealTime: CFStringRef;
    pub(super) static kVTCompressionPropertyKey_AllowFrameReordering: CFStringRef;
    pub(super) static kVTCompressionPropertyKey_MaxKeyFrameInterval: CFStringRef;
    pub(super) static kVTCompressionPropertyKey_AverageBitRate: CFStringRef;
    pub(super) static kVTCompressionPropertyKey_ExpectedFrameRate: CFStringRef;

    // Encode frame options
    pub(super) static kVTEncodeFrameOptionKey_ForceKeyFrame: CFStringRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub(super) fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;
}
