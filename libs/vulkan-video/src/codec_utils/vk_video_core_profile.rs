// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of `common/include/VkVideoCore/VkVideoCoreProfile.h`.
//!
//! Wraps Vulkan video profile structures and provides helpers for populating
//! codec-specific profile extensions, format queries, and YCbCr utilities.
//!
//! Key divergences from C++:
//! - The C++ union of codec-specific profile structs is replaced by a Rust enum
//!   (`CodecProfileExt`) for type safety.
//! - ash 0.38 does not include VP9 decode or AV1 encode extensions. These are
//!   represented as stub variants so the overall structure mirrors nvpro. When
//!   ash gains support, the stubs will be replaced with real types.
//! - ash structs carry lifetimes; we store owned copies with `'static` lifetime
//!   and manage `p_next` pointers manually.
//! - `YcbcrPrimariesConstants` and `CodecGetMatrixCoefficients` are ported
//!   inline since the original lives in `nvidia_utils/vulkan/ycbcr_utils.h`
//!   which is a small utility header.

use vulkanalia::vk;

// ---------------------------------------------------------------------------
// StdChromaFormatIdc — mirrors the C++ enum of the same name
// ---------------------------------------------------------------------------

/// Generic chroma format indicator that unifies H.264 and H.265 chroma_format_idc values.
///
/// The C++ codebase asserts that the H.264 and H.265 chroma format constants are
/// numerically identical, so a single enum suffices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum StdChromaFormatIdc {
    Monochrome = 0,
    Chroma420 = 1,
    Chroma422 = 2,
    Chroma444 = 3,
}

// Compile-time assertions matching the C++ static_asserts.
const _: () = {
    use vulkanalia::vk::video::{
        STD_VIDEO_H264_CHROMA_FORMAT_IDC_MONOCHROME,
        STD_VIDEO_H264_CHROMA_FORMAT_IDC_420,
        STD_VIDEO_H264_CHROMA_FORMAT_IDC_422,
        STD_VIDEO_H264_CHROMA_FORMAT_IDC_444,
        STD_VIDEO_H265_CHROMA_FORMAT_IDC_MONOCHROME,
        STD_VIDEO_H265_CHROMA_FORMAT_IDC_420,
        STD_VIDEO_H265_CHROMA_FORMAT_IDC_422,
        STD_VIDEO_H265_CHROMA_FORMAT_IDC_444,
    };
    assert!(
        STD_VIDEO_H264_CHROMA_FORMAT_IDC_MONOCHROME.0
            == STD_VIDEO_H265_CHROMA_FORMAT_IDC_MONOCHROME.0
    );
    assert!(
        STD_VIDEO_H264_CHROMA_FORMAT_IDC_420.0
            == STD_VIDEO_H265_CHROMA_FORMAT_IDC_420.0
    );
    assert!(
        STD_VIDEO_H264_CHROMA_FORMAT_IDC_422.0
            == STD_VIDEO_H265_CHROMA_FORMAT_IDC_422.0
    );
    assert!(
        STD_VIDEO_H264_CHROMA_FORMAT_IDC_444.0
            == STD_VIDEO_H265_CHROMA_FORMAT_IDC_444.0
    );
};

// ---------------------------------------------------------------------------
// YCbCr primaries constants — ported from nvidia_utils/vulkan/ycbcr_utils.h
// ---------------------------------------------------------------------------

/// Luma coefficients used for YCbCr <-> RGB matrix conversion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct YcbcrPrimariesConstants {
    pub kb: f64,
    pub kr: f64,
}

/// Standard identifiers for YCbCr primary sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YcbcrBtStandard {
    Bt709,
    Bt601Ebu,
    Bt601Smtpe,
    Bt2020,
}

/// Returns the KB/KR luma coefficients for a given BT standard.
pub fn get_ycbcr_primaries_constants(standard: YcbcrBtStandard) -> YcbcrPrimariesConstants {
    match standard {
        YcbcrBtStandard::Bt709 => YcbcrPrimariesConstants {
            kb: 0.0722,
            kr: 0.2126,
        },
        YcbcrBtStandard::Bt601Ebu => YcbcrPrimariesConstants {
            kb: 0.0713,
            kr: 0.2990,
        },
        YcbcrBtStandard::Bt601Smtpe => YcbcrPrimariesConstants {
            kb: 0.0870,
            kr: 0.2120,
        },
        YcbcrBtStandard::Bt2020 => YcbcrPrimariesConstants {
            kb: 0.0593,
            kr: 0.2627,
        },
    }
}

// ---------------------------------------------------------------------------
// Codec profile extension — replaces the C++ union
// ---------------------------------------------------------------------------

/// Codec-specific profile extension data.
///
/// In the C++ source this is a union of Vulkan profile-info structs.
/// Rust replaces the union with an enum for safety.
#[derive(Clone)]
pub enum CodecProfileExt {
    None,
    DecodeH264(vk::VideoDecodeH264ProfileInfoKHR),
    DecodeH265(vk::VideoDecodeH265ProfileInfoKHR),
    DecodeAv1(vk::VideoDecodeAV1ProfileInfoKHR),
    /// Placeholder — ash 0.38 does not expose `VkVideoDecodeVP9ProfileInfoKHR`.
    DecodeVp9 {
        std_profile: u32,
    },
    EncodeH264(vk::VideoEncodeH264ProfileInfoKHR),
    EncodeH265(vk::VideoEncodeH265ProfileInfoKHR),
    /// Placeholder — ash 0.38 does not expose `VkVideoEncodeAV1ProfileInfoKHR`.
    EncodeAv1 {
        std_profile: u32,
    },
}

impl Default for CodecProfileExt {
    fn default() -> Self {
        Self::None
    }
}

// Manually implement Debug since ash vk structs may not impl Debug.
impl std::fmt::Debug for CodecProfileExt {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "None"),
            Self::DecodeH264(_) => write!(f, "DecodeH264(..)"),
            Self::DecodeH265(_) => write!(f, "DecodeH265(..)"),
            Self::DecodeAv1(_) => write!(f, "DecodeAv1(..)"),
            Self::DecodeVp9 { std_profile } => {
                write!(f, "DecodeVp9(std_profile={std_profile})")
            }
            Self::EncodeH264(_) => write!(f, "EncodeH264(..)"),
            Self::EncodeH265(_) => write!(f, "EncodeH265(..)"),
            Self::EncodeAv1 { std_profile } => {
                write!(f, "EncodeAv1(std_profile={std_profile})")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Raw codec operation constants for codecs ash 0.38 doesn't define
// ---------------------------------------------------------------------------

/// `VK_VIDEO_CODEC_OPERATION_DECODE_VP9_BIT_KHR` — available in vulkanalia 0.35.
const CODEC_OP_DECODE_VP9: vk::VideoCodecOperationFlagsKHR =
    vk::VideoCodecOperationFlagsKHR::DECODE_VP9;

/// `VK_VIDEO_CODEC_OPERATION_ENCODE_AV1_BIT_KHR` — available in vulkanalia 0.35.
const CODEC_OP_ENCODE_AV1: vk::VideoCodecOperationFlagsKHR =
    vk::VideoCodecOperationFlagsKHR::ENCODE_AV1;

// ---------------------------------------------------------------------------
// VkVideoCoreProfile
// ---------------------------------------------------------------------------

/// Rust translation of `VkVideoCoreProfile`.
///
/// Owns all the Vulkan profile structures and keeps internal `p_next` chains
/// consistent. The `profile()` / `profile_list()` accessors return references
/// whose lifetimes are tied to `&self`, so callers never observe dangling
/// pointers as long as the struct is not moved while the references are live.
///
/// Because the struct contains raw pointers (`p_next`) it is **not** safe to
/// move after `profile()` has been called. Pin or box if needed.
pub struct VkVideoCoreProfile {
    /// `VkVideoProfileInfoKHR` — the root profile struct.
    profile: vk::VideoProfileInfoKHR,
    /// `VkVideoProfileListInfoKHR` with `profileCount = 1`.
    profile_list: vk::VideoProfileListInfoKHR,
    /// Encode usage info (only meaningful for encode codecs).
    encode_usage_info: vk::VideoEncodeUsageInfoKHR,
    /// The codec-specific profile extension.
    codec_ext: CodecProfileExt,
    /// Tracks whether the profile is in a valid state.
    valid: bool,
}

impl std::fmt::Debug for VkVideoCoreProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("VkVideoCoreProfile")
            .field("codec_operation", &self.profile.video_codec_operation)
            .field("chroma_subsampling", &self.profile.chroma_subsampling)
            .field("luma_bit_depth", &self.profile.luma_bit_depth)
            .field("chroma_bit_depth", &self.profile.chroma_bit_depth)
            .field("codec_ext", &self.codec_ext)
            .field("valid", &self.valid)
            .finish()
    }
}

