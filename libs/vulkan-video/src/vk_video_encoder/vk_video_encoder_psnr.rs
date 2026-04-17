// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkVideoEncoderPsnr.h + VkVideoEncoderPsnr.cpp
//!
//! PSNR (Peak Signal-to-Noise Ratio) quality measurement for the video encoder.
//!
//! In the C++ codebase this class owns a staging image pool, captures raw input
//! frames, records DPB-to-staging copies, and computes per-frame and average
//! PSNR across Y, U, V planes.
//!
//! Divergence from C++: The GPU resource management (staging image pool,
//! command buffer recording, memory mapping) is stubbed since it depends on
//! `VulkanDeviceContext`, `VulkanVideoImagePool`, and `VkImageResource` which
//! are not yet ported. The core PSNR computation logic is fully implemented.

use vulkanalia::vk;

// ---------------------------------------------------------------------------
// Per-frame data
// ---------------------------------------------------------------------------

/// Per-frame PSNR data: captured input planes and staging image reference.
///
/// Equivalent to the C++ `VkVideoEncoderPsnr::FrameData`.
#[derive(Debug, Clone, Default)]
pub struct PsnrFrameData {
    pub psnr_input_y: Vec<u8>,
    pub psnr_input_u: Vec<u8>,
    pub psnr_input_v: Vec<u8>,
    // psnr_staging_image: placeholder (GPU resource, deferred)
}

// ---------------------------------------------------------------------------
// VkVideoEncoderPsnr
// ---------------------------------------------------------------------------

/// PSNR computation for video encoder.
///
/// Accumulates per-frame PSNR for the Y, U, and V planes and provides
/// running averages.
///
/// Equivalent to the C++ `VkVideoEncoderPsnr` class.
#[derive(Debug)]
pub struct VkVideoEncoderPsnr {
    psnr_sum_y: f64,
    psnr_sum_u: f64,
    psnr_sum_v: f64,
    psnr_frame_count: u32,
    psnr_recon_y: Vec<u8>,
    psnr_recon_u: Vec<u8>,
    psnr_recon_v: Vec<u8>,
    // GPU resources (deferred)
    image_dpb_format: vk::Format,
    image_extent: vk::Extent2D,
    num_planes: u32,
    encode_width: u32,
    encode_height: u32,
    input_width: u32,
    input_height: u32,
    chroma_420: bool,
}

impl VkVideoEncoderPsnr {
    /// Create a new PSNR calculator.
    ///
    /// Equivalent to the C++ `VkVideoEncoderPsnr::Create`.
    pub fn new() -> Self {
        Self {
            psnr_sum_y: 0.0,
            psnr_sum_u: 0.0,
            psnr_sum_v: 0.0,
            psnr_frame_count: 0,
            psnr_recon_y: Vec::new(),
            psnr_recon_u: Vec::new(),
            psnr_recon_v: Vec::new(),
            image_dpb_format: vk::Format::UNDEFINED,
            image_extent: vk::Extent2D { width: 0, height: 0 },
            num_planes: 3,
            encode_width: 0,
            encode_height: 0,
            input_width: 0,
            input_height: 0,
            chroma_420: true,
        }
    }

    /// Configure the PSNR calculator with encoder parameters.
    ///
    /// Call this once before processing frames.
    pub fn configure(
        &mut self,
        encode_width: u32,
        encode_height: u32,
        input_width: u32,
        input_height: u32,
        num_planes: u32,
        chroma_420: bool,
        image_dpb_format: vk::Format,
    ) {
        self.encode_width = encode_width;
        self.encode_height = encode_height;
        self.input_width = input_width;
        self.input_height = input_height;
        self.num_planes = num_planes;
        self.chroma_420 = chroma_420;
        self.image_dpb_format = image_dpb_format;
        self.image_extent = vk::Extent2D {
            width: encode_width,
            height: encode_height,
        };
        self.psnr_sum_y = 0.0;
        self.psnr_sum_u = 0.0;
        self.psnr_sum_v = 0.0;
        self.psnr_frame_count = 0;
    }

