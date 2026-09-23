// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Where the Vulkan loader library is looked for — one list for the engine's
//! device and a helper process's consumer device.

use vulkanalia::loader::{LIBRARY, LibloadingLoader};

/// Dynamic libraries the Vulkan loader may live in, in the order they are tried.
///
/// vulkanalia's own [`LIBRARY`] name is first on every platform, so a host that
/// already resolves it loads exactly that. Apple needs the rest: dyld's default
/// search path does not include Homebrew's prefix on Apple Silicon, so a bare
/// `libvulkan.dylib` resolves nothing on a stock machine even with the loader
/// installed. `VULKAN_SDK` is the LunarG SDK's own variable, read to honour that
/// convention rather than as a StreamLib dial — there is no setting for which
/// loader to use, and the order is fixed.
///
/// The macOS wheel carries its own loader, which is what a stock machine opens;
/// the Homebrew prefixes after it are developer-machine fallbacks, and a package
/// manager must never appear in anything a user reads.
pub(crate) fn vulkan_loader_library_candidate_paths() -> Vec<std::ffi::OsString> {
    let mut candidate_paths: Vec<std::ffi::OsString> = vec![LIBRARY.into()];
    candidate_paths.extend(apple_vulkan_loader_library_candidate_paths());
    candidate_paths
}

/// Where the macOS wheel stages the loader, MoltenVK and its ICD manifest,
/// relative to the directory of the image this crate is linked into — the
/// wheel's `_engine` extension. `scripts/stage_macos_bundled_vulkan_driver.sh`
/// writes it and `streamlib/__init__.py` points the loader at the manifest.
#[cfg(target_os = "macos")]
const BUNDLED_VULKAN_DRIVER_DIRECTORY_NAME: &str = "_vulkan_driver";

/// The Apple-only tail of the search list: the versioned soname, a LunarG SDK
/// root if one is exported, the loader the wheel carries, and the two prefixes
/// dyld does not search itself.
#[cfg(target_os = "macos")]
fn apple_vulkan_loader_library_candidate_paths() -> Vec<std::ffi::OsString> {
    let mut candidate_paths: Vec<std::ffi::OsString> = vec!["libvulkan.1.dylib".into()];
    if let Some(sdk_root) = std::env::var_os("VULKAN_SDK") {
        let mut sdk_library_path = std::path::PathBuf::from(sdk_root);
        sdk_library_path.push("lib");
        sdk_library_path.push(LIBRARY);
        candidate_paths.push(sdk_library_path.into_os_string());
    }
    if let Some(bundled_loader_path) = vulkan_loader_library_bundled_beside_this_image() {
        candidate_paths.push(bundled_loader_path.into_os_string());
    }
    candidate_paths.push("/opt/homebrew/lib/libvulkan.dylib".into());
    candidate_paths.push("/usr/local/lib/libvulkan.dylib".into());
    candidate_paths
}

/// The loader the wheel carries, beside the image this code was linked into.
///
/// A path with a slash, so dlopen takes it verbatim and no `@rpath` or install
/// name is consulted. `None` only if dyld cannot name the image.
#[cfg(target_os = "macos")]
fn vulkan_loader_library_bundled_beside_this_image() -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;

    // SAFETY: `Dl_info` is plain C data; all-zero is a valid value to overwrite.
    let mut image_containing_this_function: libc::Dl_info = unsafe { std::mem::zeroed() };
    // SAFETY: `dladdr` only reads the address it is given and writes the struct
    // it is handed, which outlives the call.
    let found = unsafe {
        libc::dladdr(
            vulkan_loader_library_bundled_beside_this_image as *const libc::c_void,
            &mut image_containing_this_function,
        )
    };
    if found == 0 || image_containing_this_function.dli_fname.is_null() {
        return None;
    }
    // SAFETY: `dli_fname` is a NUL-terminated path dyld owns for as long as the
    // image stays loaded, which is at least as long as this function exists.
    let image_path_bytes =
        unsafe { std::ffi::CStr::from_ptr(image_containing_this_function.dli_fname) }.to_bytes();
    let image_path = std::path::Path::new(std::ffi::OsStr::from_bytes(image_path_bytes));
    Some(
        image_path
            .parent()?
            .join(BUNDLED_VULKAN_DRIVER_DIRECTORY_NAME)
            .join("libvulkan.1.dylib"),
    )
}

#[cfg(not(target_os = "macos"))]
fn apple_vulkan_loader_library_candidate_paths() -> Vec<std::ffi::OsString> {
    Vec::new()
}