// Safety: the struct only stores owned data behind raw pointers; no sharing
// across threads beyond what Vulkan already requires.
unsafe impl Send for VkVideoCoreProfile {}
unsafe impl Sync for VkVideoCoreProfile {}

impl Clone for VkVideoCoreProfile {
    fn clone(&self) -> Self {
        let mut dst = Self::new_default();
        dst.copy_profile(self);
        dst
    }
}

impl VkVideoCoreProfile {
    // -- Construction -------------------------------------------------------

    /// Create a default (invalid) profile.
    pub fn new_default() -> Self {
        Self {
            profile: vk::VideoProfileInfoKHR {
                s_type: vk::StructureType::VIDEO_PROFILE_INFO_KHR,
                next: std::ptr::null(),
                video_codec_operation: vk::VideoCodecOperationFlagsKHR::NONE,
                chroma_subsampling: vk::VideoChromaSubsamplingFlagsKHR::INVALID,
                luma_bit_depth: vk::VideoComponentBitDepthFlagsKHR::INVALID,
                chroma_bit_depth: vk::VideoComponentBitDepthFlagsKHR::INVALID,
                // _marker removed: vulkanalia structs don't have PhantomData
            },
            profile_list: vk::VideoProfileListInfoKHR::default(),
            encode_usage_info: vk::VideoEncodeUsageInfoKHR::default(),
            codec_ext: CodecProfileExt::None,
            valid: false,
        }
    }

