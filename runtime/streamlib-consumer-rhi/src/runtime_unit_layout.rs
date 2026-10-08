// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Where a runtime unit lays out what it carries: `bin/` beside the lend, and
//! on macOS the bundled Vulkan driver beside `_engine` in the lent
//! `tatolab/runtime/`.
//!
//! Standard library only: `xtask` includes this file by path to lay out the
//! same unit `tatolabd` and the Vulkan loader search find their way around.

use std::path::{Path, PathBuf};

/// The directory holding the runtime unit's binaries, relative to its root.
pub const BINARY_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT: &str = "bin";

/// The lend directory, relative to the runtime unit's root.
pub const LEND_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT: &str = "lib/tatolab/lend";

/// The lent `tatolab.runtime` package, relative to the lend.
pub const LENT_RUNTIME_PACKAGE_RELATIVE_TO_THE_LEND: &str = "tatolab/runtime";

/// The bundled Vulkan driver's directory, beside `_engine` in the runtime
/// package — in the wheel and in the lend alike.
pub const BUNDLED_VULKAN_DRIVER_DIRECTORY_NAME: &str = "_vulkan_driver";

/// The Vulkan loader's versioned soname: the bare name dyld searches for, and
/// the loader the bundled Vulkan driver directory carries.
pub const VERSIONED_VULKAN_LOADER_LIBRARY_FILE_NAME: &str = "libvulkan.1.dylib";

/// MoltenVK, the driver the bundled Vulkan driver directory carries.
pub const BUNDLED_MOLTENVK_LIBRARY_FILE_NAME: &str = "libMoltenVK.dylib";

/// The ICD manifest naming MoltenVK to the Vulkan loader.
pub const BUNDLED_ICD_MANIFEST_FILE_NAME: &str = "MoltenVK_icd.json";

/// Every file `scripts/stage_macos_bundled_vulkan_driver.sh` writes into the
/// bundled Vulkan driver directory.
pub const BUNDLED_VULKAN_DRIVER_FILE_NAMES: [&str; 3] = [
    VERSIONED_VULKAN_LOADER_LIBRARY_FILE_NAME,
    BUNDLED_MOLTENVK_LIBRARY_FILE_NAME,
    BUNDLED_ICD_MANIFEST_FILE_NAME,
];

/// The lend of the runtime unit rooted at `runtime_unit_root`.
pub fn lend_directory_in_the_runtime_unit(runtime_unit_root: &Path) -> PathBuf {
    runtime_unit_root.join(LEND_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT)
}

/// The bundled Vulkan driver directory in the lend at `lend_directory`.
pub fn bundled_vulkan_driver_directory_in_the_lend(lend_directory: &Path) -> PathBuf {
    lend_directory
        .join(LENT_RUNTIME_PACKAGE_RELATIVE_TO_THE_LEND)
        .join(BUNDLED_VULKAN_DRIVER_DIRECTORY_NAME)
}
