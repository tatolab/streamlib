// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::Path;

/// Move the project folder `project_directory` to this machine's Trash, so its owner can put it
/// back, answering why when it was not moved.
pub(crate) fn move_the_project_folder_to_the_trash(project_directory: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let moved =
        crate::apple::folder_moved_to_the_trash::move_the_folder_to_the_trash(project_directory);
    #[cfg(target_os = "linux")]
    let moved = crate::linux::folder_moved_to_the_freedesktop_trash::move_the_folder_to_the_trash(
        project_directory,
    );
    if moved.is_ok() {
        tracing::info!(
            project = %project_directory.display(),
            "moved the deleted project's folder to the Trash"
        );
    }
    moved
}