    /// Construct from explicit parameters — mirrors the main C++ constructor.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        video_codec_operation: vk::VideoCodecOperationFlagsKHR,
        chroma_subsampling: vk::VideoChromaSubsamplingFlagsKHR,
        luma_bit_depth: vk::VideoComponentBitDepthFlagsKHR,
        chroma_bit_depth: vk::VideoComponentBitDepthFlagsKHR,
        video_h26x_profile_idc: u32,
        h264_picture_layout: vk::VideoDecodeH264PictureLayoutFlagsKHR,
        encode_usage_info: Option<vk::VideoEncodeUsageInfoKHR>,
    ) -> Self {
        let encode_usage = encode_usage_info.unwrap_or_else(|| vk::VideoEncodeUsageInfoKHR {
            s_type: vk::StructureType::VIDEO_ENCODE_USAGE_INFO_KHR,
            next: std::ptr::null(),
            video_usage_hints: vk::VideoEncodeUsageFlagsKHR::DEFAULT,
            video_content_hints: vk::VideoEncodeContentFlagsKHR::DEFAULT,
            tuning_mode: vk::VideoEncodeTuningModeKHR::DEFAULT,
        });

        let mut this = Self {
            profile: vk::VideoProfileInfoKHR {
                s_type: vk::StructureType::VIDEO_PROFILE_INFO_KHR,
                next: std::ptr::null(),
                video_codec_operation,
                chroma_subsampling,
                luma_bit_depth,
                chroma_bit_depth,
                // _marker removed: vulkanalia structs don't have PhantomData
            },
            profile_list: vk::VideoProfileListInfoKHR::default(),
            encode_usage_info: encode_usage,
            codec_ext: CodecProfileExt::None,
            valid: false,
        };

        // Point profile_list at our profile (will be fixed up after population).
        this.profile_list = vk::VideoProfileListInfoKHR {
            s_type: vk::StructureType::VIDEO_PROFILE_LIST_INFO_KHR,
            next: std::ptr::null(),
            profile_count: 1,
            profiles: &this.profile as *const _,
        };

        if !Self::is_valid_codec(video_codec_operation) {
            return this;
        }

        // Build the codec-specific extension from the parameters.
        let ext = Self::build_codec_ext(
            video_codec_operation,
            video_h26x_profile_idc,
            h264_picture_layout,
        );

        this.populate_profile_ext_inner(ext);
        this
    }

    /// Construct from an existing `VkVideoProfileInfoKHR`.
    ///
    /// Mirrors the C++ `VkVideoCoreProfile(const VkVideoProfileInfoKHR*)` constructor.
    ///
    /// # Safety
    /// `p_next` chain of `video_profile` must point to a valid codec-specific
    /// profile struct if non-null.
    pub unsafe fn from_profile_info(video_profile: &vk::VideoProfileInfoKHR) -> Self {
        let mut this = Self::new_default();
        this.init_from_profile(video_profile);
        this
    }

    // -- Static helpers -----------------------------------------------------

    /// Returns `true` if `video_codec_operations` contains at least one known codec bit.
    pub fn is_valid_codec(video_codec_operations: vk::VideoCodecOperationFlagsKHR) -> bool {
        let known = vk::VideoCodecOperationFlagsKHR::DECODE_H264
            | vk::VideoCodecOperationFlagsKHR::DECODE_H265
            | vk::VideoCodecOperationFlagsKHR::DECODE_AV1
            | CODEC_OP_DECODE_VP9
            | vk::VideoCodecOperationFlagsKHR::ENCODE_H264
            | vk::VideoCodecOperationFlagsKHR::ENCODE_H265
            | CODEC_OP_ENCODE_AV1;

        video_codec_operations & known != vk::VideoCodecOperationFlagsKHR::NONE
    }

    // -- Profile population -------------------------------------------------

    /// Populate the codec-specific extension from a `CodecProfileExt` value.
    fn populate_profile_ext_inner(&mut self, ext: CodecProfileExt) {
        self.codec_ext = ext;
        self.valid = true;

        // Wire up p_next from profile -> codec ext.
        // We intentionally clear the codec ext's own p_next except where
        // encode usage info is chained.
        match &mut self.codec_ext {
            CodecProfileExt::DecodeH264(ref mut p) => {
                p.next = std::ptr::null();
                self.profile.next = p as *const _ as *const _;
            }
            CodecProfileExt::DecodeH265(ref mut p) => {
                p.next = std::ptr::null();
                self.profile.next = p as *const _ as *const _;
            }
            CodecProfileExt::DecodeAv1(ref mut p) => {
                p.next = std::ptr::null();
                self.profile.next = p as *const _ as *const _;
            }
            CodecProfileExt::DecodeVp9 { .. } => {
                // VP9 not in ash — cannot chain. Profile remains without p_next.
                self.profile.next = std::ptr::null();
            }
            CodecProfileExt::EncodeH264(ref mut p) => {
                // Chain encode usage info behind the codec ext.
                p.next = &self.encode_usage_info as *const _ as *const _;
                self.profile.next = p as *const _ as *const _;
            }
            CodecProfileExt::EncodeH265(ref mut p) => {
                p.next = &self.encode_usage_info as *const _ as *const _;
                self.profile.next = p as *const _ as *const _;
            }
            CodecProfileExt::EncodeAv1 { .. } => {
                // AV1 encode not in ash — cannot chain.
                self.profile.next = std::ptr::null();
            }
            CodecProfileExt::None => {
                self.valid = false;
                self.profile.next = std::ptr::null();
            }
        }

        // Keep profile_list pointing at our profile.
        self.profile_list.profiles = &self.profile as *const _;
    }

    /// Populate the codec-specific extension from a raw Vulkan `pVideoProfileExt`
    /// pointer.  Mirrors C++ `PopulateProfileExt`.
    ///
    /// # Safety
    /// The `video_profile_ext` pointer (if non-null) must point to a valid
    /// Vulkan structure whose `sType` matches the current codec operation.
    pub unsafe fn populate_profile_ext(
        &mut self,
        video_profile_ext: *const vk::BaseInStructure,
    ) -> bool {
        let op = self.profile.video_codec_operation;

        let ext = if op == vk::VideoCodecOperationFlagsKHR::DECODE_H264 {
            if !video_profile_ext.is_null() {
                let p = &*(video_profile_ext as *const vk::VideoDecodeH264ProfileInfoKHR);
                if p.s_type != vk::StructureType::VIDEO_DECODE_H264_PROFILE_INFO_KHR {
                    self.profile.s_type = vk::StructureType::APPLICATION_INFO;
                    return false;
                }
                CodecProfileExt::DecodeH264(std::ptr::read(p as *const _ as *const _))
            } else {
                CodecProfileExt::DecodeH264(vk::VideoDecodeH264ProfileInfoKHR {
                    s_type: vk::StructureType::VIDEO_DECODE_H264_PROFILE_INFO_KHR,
                    next: std::ptr::null(),
                    std_profile_idc: vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN,
                    picture_layout: vk::VideoDecodeH264PictureLayoutFlagsKHR::INTERLACED_INTERLEAVED_LINES,
                    // _marker removed: vulkanalia structs don't have PhantomData
                })
            }
        } else if op == vk::VideoCodecOperationFlagsKHR::DECODE_H265 {
            if !video_profile_ext.is_null() {
                let p = &*(video_profile_ext as *const vk::VideoDecodeH265ProfileInfoKHR);
                if p.s_type != vk::StructureType::VIDEO_DECODE_H265_PROFILE_INFO_KHR {
                    self.profile.s_type = vk::StructureType::APPLICATION_INFO;
                    return false;
                }
                CodecProfileExt::DecodeH265(std::ptr::read(p as *const _ as *const _))
            } else {
                CodecProfileExt::DecodeH265(vk::VideoDecodeH265ProfileInfoKHR {
                    s_type: vk::StructureType::VIDEO_DECODE_H265_PROFILE_INFO_KHR,
                    next: std::ptr::null(),
                    std_profile_idc: vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN,
                    // _marker removed: vulkanalia structs don't have PhantomData
                })
            }
        } else if op == vk::VideoCodecOperationFlagsKHR::DECODE_AV1 {
            if !video_profile_ext.is_null() {
                let p = &*(video_profile_ext as *const vk::VideoDecodeAV1ProfileInfoKHR);
                if p.s_type != vk::StructureType::VIDEO_DECODE_AV1_PROFILE_INFO_KHR {
                    self.profile.s_type = vk::StructureType::APPLICATION_INFO;
                    return false;
                }
                CodecProfileExt::DecodeAv1(std::ptr::read(p as *const _ as *const _))
            } else {
                CodecProfileExt::DecodeAv1(vk::VideoDecodeAV1ProfileInfoKHR {
                    s_type: vk::StructureType::VIDEO_DECODE_AV1_PROFILE_INFO_KHR,
                    next: std::ptr::null(),
                    std_profile: vk::video::STD_VIDEO_AV1_PROFILE_MAIN,
                    film_grain_support: vk::FALSE,
                    // _marker removed: vulkanalia structs don't have PhantomData
                })
            }
        } else if op == CODEC_OP_DECODE_VP9 {
            // VP9 decode not in ash 0.38 — stub handling.
            CodecProfileExt::DecodeVp9 { std_profile: 0 }
        } else if op == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
            if !video_profile_ext.is_null() {
                let p = &*(video_profile_ext as *const vk::VideoEncodeH264ProfileInfoKHR);
                if p.s_type != vk::StructureType::VIDEO_ENCODE_H264_PROFILE_INFO_KHR {
                    self.profile.s_type = vk::StructureType::APPLICATION_INFO;
                    return false;
                }
                // Copy encode usage info if chained.
                if !p.next.is_null() {
                    let usage = &*(p.next as *const vk::VideoEncodeUsageInfoKHR);
                    if usage.s_type == vk::StructureType::VIDEO_ENCODE_USAGE_INFO_KHR {
                        self.encode_usage_info = std::ptr::read(usage as *const _ as *const _);
                    }
                }
                CodecProfileExt::EncodeH264(std::ptr::read(p as *const _ as *const _))
            } else {
                CodecProfileExt::EncodeH264(vk::VideoEncodeH264ProfileInfoKHR {
                    s_type: vk::StructureType::VIDEO_ENCODE_H264_PROFILE_INFO_KHR,
                    next: std::ptr::null(),
                    std_profile_idc: vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN,
                    // _marker removed: vulkanalia structs don't have PhantomData
                })
            }
        } else if op == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
            if !video_profile_ext.is_null() {
                let p = &*(video_profile_ext as *const vk::VideoEncodeH265ProfileInfoKHR);
                if p.s_type != vk::StructureType::VIDEO_ENCODE_H265_PROFILE_INFO_KHR {
                    self.profile.s_type = vk::StructureType::APPLICATION_INFO;
                    return false;
                }
                if !p.next.is_null() {
                    let usage = &*(p.next as *const vk::VideoEncodeUsageInfoKHR);
                    if usage.s_type == vk::StructureType::VIDEO_ENCODE_USAGE_INFO_KHR {
                        self.encode_usage_info = std::ptr::read(usage as *const _ as *const _);
                    }
                }
                CodecProfileExt::EncodeH265(std::ptr::read(p as *const _ as *const _))
            } else {
                CodecProfileExt::EncodeH265(vk::VideoEncodeH265ProfileInfoKHR {
                    s_type: vk::StructureType::VIDEO_ENCODE_H265_PROFILE_INFO_KHR,
                    next: std::ptr::null(),
                    std_profile_idc: vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN,
                    // _marker removed: vulkanalia structs don't have PhantomData
                })
            }
        } else if op == CODEC_OP_ENCODE_AV1 {
            // AV1 encode not in vulkanalia 0.35 — stub handling.
            CodecProfileExt::EncodeAv1 {
                std_profile: vk::video::STD_VIDEO_AV1_PROFILE_MAIN.0 as u32,
            }
        } else {
            debug_assert!(false, "Unknown codec!");
            return false;
        };

        self.populate_profile_ext_inner(ext);
        true
    }

    /// Mirrors C++ `InitFromProfile`.
    ///
    /// # Safety
    /// Same requirements as `populate_profile_ext`.
    pub unsafe fn init_from_profile(
        &mut self,
        video_profile: &vk::VideoProfileInfoKHR,
    ) -> bool {
        self.profile = std::ptr::read(video_profile as *const _ as *const _);
        let next = video_profile.next;
        self.profile.next = std::ptr::null();
        self.populate_profile_ext(next as *const vk::BaseInStructure)
    }

    // -- Build a CodecProfileExt from constructor parameters -----------------

    fn build_codec_ext(
        video_codec_operation: vk::VideoCodecOperationFlagsKHR,
        video_h26x_profile_idc: u32,
        h264_picture_layout: vk::VideoDecodeH264PictureLayoutFlagsKHR,
    ) -> CodecProfileExt {
        use vulkanalia::vk::video::*;

        if video_codec_operation == vk::VideoCodecOperationFlagsKHR::DECODE_H264 {
            let std_profile_idc = if video_h26x_profile_idc == 0 {
                STD_VIDEO_H264_PROFILE_IDC_INVALID
            } else {
                StdVideoH264ProfileIdc(video_h26x_profile_idc as _)
            };
            CodecProfileExt::DecodeH264(vk::VideoDecodeH264ProfileInfoKHR {
                s_type: vk::StructureType::VIDEO_DECODE_H264_PROFILE_INFO_KHR,
                next: std::ptr::null(),
                std_profile_idc,
                picture_layout: h264_picture_layout,
            })
        } else if video_codec_operation == vk::VideoCodecOperationFlagsKHR::DECODE_H265 {
            let std_profile_idc = if video_h26x_profile_idc == 0 {
                STD_VIDEO_H265_PROFILE_IDC_INVALID
            } else {
                StdVideoH265ProfileIdc(video_h26x_profile_idc as _)
            };
            CodecProfileExt::DecodeH265(vk::VideoDecodeH265ProfileInfoKHR {
                s_type: vk::StructureType::VIDEO_DECODE_H265_PROFILE_INFO_KHR,
                next: std::ptr::null(),
                std_profile_idc,
            })
        } else if video_codec_operation == vk::VideoCodecOperationFlagsKHR::DECODE_AV1 {
            let av1_profile = StdVideoAV1Profile(video_h26x_profile_idc as _);
            // C++ asserts the profile is one of MAIN/HIGH/PROFESSIONAL.
            debug_assert!(
                av1_profile == STD_VIDEO_AV1_PROFILE_MAIN
                    || av1_profile == STD_VIDEO_AV1_PROFILE_HIGH
                    || av1_profile == STD_VIDEO_AV1_PROFILE_PROFESSIONAL,
                "Bad AV1 profile IDC"
            );
            CodecProfileExt::DecodeAv1(vk::VideoDecodeAV1ProfileInfoKHR {
                s_type: vk::StructureType::VIDEO_DECODE_AV1_PROFILE_INFO_KHR,
                next: std::ptr::null(),
                std_profile: av1_profile,
                film_grain_support: vk::FALSE,
            })
        } else if video_codec_operation == CODEC_OP_DECODE_VP9 {
            let std_profile = if video_h26x_profile_idc == 0 {
                0 // STD_VIDEO_VP9_PROFILE_0
            } else {
                video_h26x_profile_idc
            };
            CodecProfileExt::DecodeVp9 { std_profile }
        } else if video_codec_operation == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
            let std_profile_idc = if video_h26x_profile_idc == 0 {
                STD_VIDEO_H264_PROFILE_IDC_INVALID
            } else {
                StdVideoH264ProfileIdc(video_h26x_profile_idc as _)
            };
            CodecProfileExt::EncodeH264(vk::VideoEncodeH264ProfileInfoKHR {
                s_type: vk::StructureType::VIDEO_ENCODE_H264_PROFILE_INFO_KHR,
                next: std::ptr::null(), // will be wired to encode_usage_info later
                std_profile_idc,
            })
        } else if video_codec_operation == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
            let std_profile_idc = if video_h26x_profile_idc == 0 {
                STD_VIDEO_H265_PROFILE_IDC_INVALID
            } else {
                StdVideoH265ProfileIdc(video_h26x_profile_idc as _)
            };
            CodecProfileExt::EncodeH265(vk::VideoEncodeH265ProfileInfoKHR {
                s_type: vk::StructureType::VIDEO_ENCODE_H265_PROFILE_INFO_KHR,
                next: std::ptr::null(),
                std_profile_idc,
            })
        } else if video_codec_operation == CODEC_OP_ENCODE_AV1 {
            let std_profile = if video_h26x_profile_idc == 0 {
                STD_VIDEO_AV1_PROFILE_MAIN.0 as u32
            } else {
                video_h26x_profile_idc
            };
            CodecProfileExt::EncodeAv1 { std_profile }
        } else {
            debug_assert!(false, "Unknown codec!");
            CodecProfileExt::None
        }
    }

    // -- Accessors ----------------------------------------------------------

    /// Returns the codec operation flag.
    pub fn get_codec_type(&self) -> vk::VideoCodecOperationFlagsKHR {
        self.profile.video_codec_operation
    }

    /// `true` if the codec operation is one of the encode types.
    pub fn is_encode_codec_type(&self) -> bool {
        let op = self.profile.video_codec_operation;
        op == vk::VideoCodecOperationFlagsKHR::ENCODE_H264
            || op == vk::VideoCodecOperationFlagsKHR::ENCODE_H265
            || op == CODEC_OP_ENCODE_AV1
    }

    /// `true` if the codec operation is one of the decode types.
    pub fn is_decode_codec_type(&self) -> bool {
        let op = self.profile.video_codec_operation;
        op == vk::VideoCodecOperationFlagsKHR::DECODE_H264
            || op == vk::VideoCodecOperationFlagsKHR::DECODE_H265
            || op == vk::VideoCodecOperationFlagsKHR::DECODE_AV1
            || op == CODEC_OP_DECODE_VP9
    }

    /// `true` if the profile is in a valid state (mirrors C++ `operator bool`).
    pub fn is_valid(&self) -> bool {
        self.valid && self.profile.s_type == vk::StructureType::VIDEO_PROFILE_INFO_KHR
    }

    /// Returns a reference to the root `VkVideoProfileInfoKHR`, or `None` if invalid.
    pub fn get_profile(&self) -> Option<&vk::VideoProfileInfoKHR> {
        if self.profile.s_type == vk::StructureType::VIDEO_PROFILE_INFO_KHR {
            Some(&self.profile)
        } else {
            None
        }
    }

    /// Returns a reference to the `VkVideoProfileListInfoKHR`.
    pub fn get_profile_list_info(&self) -> Option<&vk::VideoProfileListInfoKHR> {
        if self.profile_list.s_type == vk::StructureType::VIDEO_PROFILE_LIST_INFO_KHR {
            Some(&self.profile_list)
        } else {
            None
        }
    }

    /// Returns the H.264 decode profile extension, if active.
    pub fn get_decode_h264_profile(&self) -> Option<&vk::VideoDecodeH264ProfileInfoKHR> {
        match &self.codec_ext {
            CodecProfileExt::DecodeH264(p)
                if p.s_type == vk::StructureType::VIDEO_DECODE_H264_PROFILE_INFO_KHR =>
            {
                Some(p)
            }
            _ => None,
        }
    }

    /// Returns the H.265 decode profile extension, if active.
    pub fn get_decode_h265_profile(&self) -> Option<&vk::VideoDecodeH265ProfileInfoKHR> {
        match &self.codec_ext {
            CodecProfileExt::DecodeH265(p)
                if p.s_type == vk::StructureType::VIDEO_DECODE_H265_PROFILE_INFO_KHR =>
            {
                Some(p)
            }
            _ => None,
        }
    }

    /// Returns the AV1 decode profile extension, if active.
    pub fn get_decode_av1_profile(&self) -> Option<&vk::VideoDecodeAV1ProfileInfoKHR> {
        match &self.codec_ext {
            CodecProfileExt::DecodeAv1(p)
                if p.s_type == vk::StructureType::VIDEO_DECODE_AV1_PROFILE_INFO_KHR =>
            {
                Some(p)
            }
            _ => None,
        }
    }

    /// Returns the VP9 decode profile's std_profile value, if active.
    /// Returns raw u32 since ash 0.38 lacks the VP9 struct.
    pub fn get_decode_vp9_std_profile(&self) -> Option<u32> {
        match &self.codec_ext {
            CodecProfileExt::DecodeVp9 { std_profile } => Some(*std_profile),
            _ => None,
        }
    }

    /// Returns the H.264 encode profile extension, if active.
    pub fn get_encode_h264_profile(&self) -> Option<&vk::VideoEncodeH264ProfileInfoKHR> {
        match &self.codec_ext {
            CodecProfileExt::EncodeH264(p)
                if p.s_type == vk::StructureType::VIDEO_ENCODE_H264_PROFILE_INFO_KHR =>
            {
                Some(p)
            }
            _ => None,
        }
    }

    /// Returns the H.265 encode profile extension, if active.
    pub fn get_encode_h265_profile(&self) -> Option<&vk::VideoEncodeH265ProfileInfoKHR> {
        match &self.codec_ext {
            CodecProfileExt::EncodeH265(p)
                if p.s_type == vk::StructureType::VIDEO_ENCODE_H265_PROFILE_INFO_KHR =>
            {
                Some(p)
            }
            _ => None,
        }
    }

    /// Returns the AV1 encode profile's std_profile value, if active.
    /// Returns raw u32 since ash 0.38 lacks the AV1 encode struct.
    pub fn get_encode_av1_std_profile(&self) -> Option<u32> {
        match &self.codec_ext {
            CodecProfileExt::EncodeAv1 { std_profile } => Some(*std_profile),
            _ => None,
        }
    }

    /// Returns the codec-specific extension enum.
    pub fn codec_ext(&self) -> &CodecProfileExt {
        &self.codec_ext
    }

    // -- Copy ---------------------------------------------------------------

    /// Mirrors C++ `copyProfile`.
    pub fn copy_profile(&mut self, src: &VkVideoCoreProfile) -> bool {
        if !src.is_valid() {
            return false;
        }

        self.profile = src.profile;
        self.profile.next = std::ptr::null();

        self.profile_list = src.profile_list;
        self.profile_list.next = std::ptr::null();
        self.profile_list.profiles = &self.profile as *const _;

        self.encode_usage_info = src.encode_usage_info;
        self.encode_usage_info.next = std::ptr::null();

        // Re-populate to fix up internal pointers.
        let ext = src.codec_ext.clone();
        self.populate_profile_ext_inner(ext);

        true
    }

    // -- Equality -----------------------------------------------------------

    /// Compares two profiles by codec operation, chroma subsampling, and bit depths.
    /// Mirrors C++ `operator==`.
    pub fn profile_eq(&self, other: &Self) -> bool {
        self.profile.video_codec_operation == other.profile.video_codec_operation
            && self.profile.chroma_subsampling == other.profile.chroma_subsampling
            && self.profile.luma_bit_depth == other.profile.luma_bit_depth
            && self.profile.chroma_bit_depth == other.profile.chroma_bit_depth
    }

    // -- Chroma / bit-depth queries -----------------------------------------

    pub fn get_color_subsampling(&self) -> vk::VideoChromaSubsamplingFlagsKHR {
        self.profile.chroma_subsampling
    }

    pub fn get_color_subsampling_generic(&self) -> StdChromaFormatIdc {
        let cs = self.profile.chroma_subsampling;
        if cs.contains(vk::VideoChromaSubsamplingFlagsKHR::MONOCHROME) {
            StdChromaFormatIdc::Monochrome
        } else if cs.contains(vk::VideoChromaSubsamplingFlagsKHR::_420) {
            StdChromaFormatIdc::Chroma420
        } else if cs.contains(vk::VideoChromaSubsamplingFlagsKHR::_422) {
            StdChromaFormatIdc::Chroma422
        } else if cs.contains(vk::VideoChromaSubsamplingFlagsKHR::_444) {
            StdChromaFormatIdc::Chroma444
        } else {
            StdChromaFormatIdc::Monochrome
        }
    }

    pub fn get_luma_bit_depth_minus8(&self) -> u32 {
        let ld = self.profile.luma_bit_depth;
        if ld.contains(vk::VideoComponentBitDepthFlagsKHR::_8) {
            0
        } else if ld.contains(vk::VideoComponentBitDepthFlagsKHR::_10) {
            2
        } else if ld.contains(vk::VideoComponentBitDepthFlagsKHR::_12) {
            4
        } else {
            0
        }
    }

    pub fn get_chroma_bit_depth_minus8(&self) -> u32 {
        let cd = self.profile.chroma_bit_depth;
        if cd.contains(vk::VideoComponentBitDepthFlagsKHR::_8) {
            0
        } else if cd.contains(vk::VideoComponentBitDepthFlagsKHR::_10) {
            2
        } else if cd.contains(vk::VideoComponentBitDepthFlagsKHR::_12) {
            4
        } else {
            0
        }
    }

    pub fn is_16bit_format(&self) -> bool {
        self.get_luma_bit_depth_minus8() != 0 || self.get_chroma_bit_depth_minus8() != 0
    }

    // -- Static format helpers ----------------------------------------------

    /// Maps chroma subsampling + luma bit depth + planarity to a `VkFormat`.
    pub fn codec_get_vk_format(
        chroma_subsampling: vk::VideoChromaSubsamplingFlagsKHR,
        luma_bit_depth: vk::VideoComponentBitDepthFlagsKHR,
        is_semi_planar: bool,
    ) -> vk::Format {
        match chroma_subsampling {
            vk::VideoChromaSubsamplingFlagsKHR::MONOCHROME => match luma_bit_depth {
                vk::VideoComponentBitDepthFlagsKHR::_8 => vk::Format::R8_UNORM,
                vk::VideoComponentBitDepthFlagsKHR::_10 => vk::Format::R10X6_UNORM_PACK16,
                vk::VideoComponentBitDepthFlagsKHR::_12 => vk::Format::R12X4_UNORM_PACK16,
                _ => {
                    debug_assert!(false, "Unsupported luma bit depth for monochrome");
                    vk::Format::UNDEFINED
                }
            },
            vk::VideoChromaSubsamplingFlagsKHR::_420 => match luma_bit_depth {
                vk::VideoComponentBitDepthFlagsKHR::_8 => {
                    if is_semi_planar {
                        vk::Format::G8_B8R8_2PLANE_420_UNORM
                    } else {
                        vk::Format::G8_B8_R8_3PLANE_420_UNORM
                    }
                }
                vk::VideoComponentBitDepthFlagsKHR::_10 => {
                    if is_semi_planar {
                        vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16
                    } else {
                        vk::Format::G10X6_B10X6_R10X6_3PLANE_420_UNORM_3PACK16
                    }
                }
                vk::VideoComponentBitDepthFlagsKHR::_12 => {
                    if is_semi_planar {
                        vk::Format::G12X4_B12X4R12X4_2PLANE_420_UNORM_3PACK16
                    } else {
                        vk::Format::G12X4_B12X4_R12X4_3PLANE_420_UNORM_3PACK16
                    }
                }
                _ => {
                    debug_assert!(false, "Unsupported luma bit depth for 420");
                    vk::Format::UNDEFINED
                }
            },
            vk::VideoChromaSubsamplingFlagsKHR::_422 => match luma_bit_depth {
                vk::VideoComponentBitDepthFlagsKHR::_8 => {
                    if is_semi_planar {
                        vk::Format::G8_B8R8_2PLANE_422_UNORM
                    } else {
                        vk::Format::G8_B8_R8_3PLANE_422_UNORM
                    }
                }
                vk::VideoComponentBitDepthFlagsKHR::_10 => {
                    if is_semi_planar {
                        vk::Format::G10X6_B10X6R10X6_2PLANE_422_UNORM_3PACK16
                    } else {
                        vk::Format::G10X6_B10X6_R10X6_3PLANE_422_UNORM_3PACK16
                    }
                }
                vk::VideoComponentBitDepthFlagsKHR::_12 => {
                    if is_semi_planar {
                        vk::Format::G12X4_B12X4R12X4_2PLANE_422_UNORM_3PACK16
                    } else {
                        vk::Format::G12X4_B12X4_R12X4_3PLANE_422_UNORM_3PACK16
                    }
                }
                _ => {
                    debug_assert!(false, "Unsupported luma bit depth for 422");
                    vk::Format::UNDEFINED
                }
            },
            vk::VideoChromaSubsamplingFlagsKHR::_444 => match luma_bit_depth {
                vk::VideoComponentBitDepthFlagsKHR::_8 => {
                    if is_semi_planar {
                        vk::Format::G8_B8R8_2PLANE_444_UNORM
                    } else {
                        vk::Format::G8_B8_R8_3PLANE_444_UNORM
                    }
                }
                vk::VideoComponentBitDepthFlagsKHR::_10 => {
                    if is_semi_planar {
                        vk::Format::G10X6_B10X6R10X6_2PLANE_444_UNORM_3PACK16
                    } else {
                        vk::Format::G10X6_B10X6_R10X6_3PLANE_444_UNORM_3PACK16
                    }
                }
                vk::VideoComponentBitDepthFlagsKHR::_12 => {
                    if is_semi_planar {
                        vk::Format::G12X4_B12X4R12X4_2PLANE_444_UNORM_3PACK16
                    } else {
                        vk::Format::G12X4_B12X4_R12X4_3PLANE_444_UNORM_3PACK16
                    }
                }
                _ => {
                    debug_assert!(false, "Unsupported luma bit depth for 444");
                    vk::Format::UNDEFINED
                }
            },
            _ => {
                debug_assert!(false, "Unsupported chroma subsampling");
                vk::Format::UNDEFINED
            }
        }
    }

    /// Determines the generic chroma format from a `VkFormat`.
    pub fn get_video_chroma_format_from_vk_format(format: vk::Format) -> StdChromaFormatIdc {
        match format {
            // Monochrome
            vk::Format::R8_UNORM
            | vk::Format::R10X6_UNORM_PACK16
            | vk::Format::R12X4_UNORM_PACK16 => StdChromaFormatIdc::Monochrome,

            // 4:2:0
            vk::Format::G8_B8R8_2PLANE_420_UNORM
            | vk::Format::G8_B8_R8_3PLANE_420_UNORM
            | vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16
            | vk::Format::G10X6_B10X6_R10X6_3PLANE_420_UNORM_3PACK16
            | vk::Format::G12X4_B12X4R12X4_2PLANE_420_UNORM_3PACK16
            | vk::Format::G12X4_B12X4_R12X4_3PLANE_420_UNORM_3PACK16 => {
                StdChromaFormatIdc::Chroma420
            }

            // 4:2:2
            vk::Format::G8_B8R8_2PLANE_422_UNORM
            | vk::Format::G8_B8_R8_3PLANE_422_UNORM
            | vk::Format::G10X6_B10X6R10X6_2PLANE_422_UNORM_3PACK16
            | vk::Format::G10X6_B10X6_R10X6_3PLANE_422_UNORM_3PACK16
            | vk::Format::G12X4_B12X4R12X4_2PLANE_422_UNORM_3PACK16
            | vk::Format::G12X4_B12X4_R12X4_3PLANE_422_UNORM_3PACK16 => {
                StdChromaFormatIdc::Chroma422
            }

            // 4:4:4
            vk::Format::G8_B8_R8_3PLANE_444_UNORM
            | vk::Format::G10X6_B10X6_R10X6_3PLANE_444_UNORM_3PACK16
            | vk::Format::G12X4_B12X4_R12X4_3PLANE_444_UNORM_3PACK16
            | vk::Format::G8_B8R8_2PLANE_444_UNORM
            | vk::Format::G10X6_B10X6R10X6_2PLANE_444_UNORM_3PACK16
            | vk::Format::G12X4_B12X4R12X4_2PLANE_444_UNORM_3PACK16
            | vk::Format::G16_B16R16_2PLANE_444_UNORM => StdChromaFormatIdc::Chroma444,

            _ => {
                debug_assert!(false, "Unsupported VkFormat for chroma detection");
                StdChromaFormatIdc::Chroma420
            }
        }
    }

    // -- Codec name ---------------------------------------------------------

    /// Returns a human-readable name for the codec operation.
    pub fn codec_to_name(codec: vk::VideoCodecOperationFlagsKHR) -> &'static str {
        if codec == vk::VideoCodecOperationFlagsKHR::DECODE_H264 {
            "decode h.264"
        } else if codec == vk::VideoCodecOperationFlagsKHR::DECODE_H265 {
            "decode h.265"
        } else if codec == vk::VideoCodecOperationFlagsKHR::DECODE_AV1 {
            "decode av1"
        } else if codec == CODEC_OP_DECODE_VP9 {
            "decode vp9"
        } else if codec == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
            "encode h.264"
        } else if codec == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
            "encode h.265"
        } else if codec == CODEC_OP_ENCODE_AV1 {
            "encode av1"
        } else {
            debug_assert!(false, "Unknown codec");
            "UNKNOWN"
        }
    }

    // -- Format profile dump (uses tracing instead of std::cout) ------------

    /// Logs the profile's chroma subsampling and bit depth information.
    /// Mirrors C++ `DumpFormatProfiles`.
    pub fn dump_format_profiles(video_profile: &vk::VideoProfileInfoKHR) {
        let cs = video_profile.chroma_subsampling;
        let mut chroma_parts = Vec::new();
        if cs.contains(vk::VideoChromaSubsamplingFlagsKHR::MONOCHROME) {
            chroma_parts.push("MONO");
        }
        if cs.contains(vk::VideoChromaSubsamplingFlagsKHR::_420) {
            chroma_parts.push("420");
        }
        if cs.contains(vk::VideoChromaSubsamplingFlagsKHR::_422) {
            chroma_parts.push("422");
        }
        if cs.contains(vk::VideoChromaSubsamplingFlagsKHR::_444) {
            chroma_parts.push("444");
        }

        let ld = video_profile.luma_bit_depth;
        let mut luma_parts = Vec::new();
        if ld.contains(vk::VideoComponentBitDepthFlagsKHR::_8) {
            luma_parts.push("LUMA: 8-bit");
        }
        if ld.contains(vk::VideoComponentBitDepthFlagsKHR::_10) {
            luma_parts.push("LUMA: 10-bit");
        }
        if ld.contains(vk::VideoComponentBitDepthFlagsKHR::_12) {
            luma_parts.push("LUMA: 12-bit");
        }

        let cd = video_profile.chroma_bit_depth;
        let mut chroma_depth_parts = Vec::new();
        if cd.contains(vk::VideoComponentBitDepthFlagsKHR::_8) {
            chroma_depth_parts.push("CHROMA: 8-bit");
        }
        if cd.contains(vk::VideoComponentBitDepthFlagsKHR::_10) {
            chroma_depth_parts.push("CHROMA: 10-bit");
        }
        if cd.contains(vk::VideoComponentBitDepthFlagsKHR::_12) {
            chroma_depth_parts.push("CHROMA: 12-bit");
        }

        tracing::debug!(
            chroma = %chroma_parts.join(", "),
            luma = %luma_parts.join(", "),
            chroma_depth = %chroma_depth_parts.join(", "),
            "Video format profiles"
        );
    }

    /// Comprehensive profile dump for debugging. Mirrors C++ `DumpProfile`.
    pub fn dump_profile(&self) {
        let op = self.profile.video_codec_operation;
        let op_name = Self::codec_to_name(op);

        let cs = self.profile.chroma_subsampling;
        let mut cs_parts = Vec::new();
        if cs.contains(vk::VideoChromaSubsamplingFlagsKHR::MONOCHROME) {
            cs_parts.push("MONOCHROME");
        }
        if cs.contains(vk::VideoChromaSubsamplingFlagsKHR::_420) {
            cs_parts.push("420");
        }
        if cs.contains(vk::VideoChromaSubsamplingFlagsKHR::_422) {
            cs_parts.push("422");
        }
        if cs.contains(vk::VideoChromaSubsamplingFlagsKHR::_444) {
            cs_parts.push("444");
        }

        let ld = self.profile.luma_bit_depth;
        let mut ld_parts = Vec::new();
        if ld.contains(vk::VideoComponentBitDepthFlagsKHR::_8) {
            ld_parts.push("8-bit");
        }
        if ld.contains(vk::VideoComponentBitDepthFlagsKHR::_10) {
            ld_parts.push("10-bit");
        }
        if ld.contains(vk::VideoComponentBitDepthFlagsKHR::_12) {
            ld_parts.push("12-bit");
        }

        let cd = self.profile.chroma_bit_depth;
        let mut cd_parts = Vec::new();
        if cd.contains(vk::VideoComponentBitDepthFlagsKHR::_8) {
            cd_parts.push("8-bit");
        }
        if cd.contains(vk::VideoComponentBitDepthFlagsKHR::_10) {
            cd_parts.push("10-bit");
        }
        if cd.contains(vk::VideoComponentBitDepthFlagsKHR::_12) {
            cd_parts.push("12-bit");
        }

        tracing::debug!(
            codec = %op_name,
            chroma_subsampling = %cs_parts.join(" "),
            luma_bit_depth = %ld_parts.join(" "),
            chroma_bit_depth = %cd_parts.join(" "),
            codec_ext = ?self.codec_ext,
            "=== Video Profile Dump ==="
        );
    }

    // -- YCbCr helpers (ported from ycbcr_utils.h) --------------------------

    /// Maps `video_full_range_flag` to `VkSamplerYcbcrRange`.
    pub fn codec_full_range_to_ycbcr_range(
        video_full_range_flag: bool,
    ) -> vk::SamplerYcbcrRange {
        if video_full_range_flag {
            vk::SamplerYcbcrRange::ITU_FULL
        } else {
            vk::SamplerYcbcrRange::ITU_NARROW
        }
    }

    /// Maps ITU colour_primaries to `VkSamplerYcbcrModelConversion`.
    pub fn codec_color_primaries_to_ycbcr_model(
        colour_primaries: u32,
    ) -> vk::SamplerYcbcrModelConversion {
        match colour_primaries {
            1 => vk::SamplerYcbcrModelConversion::YCBCR_709,
            5 | 6 => vk::SamplerYcbcrModelConversion::YCBCR_601,
            9 => vk::SamplerYcbcrModelConversion::YCBCR_2020,
            _ => vk::SamplerYcbcrModelConversion::YCBCR_IDENTITY,
        }
    }

    /// Maps ITU matrix_coefficients to `YcbcrPrimariesConstants`.
    pub fn codec_get_matrix_coefficients(
        matrix_coefficients: u32,
    ) -> YcbcrPrimariesConstants {
        match matrix_coefficients {
            1 => get_ycbcr_primaries_constants(YcbcrBtStandard::Bt709),
            5 | 6 => get_ycbcr_primaries_constants(YcbcrBtStandard::Bt601Ebu),
            7 => get_ycbcr_primaries_constants(YcbcrBtStandard::Bt601Smtpe),
            9 => get_ycbcr_primaries_constants(YcbcrBtStandard::Bt2020),
            _ => YcbcrPrimariesConstants { kb: 1.0, kr: 1.0 },
        }
    }
}

