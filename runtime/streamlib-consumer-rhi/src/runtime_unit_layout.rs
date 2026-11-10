// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Where a runtime unit lays out what it carries: `bin/` beside the lend, the
//! browser host beside the lend, and on macOS and Linux the bundled Vulkan
//! driver and the Slang shader compiler beside `_engine` in the lent
//! `tatolab/runtime/`. Also where the macOS app bundle lays the same unit out:
//! binaries in `Contents/MacOS`, the lend in `Contents/Resources/lend`, and the
//! lend's Mach-Os in `Contents/Frameworks/tatolab-runtime`.
//!
//! Standard library only: `xtask` includes this file by path to lay out the
//! same unit `tatolabd` and the Vulkan loader search find their way around.

use std::path::{Path, PathBuf};

/// The directory holding the runtime unit's binaries, relative to its root.
pub const BINARY_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT: &str = "bin";

/// The uv the runtime makes every project's `.venv` with, in the runtime
/// unit's `bin/` and the app bundle's `Contents/MacOS` beside `tatolabd`.
pub const RUNTIME_UNIT_UV_EXECUTABLE_FILE_NAME: &str = "uv";

/// The lend directory, relative to the runtime unit's root.
pub const LEND_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT: &str = "lib/tatolab/lend";

/// The directory the browser host and the CEF runtime it loads are staged in,
/// beside the lend.
pub const BROWSER_HOST_DIRECTORY_NAME: &str = "browser-host";

/// The browser host's executable, in its directory.
pub const BROWSER_HOST_EXECUTABLE_FILE_NAME: &str = "tatolab-browser-host";

/// The name a node leases the browser host by, as `tatolab-browser` spells it.
pub const BROWSER_HOST_SHARED_NATIVE_SERVICE_NAME: &str = "browser-host";

/// The lent `tatolab.runtime` package, relative to the lend.
pub const LENT_RUNTIME_PACKAGE_RELATIVE_TO_THE_LEND: &str = "tatolab/runtime";

/// The bundled Vulkan driver's directory, beside `_engine` in the runtime
/// package — in the wheel and in the lend alike.
pub const BUNDLED_VULKAN_DRIVER_DIRECTORY_NAME: &str = "_vulkan_driver";

#[cfg(target_os = "macos")]
mod bundled_vulkan_driver_file_names_on_this_platform {
    /// The Vulkan loader's versioned soname: the bare name dyld searches for,
    /// and the loader the bundled Vulkan driver directory carries.
    pub const VERSIONED_VULKAN_LOADER_LIBRARY_FILE_NAME: &str = "libvulkan.1.dylib";

    /// MoltenVK, the driver the bundled Vulkan driver directory carries.
    pub const BUNDLED_VULKAN_DRIVER_LIBRARY_FILE_NAME: &str = "libMoltenVK.dylib";

    /// The ICD manifest naming MoltenVK to the Vulkan loader.
    pub const BUNDLED_ICD_MANIFEST_FILE_NAME: &str = "MoltenVK_icd.json";
}

#[cfg(target_os = "linux")]
mod bundled_vulkan_driver_file_names_on_this_platform {
    /// The Vulkan loader's versioned soname: the bare name the dynamic linker
    /// searches for, and the loader the bundled Vulkan driver directory carries.
    pub const VERSIONED_VULKAN_LOADER_LIBRARY_FILE_NAME: &str = "libvulkan.so.1";

    /// Mesa's lavapipe, the driver the bundled Vulkan driver directory carries.
    pub const BUNDLED_VULKAN_DRIVER_LIBRARY_FILE_NAME: &str = "libvulkan_lvp.so";

