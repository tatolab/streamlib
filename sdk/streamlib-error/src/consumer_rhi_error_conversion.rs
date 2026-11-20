// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::Error;

impl From<streamlib_consumer_rhi::ConsumerRhiError> for Error {
    fn from(e: streamlib_consumer_rhi::ConsumerRhiError) -> Self {
        match e {
            streamlib_consumer_rhi::ConsumerRhiError::Gpu(s)
            | streamlib_consumer_rhi::ConsumerRhiError::GpuOutOfResourcesForNow(s) => {
                Error::GpuError(s)
            }
            streamlib_consumer_rhi::ConsumerRhiError::Configuration(s) => Error::Configuration(s),
        }
    }
}