impl PartialEq for VkVideoCoreProfile {
    fn eq(&self, other: &Self) -> bool {
        self.profile_eq(other)
    }
}

impl Eq for VkVideoCoreProfile {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_codec() {
        assert!(VkVideoCoreProfile::is_valid_codec(
            vk::VideoCodecOperationFlagsKHR::DECODE_H264
        ));
        assert!(VkVideoCoreProfile::is_valid_codec(
            vk::VideoCodecOperationFlagsKHR::DECODE_H265
        ));
        assert!(VkVideoCoreProfile::is_valid_codec(
            vk::VideoCodecOperationFlagsKHR::DECODE_AV1
        ));
        assert!(VkVideoCoreProfile::is_valid_codec(
            vk::VideoCodecOperationFlagsKHR::ENCODE_H264
        ));
        assert!(VkVideoCoreProfile::is_valid_codec(
            vk::VideoCodecOperationFlagsKHR::ENCODE_H265
        ));
        assert!(VkVideoCoreProfile::is_valid_codec(CODEC_OP_DECODE_VP9));
        assert!(VkVideoCoreProfile::is_valid_codec(CODEC_OP_ENCODE_AV1));
        assert!(!VkVideoCoreProfile::is_valid_codec(
            vk::VideoCodecOperationFlagsKHR::NONE
        ));
    }

