// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CoreVideo's pixel-format dictionary, initialised once before anything
//! that races on it.
//!
//! CoreVideo builds the dictionary lazily and not thread-safely: a pixel
//! buffer pool or texture cache created while `AVCaptureDeviceInput`
//! initialises on another thread crashes inside `_pixelFormatDictionaryInit`.
//! Every path that reaches it — a camera's device input, a VideoToolbox
//! session's pools — runs this first, so the first build happens once, with
//! every other caller blocked behind it.

use std::sync::Once;

use objc2_core_video::CVPixelFormatDescriptionArrayCreateWithAllPixelFormatTypes;

/// Build CoreVideo's pixel-format dictionary if nothing has yet; every later
/// call returns at once.
pub(crate) fn ensure_core_video_pixel_format_dictionary_is_initialised() {
    static INITIALISED: Once = Once::new();
    INITIALISED.call_once(|| {
        let every_pixel_format_description =
            CVPixelFormatDescriptionArrayCreateWithAllPixelFormatTypes(None);
        tracing::debug!(
            initialised = every_pixel_format_description.is_some(),
            "CoreVideo's pixel-format dictionary built"
        );
    });
}