    /// Compute PSNR for a single plane.
    ///
    /// This is the core computation, equivalent to the C++ `computePlanePsnr` lambda.
    ///
    /// `src` and `recon` are plane buffers with the given strides and comparison dimensions.
    /// Returns the PSNR in dB, or -1.0 if computation is not possible.
    pub fn compute_plane_psnr(
        src: &[u8],
        src_stride: usize,
        recon: &[u8],
        recon_stride: usize,
        compare_width: u32,
        compare_height: u32,
    ) -> f64 {
        let n = (compare_width as u64) * (compare_height as u64);
        if n == 0 || src.is_empty() || recon.is_empty() {
            return -1.0;
        }

        let mut sum_sq_diff: u64 = 0;
        for y in 0..compare_height as usize {
            for x in 0..compare_width as usize {
                let src_idx = y * src_stride + x;
                let recon_idx = y * recon_stride + x;
                if src_idx < src.len() && recon_idx < recon.len() {
                    let d = src[src_idx] as i32 - recon[recon_idx] as i32;
                    sum_sq_diff += (d * d) as u64;
                }
            }
        }

        let mse = sum_sq_diff as f64 / n as f64;
        if mse <= 1e-10 {
            100.0
        } else {
            10.0 * (255.0 * 255.0 / mse).log10()
        }
    }

    /// Compute per-frame PSNR given input and reconstruction plane data.
    ///
    /// This is the software-only path for PSNR computation. The GPU-based
    /// capture (CaptureInput/CaptureOutput) is deferred until GPU resources
    /// are ported.
    pub fn compute_frame_psnr(
        &mut self,
        input_y: &[u8],
        input_u: &[u8],
        input_v: &[u8],
        recon_y: &[u8],
        recon_u: &[u8],
        recon_v: &[u8],
    ) {
        let width = self.input_width.min(self.encode_width);
        let height = self.input_height.min(self.encode_height);
        let chroma_w = if self.chroma_420 {
            (width + 1) / 2
        } else {
            width
        };
        let chroma_h = if self.chroma_420 {
            (height + 1) / 2
        } else {
            height
        };

        // Y plane
        let frame_psnr_y = Self::compute_plane_psnr(
            input_y,
            width as usize,
            recon_y,
            self.encode_width as usize,
            width,
            height,
        );
        if frame_psnr_y >= 0.0 {
            self.psnr_sum_y += frame_psnr_y;
        }

        // U plane
        if self.num_planes >= 2 && !input_u.is_empty() && !recon_u.is_empty() {
            let frame_psnr_u = Self::compute_plane_psnr(
                input_u,
                chroma_w as usize,
                recon_u,
                if self.chroma_420 {
                    (self.encode_width as usize + 1) / 2
                } else {
                    self.encode_width as usize
                },
                chroma_w,
                chroma_h,
            );
            if frame_psnr_u >= 0.0 {
                self.psnr_sum_u += frame_psnr_u;
            }
        }

        // V plane
        if self.num_planes >= 3 && !input_v.is_empty() && !recon_v.is_empty() {
            let frame_psnr_v = Self::compute_plane_psnr(
                input_v,
                chroma_w as usize,
                recon_v,
                if self.chroma_420 {
                    (self.encode_width as usize + 1) / 2
                } else {
                    self.encode_width as usize
                },
                chroma_w,
                chroma_h,
            );
            if frame_psnr_v >= 0.0 {
                self.psnr_sum_v += frame_psnr_v;
            }
        }

        self.psnr_frame_count += 1;
    }

    /// Get the average PSNR for the Y plane.
    ///
    /// Equivalent to the C++ `GetAveragePsnrY`.
    pub fn get_average_psnr_y(&self) -> f64 {
        if self.psnr_frame_count > 0 {
            self.psnr_sum_y / self.psnr_frame_count as f64
        } else {
            -1.0
        }
    }

    /// Get the average PSNR for the U plane.
    ///
    /// Equivalent to the C++ `GetAveragePsnrU`.
    pub fn get_average_psnr_u(&self) -> f64 {
        if self.num_planes >= 2 && self.psnr_frame_count > 0 {
            self.psnr_sum_u / self.psnr_frame_count as f64
        } else {
            -1.0
        }
    }