    #[test]
    fn test_is_decode_codec_type() {
        let profile = VkVideoCoreProfile::new(
            vk::VideoCodecOperationFlagsKHR::DECODE_H264,
            vk::VideoChromaSubsamplingFlagsKHR::_420,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN.0 as u32,
            vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
            None,
        );
        assert!(profile.is_decode_codec_type());
        assert!(!profile.is_encode_codec_type());
    }

    #[test]
    fn test_is_encode_codec_type() {
        let profile = VkVideoCoreProfile::new(
            vk::VideoCodecOperationFlagsKHR::ENCODE_H264,
            vk::VideoChromaSubsamplingFlagsKHR::_420,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN.0 as u32,
            vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
            None,
        );
        assert!(profile.is_encode_codec_type());
        assert!(!profile.is_decode_codec_type());
    }

    #[test]
    fn test_profile_equality() {
        let a = VkVideoCoreProfile::new(
            vk::VideoCodecOperationFlagsKHR::DECODE_H264,
            vk::VideoChromaSubsamplingFlagsKHR::_420,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN.0 as u32,
            vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
            None,
        );
        let b = a.clone();
        assert_eq!(a, b);

        let c = VkVideoCoreProfile::new(
            vk::VideoCodecOperationFlagsKHR::DECODE_H265,
            vk::VideoChromaSubsamplingFlagsKHR::_420,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN.0 as u32,
            vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
            None,
        );
        assert_ne!(a, c);
    }