/// No Vulkan loader library opened: why each candidate on the search list
/// refused, one `path: reason` line apiece.
#[derive(Debug)]
pub struct VulkanLoaderLibraryNotFound {
    refusal_per_candidate: Vec<String>,
}

impl std::fmt::Display for VulkanLoaderLibraryNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "  {}", self.refusal_per_candidate.join("\n  "))
    }
}

/// Open the first Vulkan loader library that dlopens, or say where it looked.
pub fn open_the_first_vulkan_loader_library_that_opens()
-> std::result::Result<LibloadingLoader, VulkanLoaderLibraryNotFound> {
    let mut refusal_per_candidate: Vec<String> = Vec::new();
    for candidate_path in vulkan_loader_library_candidate_paths() {
        // SAFETY: loading the Vulkan loader runs only its own initialisers.
        match unsafe { LibloadingLoader::new(&candidate_path) } {
            Ok(loader) => {
                tracing::info!(
                    vulkan_loader_library = %candidate_path.to_string_lossy(),
                    "Vulkan loader library opened"
                );
                return Ok(loader);
            }
            Err(open_failure) => refusal_per_candidate.push(format!(
                "{}: {open_failure}",
                candidate_path.to_string_lossy()
            )),
        }
    }
    Err(VulkanLoaderLibraryNotFound {
        refusal_per_candidate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first candidate is vulkanalia's own platform name, so a host that
    /// already resolves it is unaffected by the search list existing.
    #[test]
    fn the_vulkan_loader_search_list_starts_at_the_platform_default_name() {
        let candidate_paths = vulkan_loader_library_candidate_paths();

        assert_eq!(
            candidate_paths.first().map(|path| path.as_os_str()),
            Some(std::ffi::OsStr::new(LIBRARY)),
            "the platform default must stay first so an already-resolving host is unchanged"
        );
    }

    /// Apple needs more than the bare name: dyld's default search path excludes
    /// Homebrew's prefix on Apple Silicon, so `libvulkan.dylib` alone resolves
    /// nothing on a stock machine with the loader installed.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_vulkan_loader_search_list_reaches_a_stock_homebrew_install() {
        let candidate_paths: Vec<String> = vulkan_loader_library_candidate_paths()
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();

        assert!(
            candidate_paths
                .iter()
                .any(|path| path == "/opt/homebrew/lib/libvulkan.dylib"),
            "a Homebrew install must be reachable: {candidate_paths:?}"
        );
    }

    /// A stock Mac has no loader of its own, so the one the wheel carries is
    /// tried before any developer-machine prefix could shadow it.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_loader_the_wheel_carries_is_tried_before_any_homebrew_prefix() {
        let candidate_paths: Vec<String> = vulkan_loader_library_candidate_paths()
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        let position_of =
            |wanted: &dyn Fn(&str) -> bool| candidate_paths.iter().position(|path| wanted(path));

        let bundled_loader_position = position_of(&|path| {
            path.ends_with(&format!(
                "/{BUNDLED_VULKAN_DRIVER_DIRECTORY_NAME}/libvulkan.1.dylib"
            ))
        })
        .unwrap_or_else(|| panic!("the wheel's own loader is not searched: {candidate_paths:?}"));
        let first_homebrew_position = position_of(&|path| path.starts_with("/opt/homebrew/"))
            .unwrap_or_else(|| panic!("no Homebrew prefix is searched: {candidate_paths:?}"));

        assert!(
            bundled_loader_position < first_homebrew_position,
            "the wheel's loader must come before Homebrew's: {candidate_paths:?}"
        );
    }

    /// The bundled loader is looked for beside the image this crate is linked
    /// into — for the wheel, `_engine`'s own directory.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_bundled_loader_is_looked_for_beside_the_image_this_code_is_in() {
        let bundled_loader_path = vulkan_loader_library_bundled_beside_this_image()
            .expect("dyld names the image a function of this test binary is in");
        let running_test_binary = std::env::current_exe().expect("the test binary has a path");

        assert_eq!(
            bundled_loader_path
                .parent()
                .and_then(std::path::Path::parent)
                .map(std::fs::canonicalize)
                .transpose()
                .expect("the image's directory exists"),
            running_test_binary
                .parent()
                .map(std::fs::canonicalize)
                .transpose()
                .expect("exists"),
            "the loader must be looked for in the directory of the image this code is in"
        );
    }

    /// Linux keeps exactly one candidate — the search list must not change what
    /// a Linux host loads.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_vulkan_loader_search_list_is_unchanged_on_linux() {
        assert_eq!(
            vulkan_loader_library_candidate_paths().len(),
            1,
            "Linux loads the platform default and nothing else"
        );
    }
}