    /// The ICD manifest naming lavapipe to the Vulkan loader, as Mesa names it
    /// on x86_64, the one Linux build.
    pub const BUNDLED_ICD_MANIFEST_FILE_NAME: &str = "lvp_icd.x86_64.json";
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use bundled_vulkan_driver_file_names_on_this_platform::{
    BUNDLED_ICD_MANIFEST_FILE_NAME, BUNDLED_VULKAN_DRIVER_LIBRARY_FILE_NAME,
    VERSIONED_VULKAN_LOADER_LIBRARY_FILE_NAME,
};

/// Every file `scripts/stage_macos_bundled_vulkan_driver.sh` or
/// `scripts/stage_linux_bundled_vulkan_driver.sh` writes into the bundled
/// Vulkan driver directory.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub const BUNDLED_VULKAN_DRIVER_FILE_NAMES: [&str; 3] = [
    VERSIONED_VULKAN_LOADER_LIBRARY_FILE_NAME,
    BUNDLED_VULKAN_DRIVER_LIBRARY_FILE_NAME,
    BUNDLED_ICD_MANIFEST_FILE_NAME,
];

/// The Slang shader compiler's directory, beside `_engine` in the runtime
/// package — in the wheel and in the lend alike.
pub const BUNDLED_SLANG_COMPILER_DIRECTORY_NAME: &str = "_slang_compiler";

/// The Slang release the bundled compiler directory carries.
pub const BUNDLED_SLANG_COMPILER_VERSION: &str =
    streamlib_slang_shader_compiler::SLANG_COMPILER_RELEASE;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use streamlib_slang_shader_compiler::{
    SLANG_COMPILER_LIBRARY_FILE_NAME, SLANG_SPIRV_OPTIMISER_LIBRARY_FILE_NAME,
};

/// Every file `scripts/stage_slang_compiler_libraries.sh` writes into the
/// bundled Slang compiler directory.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub const BUNDLED_SLANG_COMPILER_FILE_NAMES: [&str; 2] = [
    SLANG_COMPILER_LIBRARY_FILE_NAME,
    SLANG_SPIRV_OPTIMISER_LIBRARY_FILE_NAME,
];

/// The directory name of a macOS app bundle's `Contents/`.
pub const APP_BUNDLE_CONTENTS_DIRECTORY_NAME: &str = "Contents";

/// The directory name of a macOS app bundle's executables, inside `Contents/`.
pub const APP_BUNDLE_EXECUTABLE_DIRECTORY_NAME: &str = "MacOS";

/// The lend directory, relative to a macOS app bundle's `Contents/`.
pub const LEND_DIRECTORY_RELATIVE_TO_THE_APP_BUNDLE_CONTENTS: &str = "Resources/lend";

/// The directory holding every Mach-O of the lent `tatolab/runtime/`, at its
/// path below that package, relative to a macOS app bundle's `Contents/`.
/// Code signing admits no Mach-O under `Resources/`, so the lend links to these.
pub const LENT_RUNTIME_PACKAGE_MACH_O_DIRECTORY_RELATIVE_TO_THE_APP_BUNDLE_CONTENTS: &str =
    "Frameworks/tatolab-runtime";

/// The lend of the runtime unit rooted at `runtime_unit_root`.
pub fn lend_directory_in_the_runtime_unit(runtime_unit_root: &Path) -> PathBuf {
    runtime_unit_root.join(LEND_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT)
}

/// The browser host's app bundle in its directory on macOS, where CEF finds
/// its framework and helpers inside the bundle.
pub const BROWSER_HOST_APP_BUNDLE_DIRECTORY_NAME: &str = "tatolab-browser-host.app";

/// The browser host's directory beside the lend at `lend_directory`, or `None`
/// for a lend at the filesystem root.
pub fn browser_host_directory_beside_the_lend(lend_directory: &Path) -> Option<PathBuf> {
    Some(lend_directory.parent()?.join(BROWSER_HOST_DIRECTORY_NAME))
}

/// The directory beside the lend that holds the first-party packages the
/// runtime unit carries, `tatolab-stream` and the core modules, one wheel
/// each, installed into every project's `.venv`.
pub const FIRST_PARTY_MODULES_DIRECTORY_NAME: &str = "imports";