    #[test]
    fn test_profile_inequality_chroma() {
        let a = VkVideoCoreProfile::new(
            vk::VideoCodecOperationFlagsKHR::DECODE_H264,
            vk::VideoChromaSubsamplingFlagsKHR::_420,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN.0 as u32,
            vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
            None,
        );
        let b = VkVideoCoreProfile::new(
            vk::VideoCodecOperationFlagsKHR::DECODE_H264,
            vk::VideoChromaSubsamplingFlagsKHR::_444,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN.0 as u32,
            vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
            None,
        );
        assert_ne!(a, b);
    }

    #[test]
    fn test_get_color_subsampling_generic() {
        let profile = VkVideoCoreProfile::new(
            vk::VideoCodecOperationFlagsKHR::DECODE_H264,
            vk::VideoChromaSubsamplingFlagsKHR::_422,
            vk::VideoComponentBitDepthFlagsKHR::_10,
            vk::VideoComponentBitDepthFlagsKHR::_10,
            vk::video::STD_VIDEO_H264_PROFILE_IDC_HIGH.0 as u32,
            vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
            None,
        );
        assert_eq!(
            profile.get_color_subsampling_generic(),
            StdChromaFormatIdc::Chroma422
        );
    }

    #[test]
    fn test_luma_chroma_bit_depth() {
        let profile = VkVideoCoreProfile::new(
            vk::VideoCodecOperationFlagsKHR::DECODE_H265,
            vk::VideoChromaSubsamplingFlagsKHR::_420,
            vk::VideoComponentBitDepthFlagsKHR::_10,
            vk::VideoComponentBitDepthFlagsKHR::_10,
            vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN_10.0 as u32,
            vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
            None,
        );
        assert_eq!(profile.get_luma_bit_depth_minus8(), 2);
        assert_eq!(profile.get_chroma_bit_depth_minus8(), 2);
        assert!(profile.is_16bit_format());
    }