    /// Get the average PSNR for the V plane.
    ///
    /// Equivalent to the C++ `GetAveragePsnrV`.
    pub fn get_average_psnr_v(&self) -> f64 {
        if self.num_planes >= 3 && self.psnr_frame_count > 0 {
            self.psnr_sum_v / self.psnr_frame_count as f64
        } else {
            -1.0
        }
    }

    /// Get the average PSNR (same as Y plane).
    ///
    /// Equivalent to the deprecated C++ `GetAveragePsnr`.
    pub fn get_average_psnr(&self) -> f64 {
        self.get_average_psnr_y()
    }

    /// Reset all state.
    ///
    /// Equivalent to the C++ `Deinit`.
    pub fn deinit(&mut self) {
        self.psnr_recon_y.clear();
        self.psnr_recon_u.clear();
        self.psnr_recon_v.clear();
        self.psnr_sum_y = 0.0;
        self.psnr_sum_u = 0.0;
        self.psnr_sum_v = 0.0;
        self.psnr_frame_count = 0;
    }
}

impl Default for VkVideoEncoderPsnr {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_psnr_identical_frames() {
        // Identical frames should give 100 dB (capped)
        let data = vec![128u8; 16 * 16];
        let psnr = VkVideoEncoderPsnr::compute_plane_psnr(&data, 16, &data, 16, 16, 16);
        assert!((psnr - 100.0).abs() < 1e-6);
    }

    #[test]
    fn test_psnr_different_frames() {
        let src = vec![128u8; 16 * 16];
        let mut recon = vec![128u8; 16 * 16];
        // Introduce some error
        for i in 0..16 {
            recon[i] = 130;
        }
        let psnr = VkVideoEncoderPsnr::compute_plane_psnr(&src, 16, &recon, 16, 16, 16);
        assert!(psnr > 0.0);
        assert!(psnr < 100.0);
    }

    #[test]
    fn test_psnr_empty_data() {
        let psnr = VkVideoEncoderPsnr::compute_plane_psnr(&[], 0, &[], 0, 0, 0);
        assert!((psnr - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn test_average_psnr_no_frames() {
        let psnr = VkVideoEncoderPsnr::new();
        assert_eq!(psnr.get_average_psnr_y(), -1.0);
        assert_eq!(psnr.get_average_psnr_u(), -1.0);
        assert_eq!(psnr.get_average_psnr_v(), -1.0);
    }

    #[test]
    fn test_compute_frame_psnr_accumulation() {
        let mut psnr = VkVideoEncoderPsnr::new();
        psnr.configure(16, 16, 16, 16, 3, true, vk::Format::UNDEFINED);

        let y = vec![128u8; 16 * 16];
        let u = vec![128u8; 8 * 8];
        let v = vec![128u8; 8 * 8];

        psnr.compute_frame_psnr(&y, &u, &v, &y, &u, &v);
        assert!((psnr.get_average_psnr_y() - 100.0).abs() < 1e-6);

        psnr.compute_frame_psnr(&y, &u, &v, &y, &u, &v);
        assert!((psnr.get_average_psnr_y() - 100.0).abs() < 1e-6);
        assert_eq!(psnr.psnr_frame_count, 2);
    }

    #[test]
    fn test_deinit() {
        let mut psnr = VkVideoEncoderPsnr::new();
        psnr.configure(16, 16, 16, 16, 3, true, vk::Format::UNDEFINED);

        let y = vec![128u8; 16 * 16];
        psnr.compute_frame_psnr(&y, &[], &[], &y, &[], &[]);
        assert_eq!(psnr.psnr_frame_count, 1);

        psnr.deinit();
        assert_eq!(psnr.psnr_frame_count, 0);
        assert_eq!(psnr.get_average_psnr_y(), -1.0);
    }

    #[test]
    fn test_known_psnr_value() {
        // Create a frame with known MSE
        // All 128 vs all 138 => diff = 10, MSE = 100, PSNR = 10*log10(65025/100) = ~28.13 dB
        let src = vec![128u8; 16 * 16];
        let recon = vec![138u8; 16 * 16];
        let psnr = VkVideoEncoderPsnr::compute_plane_psnr(&src, 16, &recon, 16, 16, 16);
        let expected = 10.0 * (255.0 * 255.0 / 100.0_f64).log10();
        assert!((psnr - expected).abs() < 0.01);
    }
}
