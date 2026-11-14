// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! A folder moved to the user's Trash through `NSFileManager`, so Finder can put it back.

use std::path::Path;

use objc2_foundation::{NSFileManager, NSString, NSURL};

/// Move `folder` to the Trash of the volume it is on.
pub(crate) fn move_the_folder_to_the_trash(folder: &Path) -> Result<(), String> {
    let folder_url = NSURL::fileURLWithPath(&NSString::from_str(&folder.to_string_lossy()));
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&folder_url, None)
        .map_err(|not_trashed| not_trashed.localizedDescription().to_string())
}