    #[test]
    fn test_8bit_not_16bit() {
        let profile = VkVideoCoreProfile::new(
            vk::VideoCodecOperationFlagsKHR::DECODE_H264,
            vk::VideoChromaSubsamplingFlagsKHR::_420,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN.0 as u32,
            vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
            None,
        );
        assert!(!profile.is_16bit_format());
    }

    #[test]
    fn test_codec_get_vk_format_420_8bit() {
        assert_eq!(
            VkVideoCoreProfile::codec_get_vk_format(
                vk::VideoChromaSubsamplingFlagsKHR::_420,
                vk::VideoComponentBitDepthFlagsKHR::_8,
                true,
            ),
            vk::Format::G8_B8R8_2PLANE_420_UNORM
        );
        assert_eq!(
            VkVideoCoreProfile::codec_get_vk_format(
                vk::VideoChromaSubsamplingFlagsKHR::_420,
                vk::VideoComponentBitDepthFlagsKHR::_8,
                false,
            ),
            vk::Format::G8_B8_R8_3PLANE_420_UNORM
        );
    }

    #[test]
    fn test_codec_get_vk_format_monochrome() {
        assert_eq!(
            VkVideoCoreProfile::codec_get_vk_format(
                vk::VideoChromaSubsamplingFlagsKHR::MONOCHROME,
                vk::VideoComponentBitDepthFlagsKHR::_8,
                true,
            ),
            vk::Format::R8_UNORM
        );
        assert_eq!(
            VkVideoCoreProfile::codec_get_vk_format(
                vk::VideoChromaSubsamplingFlagsKHR::MONOCHROME,
                vk::VideoComponentBitDepthFlagsKHR::_10,
                false,
            ),
            vk::Format::R10X6_UNORM_PACK16
        );
    }