/// The first-party modules directory beside the lend at `lend_directory`, or
/// `None` for a lend at the filesystem root.
pub fn first_party_modules_directory_beside_the_lend(lend_directory: &Path) -> Option<PathBuf> {
    Some(
        lend_directory
            .parent()?
            .join(FIRST_PARTY_MODULES_DIRECTORY_NAME),
    )
}

/// The directory beside the lend that holds the third-party wheels the core
/// modules require, for the unit's own platform, which every project's sync
/// finds through its `.tatolab/carried-wheels` link before any index.
pub const CARRIED_WHEELS_DIRECTORY_NAME: &str = "carried-wheels";

/// The carried wheels directory beside the lend at `lend_directory`, or
/// `None` for a lend at the filesystem root.
pub fn carried_wheels_directory_beside_the_lend(lend_directory: &Path) -> Option<PathBuf> {
    Some(lend_directory.parent()?.join(CARRIED_WHEELS_DIRECTORY_NAME))
}

/// The directory beside the lend that holds the CPython build the unit carries,
/// at `<build>/<archive file name>`: the layout uv reads from a
/// `UV_PYTHON_INSTALL_MIRROR`.
pub const CARRIED_PYTHON_DOWNLOADS_DIRECTORY_NAME: &str = "python-downloads";

/// The carried Python downloads directory beside the lend at `lend_directory`,
/// or `None` for a lend at the filesystem root.
pub fn carried_python_downloads_directory_beside_the_lend(
    lend_directory: &Path,
) -> Option<PathBuf> {
    Some(
        lend_directory
            .parent()?
            .join(CARRIED_PYTHON_DOWNLOADS_DIRECTORY_NAME),
    )
}

/// The browser host's executable beside the lend at `lend_directory`, or `None`
/// for a lend at the filesystem root: in its directory on Linux, inside its
/// app bundle there on macOS.
pub fn browser_host_executable_beside_the_lend(lend_directory: &Path) -> Option<PathBuf> {
    let browser_host_directory = browser_host_directory_beside_the_lend(lend_directory)?;
    if cfg!(target_os = "macos") {
        return Some(
            browser_host_directory
                .join(BROWSER_HOST_APP_BUNDLE_DIRECTORY_NAME)
                .join("Contents/MacOS")
                .join(BROWSER_HOST_EXECUTABLE_FILE_NAME),
        );
    }
    Some(browser_host_directory.join(BROWSER_HOST_EXECUTABLE_FILE_NAME))
}

/// The bundled Vulkan driver directory in the lend at `lend_directory`.
pub fn bundled_vulkan_driver_directory_in_the_lend(lend_directory: &Path) -> PathBuf {
    lend_directory
        .join(LENT_RUNTIME_PACKAGE_RELATIVE_TO_THE_LEND)
        .join(BUNDLED_VULKAN_DRIVER_DIRECTORY_NAME)
}

/// The bundled Slang compiler directory in the lend at `lend_directory`.
pub fn bundled_slang_compiler_directory_in_the_lend(lend_directory: &Path) -> PathBuf {
    lend_directory
        .join(LENT_RUNTIME_PACKAGE_RELATIVE_TO_THE_LEND)
        .join(BUNDLED_SLANG_COMPILER_DIRECTORY_NAME)
}

/// The app bundle's `Contents/` when `executable_directory` is its
/// `Contents/MacOS`, else `None`.
pub fn app_bundle_contents_directory_holding_the_executable_directory(
    executable_directory: &Path,
) -> Option<&Path> {
    if executable_directory.file_name()? != APP_BUNDLE_EXECUTABLE_DIRECTORY_NAME {
        return None;
    }
    let app_bundle_contents_directory = executable_directory.parent()?;
    (app_bundle_contents_directory.file_name()? == APP_BUNDLE_CONTENTS_DIRECTORY_NAME)
        .then_some(app_bundle_contents_directory)
}

