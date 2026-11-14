// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The codec layer's error.

use vulkanalia::vk;

/// Errors that can occur during video context or session creation.
#[derive(Debug)]
pub enum VideoError {
    /// A Vulkan API call returned an error.
    Vulkan(vk::Result),
    /// The requested codec is not supported by the device.
    CodecNotSupported(vk::VideoCodecOperationFlagsKHR),
    /// No suitable queue family found.
    NoVideoQueueFamily,
    /// No suitable memory type found.
    NoSuitableMemoryType,
    /// The parser encountered an invalid bitstream.
    BitstreamError(String),
    /// A required video format is not supported.
    FormatNotSupported(vk::Format),
    /// A resource descriptor the codec was handed cannot describe a resource.
    Configuration(String),
    /// A device-level operation failed: an allocation, a submit, a session.
    Rhi(String),
    /// A stream signals an H.273 `matrix_coefficients` no Vulkan sampler
    /// YCbCr model converts.
    MatrixCoefficientsNotConvertible(u8),
}

impl std::fmt::Display for VideoError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Vulkan(r) => write!(f, "Vulkan error: {:?}", r),
            Self::CodecNotSupported(op) => write!(f, "Codec not supported: {:?}", op),
            Self::NoVideoQueueFamily => write!(f, "No video queue family found"),
            Self::NoSuitableMemoryType => write!(f, "No suitable memory type found"),
            Self::BitstreamError(msg) => write!(f, "Bitstream error: {}", msg),
            Self::FormatNotSupported(fmt) => write!(f, "Format not supported: {:?}", fmt),
            Self::Configuration(msg) => write!(f, "{}", msg),
            Self::Rhi(msg) => write!(f, "{}", msg),
            Self::MatrixCoefficientsNotConvertible(matrix_coefficients) => write!(
                f,
                "the stream signals matrix_coefficients {matrix_coefficients}, which no Vulkan \
                 sampler YCbCr model converts"
            ),
        }
    }
}

impl std::error::Error for VideoError {}

impl From<vk::Result> for VideoError {
    fn from(r: vk::Result) -> Self {
        Self::Vulkan(r)
    }
}

impl From<vk::ErrorCode> for VideoError {
    fn from(e: vk::ErrorCode) -> Self {
        Self::Vulkan(vk::Result::from(e))
    }
}

impl From<streamlib_consumer_rhi::ConsumerRhiError> for VideoError {
    fn from(e: streamlib_consumer_rhi::ConsumerRhiError) -> Self {
        Self::Rhi(e.to_string())
    }
}

/// The codec layer's result.
pub type VideoResult<T> = Result<T, VideoError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_video_error_display() {
        let e = VideoError::Vulkan(vk::Result::ERROR_DEVICE_LOST);
        assert!(format!("{}", e).contains("DEVICE_LOST"));

        let e = VideoError::NoVideoQueueFamily;
        assert!(format!("{}", e).contains("queue family"));
    }
}