    #[test]
    fn test_get_video_chroma_format_from_vk_format() {
        assert_eq!(
            VkVideoCoreProfile::get_video_chroma_format_from_vk_format(
                vk::Format::G8_B8R8_2PLANE_420_UNORM
            ),
            StdChromaFormatIdc::Chroma420
        );
        assert_eq!(
            VkVideoCoreProfile::get_video_chroma_format_from_vk_format(vk::Format::R8_UNORM),
            StdChromaFormatIdc::Monochrome
        );
        assert_eq!(
            VkVideoCoreProfile::get_video_chroma_format_from_vk_format(
                vk::Format::G8_B8_R8_3PLANE_444_UNORM
            ),
            StdChromaFormatIdc::Chroma444
        );
        assert_eq!(
            VkVideoCoreProfile::get_video_chroma_format_from_vk_format(
                vk::Format::G8_B8R8_2PLANE_422_UNORM
            ),
            StdChromaFormatIdc::Chroma422
        );
    }

    #[test]
    fn test_codec_to_name() {
        assert_eq!(
            VkVideoCoreProfile::codec_to_name(vk::VideoCodecOperationFlagsKHR::DECODE_H264),
            "decode h.264"
        );
        assert_eq!(
            VkVideoCoreProfile::codec_to_name(vk::VideoCodecOperationFlagsKHR::ENCODE_H265),
            "encode h.265"
        );
        assert_eq!(
            VkVideoCoreProfile::codec_to_name(vk::VideoCodecOperationFlagsKHR::DECODE_AV1),
            "decode av1"
        );
        assert_eq!(
            VkVideoCoreProfile::codec_to_name(CODEC_OP_DECODE_VP9),
            "decode vp9"
        );
    }

    #[test]
    fn test_ycbcr_range() {
        assert_eq!(
            VkVideoCoreProfile::codec_full_range_to_ycbcr_range(true),
            vk::SamplerYcbcrRange::ITU_FULL
        );
        assert_eq!(
            VkVideoCoreProfile::codec_full_range_to_ycbcr_range(false),
            vk::SamplerYcbcrRange::ITU_NARROW
        );
    }

    #[test]
    fn test_ycbcr_model() {
        assert_eq!(
            VkVideoCoreProfile::codec_color_primaries_to_ycbcr_model(1),
            vk::SamplerYcbcrModelConversion::YCBCR_709
        );
        assert_eq!(
            VkVideoCoreProfile::codec_color_primaries_to_ycbcr_model(5),
            vk::SamplerYcbcrModelConversion::YCBCR_601
        );
        assert_eq!(
            VkVideoCoreProfile::codec_color_primaries_to_ycbcr_model(6),
            vk::SamplerYcbcrModelConversion::YCBCR_601
        );
        assert_eq!(
            VkVideoCoreProfile::codec_color_primaries_to_ycbcr_model(9),
            vk::SamplerYcbcrModelConversion::YCBCR_2020
        );
        assert_eq!(
            VkVideoCoreProfile::codec_color_primaries_to_ycbcr_model(99),
            vk::SamplerYcbcrModelConversion::YCBCR_IDENTITY
        );
    }

    #[test]
    fn test_matrix_coefficients() {
        let bt709 = VkVideoCoreProfile::codec_get_matrix_coefficients(1);
        assert!((bt709.kb - 0.0722).abs() < 1e-6);
        assert!((bt709.kr - 0.2126).abs() < 1e-6);

        let bt2020 = VkVideoCoreProfile::codec_get_matrix_coefficients(9);
        assert!((bt2020.kb - 0.0593).abs() < 1e-6);
        assert!((bt2020.kr - 0.2627).abs() < 1e-6);

        let unknown = VkVideoCoreProfile::codec_get_matrix_coefficients(255);
        assert!((unknown.kb - 1.0).abs() < 1e-6);
        assert!((unknown.kr - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_get_codec_specific_profiles() {
        let decode_h264 = VkVideoCoreProfile::new(
            vk::VideoCodecOperationFlagsKHR::DECODE_H264,
            vk::VideoChromaSubsamplingFlagsKHR::_420,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            vk::video::STD_VIDEO_H264_PROFILE_IDC_MAIN.0 as u32,
            vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
            None,
        );
        assert!(decode_h264.get_decode_h264_profile().is_some());
        assert!(decode_h264.get_decode_h265_profile().is_none());
        assert!(decode_h264.get_encode_h264_profile().is_none());

        let encode_h265 = VkVideoCoreProfile::new(
            vk::VideoCodecOperationFlagsKHR::ENCODE_H265,
            vk::VideoChromaSubsamplingFlagsKHR::_420,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            vk::VideoComponentBitDepthFlagsKHR::_8,
            vk::video::STD_VIDEO_H265_PROFILE_IDC_MAIN.0 as u32,
            vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
            None,
        );
        assert!(encode_h265.get_encode_h265_profile().is_some());
        assert!(encode_h265.get_decode_h265_profile().is_none());
    }

    #[test]
    fn test_default_is_invalid() {
        let profile = VkVideoCoreProfile::new_default();
        assert!(!profile.is_valid());
        // get_profile() returns Some because s_type is set, but the profile
        // has NONE codec operation and INVALID subsampling/bit-depth.
        let p = profile.get_profile().unwrap();
        assert_eq!(p.video_codec_operation, vk::VideoCodecOperationFlagsKHR::NONE);
    }

    #[test]
    fn test_clone_and_copy() {
        let original = VkVideoCoreProfile::new(
            vk::VideoCodecOperationFlagsKHR::DECODE_AV1,
            vk::VideoChromaSubsamplingFlagsKHR::_420,
            vk::VideoComponentBitDepthFlagsKHR::_10,
            vk::VideoComponentBitDepthFlagsKHR::_10,
            vk::video::STD_VIDEO_AV1_PROFILE_MAIN.0 as u32,
            vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
            None,
        );
        assert!(original.is_valid());
        assert!(original.get_decode_av1_profile().is_some());

        let cloned = original.clone();
        assert!(cloned.is_valid());
        assert_eq!(original, cloned);
        assert!(cloned.get_decode_av1_profile().is_some());
    }

    #[test]
    fn test_chroma_format_idc_values() {
        // Verify our enum values match the native C constants.
        assert_eq!(
            StdChromaFormatIdc::Monochrome as u32,
            vk::video::STD_VIDEO_H264_CHROMA_FORMAT_IDC_MONOCHROME.0 as u32,
        );
        assert_eq!(
            StdChromaFormatIdc::Chroma420 as u32,
            vk::video::STD_VIDEO_H264_CHROMA_FORMAT_IDC_420.0 as u32,
        );
        assert_eq!(
            StdChromaFormatIdc::Chroma422 as u32,
            vk::video::STD_VIDEO_H264_CHROMA_FORMAT_IDC_422.0 as u32,
        );
        assert_eq!(
            StdChromaFormatIdc::Chroma444 as u32,
            vk::video::STD_VIDEO_H264_CHROMA_FORMAT_IDC_444.0 as u32,
        );
    }

    #[test]
    fn test_codec_get_vk_format_all_subsampling_depths() {
        // 422 / 10-bit
        assert_eq!(
            VkVideoCoreProfile::codec_get_vk_format(
                vk::VideoChromaSubsamplingFlagsKHR::_422,
                vk::VideoComponentBitDepthFlagsKHR::_10,
                true,
            ),
            vk::Format::G10X6_B10X6R10X6_2PLANE_422_UNORM_3PACK16
        );
        // 444 / 12-bit / 3-plane
        assert_eq!(
            VkVideoCoreProfile::codec_get_vk_format(
                vk::VideoChromaSubsamplingFlagsKHR::_444,
                vk::VideoComponentBitDepthFlagsKHR::_12,
                false,
            ),
            vk::Format::G12X4_B12X4_R12X4_3PLANE_444_UNORM_3PACK16
        );
        // monochrome / 12-bit
        assert_eq!(
            VkVideoCoreProfile::codec_get_vk_format(
                vk::VideoChromaSubsamplingFlagsKHR::MONOCHROME,
                vk::VideoComponentBitDepthFlagsKHR::_12,
                true,
            ),
            vk::Format::R12X4_UNORM_PACK16
        );
    }
}