/// The lend of the app bundle whose `Contents/` is `app_bundle_contents_directory`.
pub fn lend_directory_in_the_app_bundle_contents(app_bundle_contents_directory: &Path) -> PathBuf {
    app_bundle_contents_directory.join(LEND_DIRECTORY_RELATIVE_TO_THE_APP_BUNDLE_CONTENTS)
}

/// The bundled Vulkan driver directory among the lent runtime package's
/// Mach-Os in the app bundle whose `Contents/` is
/// `app_bundle_contents_directory`.
pub fn bundled_vulkan_driver_directory_in_the_app_bundle_contents(
    app_bundle_contents_directory: &Path,
) -> PathBuf {
    app_bundle_contents_directory
        .join(LENT_RUNTIME_PACKAGE_MACH_O_DIRECTORY_RELATIVE_TO_THE_APP_BUNDLE_CONTENTS)
        .join(BUNDLED_VULKAN_DRIVER_DIRECTORY_NAME)
}

/// The bundled Slang compiler directory among the lent runtime package's
/// Mach-Os in the app bundle whose `Contents/` is
/// `app_bundle_contents_directory`.
pub fn bundled_slang_compiler_directory_in_the_app_bundle_contents(
    app_bundle_contents_directory: &Path,
) -> PathBuf {
    app_bundle_contents_directory
        .join(LENT_RUNTIME_PACKAGE_MACH_O_DIRECTORY_RELATIVE_TO_THE_APP_BUNDLE_CONTENTS)
        .join(BUNDLED_SLANG_COMPILER_DIRECTORY_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn the_macos_unit_carries_the_loader_moltenvk_and_its_manifest() {
        assert_eq!(
            BUNDLED_VULKAN_DRIVER_FILE_NAMES,
            [
                "libvulkan.1.dylib",
                "libMoltenVK.dylib",
                "MoltenVK_icd.json"
            ]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_linux_unit_carries_the_loader_lavapipe_and_its_manifest() {
        assert_eq!(
            BUNDLED_VULKAN_DRIVER_FILE_NAMES,
            ["libvulkan.so.1", "libvulkan_lvp.so", "lvp_icd.x86_64.json"]
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_bundled_optimiser_file_name_names_the_bundled_slang_version() {
        assert!(
            SLANG_SPIRV_OPTIMISER_LIBRARY_FILE_NAME
                .contains(&format!("-{BUNDLED_SLANG_COMPILER_VERSION}.")),
            "Slang loads its optimiser by a versioned name, so the two must move together"
        );
    }

    #[test]
    fn the_slang_compiler_sits_beside_the_vulkan_driver_in_the_lend() {
        let lend_directory = lend_directory_in_the_runtime_unit(Path::new("/opt/tatolab"));
        assert_eq!(
            bundled_slang_compiler_directory_in_the_lend(&lend_directory).parent(),
            bundled_vulkan_driver_directory_in_the_lend(&lend_directory).parent()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_browser_host_sits_in_its_own_directory_beside_the_lend() {
        assert_eq!(
            browser_host_executable_beside_the_lend(&lend_directory_in_the_runtime_unit(
                Path::new("/opt/tatolab")
            )),
            Some(PathBuf::from(
                "/opt/tatolab/lib/tatolab/browser-host/tatolab-browser-host"
            ))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_browser_host_sits_in_its_app_bundle_beside_the_lend() {
        assert_eq!(
            browser_host_executable_beside_the_lend(&lend_directory_in_the_runtime_unit(
                Path::new("/opt/tatolab")
            )),
            Some(PathBuf::from(
                "/opt/tatolab/lib/tatolab/browser-host/tatolab-browser-host.app/Contents/MacOS/\
                 tatolab-browser-host"
            ))
        );
    }
}
