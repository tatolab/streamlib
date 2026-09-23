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
/// **These are developer-machine fallbacks, never the install experience.** The
/// wheel carries the loader and MoltenVK and points the loader at them (#2362),
/// and a package manager must never appear in anything a user reads.
pub(crate) fn vulkan_loader_library_candidate_paths() -> Vec<std::ffi::OsString> {
    let mut candidate_paths: Vec<std::ffi::OsString> = vec![LIBRARY.into()];
    candidate_paths.extend(apple_vulkan_loader_library_candidate_paths());
    candidate_paths
}

/// The Apple-only tail of the search list: the versioned soname, a LunarG SDK
/// root if one is exported, and the two prefixes dyld does not search itself.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn apple_vulkan_loader_library_candidate_paths() -> Vec<std::ffi::OsString> {
    let mut candidate_paths: Vec<std::ffi::OsString> = vec!["libvulkan.1.dylib".into()];
    if let Some(sdk_root) = std::env::var_os("VULKAN_SDK") {
        let mut sdk_library_path = std::path::PathBuf::from(sdk_root);
        sdk_library_path.push("lib");
        sdk_library_path.push(LIBRARY);
        candidate_paths.push(sdk_library_path.into_os_string());
    }
    candidate_paths.push("/opt/homebrew/lib/libvulkan.dylib".into());
    candidate_paths.push("/usr/local/lib/libvulkan.dylib".into());
    candidate_paths
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
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
    #[cfg(any(target_os = "macos", target_os = "ios"))]
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
