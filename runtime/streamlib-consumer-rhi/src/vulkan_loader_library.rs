// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Where the Vulkan loader library is looked for — one list for the engine's
//! device and a helper process's consumer device.

#[cfg(any(target_os = "macos", target_os = "linux"))]
use crate::runtime_unit_layout::{
    BUNDLED_VULKAN_DRIVER_DIRECTORY_NAME, VERSIONED_VULKAN_LOADER_LIBRARY_FILE_NAME,
    bundled_vulkan_driver_directory_in_the_lend, lend_directory_in_the_runtime_unit,
};
#[cfg(target_os = "macos")]
use crate::runtime_unit_layout::{
    app_bundle_contents_directory_holding_the_executable_directory,
    bundled_vulkan_driver_directory_in_the_app_bundle_contents,
};
use vulkanalia::loader::{LIBRARY, LibloadingLoader};

/// Dynamic libraries the Vulkan loader may live in, in the order they are tried.
///
/// vulkanalia's own [`LIBRARY`] name is first on every platform, so a host that
/// already resolves it loads exactly that. Apple needs more: dyld's default
/// search path does not include Homebrew's prefix on Apple Silicon, so a bare
/// `libvulkan.dylib` resolves nothing on a stock machine even with the loader
/// installed. `VULKAN_SDK` is the LunarG SDK's own variable, read to honour that
/// convention rather than as a StreamLib dial — there is no setting for which
/// loader to use, and the order is fixed.
///
/// The runtime unit carries its own loader on macOS and Linux, which is what a
/// machine without one opens: beside the wheel's `_engine`, in the lend a
/// binary linking the engine finds relative to its own directory, or, on
/// macOS, in the `Frameworks/` of the app bundle whose `Contents/MacOS` holds
/// that binary. The Homebrew prefixes after it are macOS developer-machine
/// fallbacks, and a package manager must never appear in anything a user reads.
pub(crate) fn vulkan_loader_library_candidate_paths() -> Vec<std::ffi::OsString> {
    let mut candidate_paths: Vec<std::ffi::OsString> = vec![LIBRARY.into()];
    candidate_paths.extend(platform_vulkan_loader_library_candidate_paths());
    candidate_paths
}

#[cfg(target_os = "macos")]
fn platform_vulkan_loader_library_candidate_paths() -> Vec<std::ffi::OsString> {
    macos_vulkan_loader_library_candidate_paths_for_the_image_in(
        directory_of_the_image_this_code_is_in().as_deref(),
    )
}

#[cfg(target_os = "linux")]
fn platform_vulkan_loader_library_candidate_paths() -> Vec<std::ffi::OsString> {
    linux_vulkan_loader_library_candidate_paths_for_the_image_in(
        directory_of_the_image_this_code_is_in().as_deref(),
    )
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_vulkan_loader_library_candidate_paths() -> Vec<std::ffi::OsString> {
    Vec::new()
}

/// The macOS-only tail of the search list, for code in an image whose
/// directory is `directory_of_the_image`: the versioned soname, a LunarG SDK
/// root if one is exported, the loader the wheel carries, the loader the lend
/// beside a binary's directory carries, the loader the app bundle holding the
/// image carries, and the two prefixes dyld does not search itself.
#[cfg(target_os = "macos")]
fn macos_vulkan_loader_library_candidate_paths_for_the_image_in(
    directory_of_the_image: Option<&std::path::Path>,
) -> Vec<std::ffi::OsString> {
    let mut candidate_paths: Vec<std::ffi::OsString> =
        vec![VERSIONED_VULKAN_LOADER_LIBRARY_FILE_NAME.into()];
    if let Some(sdk_root) = std::env::var_os("VULKAN_SDK") {
        let mut sdk_library_path = std::path::PathBuf::from(sdk_root);
        sdk_library_path.push("lib");
        sdk_library_path.push(LIBRARY);
        candidate_paths.push(sdk_library_path.into_os_string());
    }
    if let Some(directory_of_the_image) = directory_of_the_image {
        candidate_paths.extend(
            vulkan_loader_libraries_the_runtime_unit_carries_for_the_image_in(
                directory_of_the_image,
            ),
        );
        if let Some(app_bundle_loader_path) =
            vulkan_loader_library_in_the_app_bundle_holding_the_image_directory(
                directory_of_the_image,
            )
        {
            candidate_paths.push(app_bundle_loader_path.into_os_string());
        }
    }
    candidate_paths.push("/opt/homebrew/lib/libvulkan.dylib".into());
    candidate_paths.push("/usr/local/lib/libvulkan.dylib".into());
    candidate_paths
}

/// The Linux-only tail of the search list, for code in an image whose
/// directory is `directory_of_the_image`: the loader the wheel carries, then
/// the loader the lend beside a binary's directory carries. The bare name
/// before it is already the versioned soname, and the dynamic linker's own
/// search covers every prefix a distribution installs a loader to.
#[cfg(target_os = "linux")]
fn linux_vulkan_loader_library_candidate_paths_for_the_image_in(
    directory_of_the_image: Option<&std::path::Path>,
) -> Vec<std::ffi::OsString> {
    directory_of_the_image
        .map(vulkan_loader_libraries_the_runtime_unit_carries_for_the_image_in)
        .unwrap_or_default()
}

/// The loader beside the image whose directory is `directory_of_the_image`,
/// then the loader the lend beside that directory carries.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn vulkan_loader_libraries_the_runtime_unit_carries_for_the_image_in(
    directory_of_the_image: &std::path::Path,
) -> Vec<std::ffi::OsString> {
    let mut candidate_paths: Vec<std::ffi::OsString> =
        vec![vulkan_loader_library_bundled_beside_the_image_in(directory_of_the_image).into()];
    if let Some(lent_loader_path) =
        vulkan_loader_library_in_the_lend_beside_the_binary_directory(directory_of_the_image)
    {
        candidate_paths.push(lent_loader_path.into_os_string());
    }
    candidate_paths
}

/// The loader the wheel carries, beside the image whose directory is
/// `directory_of_the_image`.
///
/// A path with a slash, so dlopen takes it verbatim and no `@rpath`, install
/// name or library search path is consulted.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn vulkan_loader_library_bundled_beside_the_image_in(
    directory_of_the_image: &std::path::Path,
) -> std::path::PathBuf {
    directory_of_the_image
        .join(BUNDLED_VULKAN_DRIVER_DIRECTORY_NAME)
        .join(VERSIONED_VULKAN_LOADER_LIBRARY_FILE_NAME)
}

/// The loader the lend carries, for an image in a runtime unit's `bin/` — the
/// runtime unit `tatolabd` finds its lend in. `None` for an image at the
/// filesystem root.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn vulkan_loader_library_in_the_lend_beside_the_binary_directory(
    directory_of_the_image: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let runtime_unit_root = directory_of_the_image.parent()?;
    Some(
        bundled_vulkan_driver_directory_in_the_lend(&lend_directory_in_the_runtime_unit(
            runtime_unit_root,
        ))
        .join(VERSIONED_VULKAN_LOADER_LIBRARY_FILE_NAME),
    )
}

/// The loader the macOS app bundle carries among its `Frameworks/`, for an
/// image in that bundle's `Contents/MacOS` — the bundle `tatolabd` finds its
/// lend in. `None` for an image anywhere else.
#[cfg(target_os = "macos")]
fn vulkan_loader_library_in_the_app_bundle_holding_the_image_directory(
    directory_of_the_image: &std::path::Path,
) -> Option<std::path::PathBuf> {
    app_bundle_contents_directory_holding_the_executable_directory(directory_of_the_image).map(
        |app_bundle_contents_directory| {
            bundled_vulkan_driver_directory_in_the_app_bundle_contents(
                app_bundle_contents_directory,
            )
            .join(VERSIONED_VULKAN_LOADER_LIBRARY_FILE_NAME)
        },
    )
}

/// The canonical directory of the image this code was linked into — the
/// wheel's `_engine`, or a binary that links the engine statically. `None`
/// only if the dynamic linker cannot name the image.
///
/// Canonical because `tatolabd` finds its lend from its own canonical path: a
/// `tatolabd` started through a symlink must find the lent loader in the same
/// runtime unit. A path that cannot be canonicalized is taken as given.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn directory_of_the_image_this_code_is_in() -> Option<std::path::PathBuf> {
    let image_path = path_of_the_image_this_code_is_in()?;
    let canonical_image_path =
        std::fs::canonicalize(&image_path).unwrap_or_else(|_| image_path.clone());
    canonical_image_path
        .parent()
        .map(std::path::Path::to_path_buf)
}

/// The path dyld names the image containing this function by.
#[cfg(target_os = "macos")]
fn path_of_the_image_this_code_is_in() -> Option<std::path::PathBuf> {
    image_containing_the_address(path_of_the_image_this_code_is_in as *const libc::c_void)
        .map(|image| image.path_as_the_dynamic_linker_names_it)
}

/// The path of the image containing this function: the one the dynamic
/// linker opened it by, or `/proc/self/exe` for the main executable.
///
/// glibc's `dladdr` names the main executable by its `argv[0]`, which may be
/// relative to a directory the process has since left, or a bare name `PATH`
/// resolved; only the kernel's own link names the file.
#[cfg(target_os = "linux")]
fn path_of_the_image_this_code_is_in() -> Option<std::path::PathBuf> {
    let image =
        image_containing_the_address(path_of_the_image_this_code_is_in as *const libc::c_void)?;
    if load_base_of_the_main_executable() == Some(image.load_base) {
        return std::env::current_exe().ok();
    }
    Some(image.path_as_the_dynamic_linker_names_it)
}

/// Where the main executable is mapped, found through the program headers the
/// kernel names in every process's auxiliary vector.
#[cfg(target_os = "linux")]
fn load_base_of_the_main_executable() -> Option<usize> {
    // SAFETY: `getauxval` only reads the process's auxiliary vector.
    let main_executable_program_headers = unsafe { libc::getauxval(libc::AT_PHDR) };
    if main_executable_program_headers == 0 {
        return None;
    }
    image_containing_the_address(main_executable_program_headers as *const libc::c_void)
        .map(|image| image.load_base)
}

/// The loaded image `dladdr` reports as containing an address.
#[cfg(any(target_os = "macos", target_os = "linux"))]
struct LoadedImageContainingAnAddress {
    path_as_the_dynamic_linker_names_it: std::path::PathBuf,
    #[cfg(target_os = "linux")]
    load_base: usize,
}

/// The loaded image containing `address`, or `None` if `dladdr` names none.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn image_containing_the_address(
    address: *const libc::c_void,
) -> Option<LoadedImageContainingAnAddress> {
    use std::os::unix::ffi::OsStrExt;

    // SAFETY: `Dl_info` is plain C data; all-zero is a valid value to overwrite.
    let mut image_containing_the_address: libc::Dl_info = unsafe { std::mem::zeroed() };
    // SAFETY: `dladdr` only reads the address it is given and writes the struct
    // it is handed, which outlives the call.
    let dladdr_named_the_image_containing_the_address =
        unsafe { libc::dladdr(address, &mut image_containing_the_address) };
    if dladdr_named_the_image_containing_the_address == 0
        || image_containing_the_address.dli_fname.is_null()
    {
        return None;
    }
    // SAFETY: `dli_fname` is a NUL-terminated path the dynamic linker owns for
    // as long as the image stays loaded; it is copied before this returns.
    let image_path_bytes =
        unsafe { std::ffi::CStr::from_ptr(image_containing_the_address.dli_fname) }.to_bytes();
    Some(LoadedImageContainingAnAddress {
        path_as_the_dynamic_linker_names_it: std::path::PathBuf::from(std::ffi::OsStr::from_bytes(
            image_path_bytes,
        )),
        #[cfg(target_os = "linux")]
        load_base: image_containing_the_address.dli_fbase as usize,
    })
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
    /// tried before the Homebrew prefixes listed explicitly. A bare soname dyld
    /// resolves, or an exported `VULKAN_SDK`, still comes first.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_loader_the_wheel_carries_is_tried_before_any_homebrew_prefix() {
        let candidate_paths: Vec<String> = vulkan_loader_library_candidate_paths()
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        let bundled_loader_suffix = format!(
            "/{BUNDLED_VULKAN_DRIVER_DIRECTORY_NAME}/{VERSIONED_VULKAN_LOADER_LIBRARY_FILE_NAME}"
        );

        let bundled_loader_position = candidate_paths
            .iter()
            .position(|path| path.ends_with(&bundled_loader_suffix))
            .unwrap_or_else(|| {
                panic!("the wheel's own loader is not searched: {candidate_paths:?}")
            });
        let first_homebrew_position = candidate_paths
            .iter()
            .position(|path| path.starts_with("/opt/homebrew/"))
            .unwrap_or_else(|| panic!("no Homebrew prefix is searched: {candidate_paths:?}"));

        assert!(
            bundled_loader_position < first_homebrew_position,
            "the wheel's loader must come before Homebrew's: {candidate_paths:?}"
        );
    }

    /// The bundled loader is looked for beside the image this crate is linked
    /// into — for the wheel, `_engine`'s own directory.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_bundled_loader_is_looked_for_beside_the_image_this_code_is_in() {
        let directory_of_this_image = directory_of_the_image_this_code_is_in()
            .expect("the dynamic linker names the image a function of this test binary is in");
        let bundled_loader_path =
            vulkan_loader_library_bundled_beside_the_image_in(&directory_of_this_image);
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

    /// A binary that links the engine statically, in a runtime unit's `bin/`,
    /// finds the loader in the lend beside `bin/`, right after the one beside
    /// the image itself — the same rule `tatolabd` finds its lend by.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_lent_loader_is_looked_for_in_the_lend_beside_the_binary_directory_of_this_image() {
        let directory_of_this_image = directory_of_the_image_this_code_is_in()
            .expect("the dynamic linker names the image a function of this test binary is in");
        let lent_loader_path =
            vulkan_loader_library_in_the_lend_beside_the_binary_directory(&directory_of_this_image)
                .expect("the test binary's directory has a parent");

        assert_eq!(
            lent_loader_path,
            directory_of_this_image
                .parent()
                .expect("the test binary's directory has a parent")
                .join(crate::runtime_unit_layout::LEND_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT)
                .join(crate::runtime_unit_layout::LENT_RUNTIME_PACKAGE_RELATIVE_TO_THE_LEND)
                .join(BUNDLED_VULKAN_DRIVER_DIRECTORY_NAME)
                .join(VERSIONED_VULKAN_LOADER_LIBRARY_FILE_NAME),
        );

        let candidate_paths: Vec<std::path::PathBuf> = vulkan_loader_library_candidate_paths()
            .into_iter()
            .map(std::path::PathBuf::from)
            .collect();
        let beside_the_image_position = candidate_paths
            .iter()
            .position(|path| {
                *path == vulkan_loader_library_bundled_beside_the_image_in(&directory_of_this_image)
            })
            .unwrap_or_else(|| panic!("the loader beside the image is not searched"));
        assert_eq!(
            candidate_paths.get(beside_the_image_position + 1),
            Some(&lent_loader_path),
            "the lend's loader must come right after the one beside the image: \
             {candidate_paths:?}"
        );
    }

    /// `tatolabd` in an app bundle's `Contents/MacOS` finds the loader among the
    /// bundle's `Frameworks/`, right after the runtime-unit lend's candidate and
    /// before any Homebrew prefix.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_app_bundles_loader_is_looked_for_right_after_the_lent_loader_for_an_image_in_contents_macos()
     {
        let app_bundle_executable_directory =
            std::path::Path::new("/Applications/Tatolab.app/Contents/MacOS");
        let candidate_paths: Vec<std::path::PathBuf> =
            macos_vulkan_loader_library_candidate_paths_for_the_image_in(Some(
                app_bundle_executable_directory,
            ))
            .into_iter()
            .map(std::path::PathBuf::from)
            .collect();
        let app_bundle_loader_path = std::path::PathBuf::from(
            "/Applications/Tatolab.app/Contents/Frameworks/tatolab-runtime/_vulkan_driver/\
             libvulkan.1.dylib",
        );
        let lent_loader_path = vulkan_loader_library_in_the_lend_beside_the_binary_directory(
            app_bundle_executable_directory,
        )
        .expect("Contents/MacOS has a parent");

        let lent_loader_position = candidate_paths
            .iter()
            .position(|path| *path == lent_loader_path)
            .unwrap_or_else(|| panic!("the lent loader is not searched: {candidate_paths:?}"));
        assert_eq!(
            candidate_paths.get(lent_loader_position + 1),
            Some(&app_bundle_loader_path),
            "the app bundle's loader must come right after the lend's: {candidate_paths:?}"
        );
        let first_homebrew_position = candidate_paths
            .iter()
            .position(|path| path.starts_with("/opt/homebrew/"))
            .unwrap_or_else(|| panic!("no Homebrew prefix is searched: {candidate_paths:?}"));
        assert!(
            lent_loader_position + 1 < first_homebrew_position,
            "the app bundle's loader must come before Homebrew's: {candidate_paths:?}"
        );
    }

    /// An image outside an app bundle's `Contents/MacOS` adds no app bundle
    /// candidate, so the search list of the wheel and of a runtime unit's
    /// `bin/` is unchanged.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_image_outside_an_app_bundle_adds_no_app_bundle_loader_candidate() {
        for directory_of_the_image in [
            "/opt/tatolab-runtime/bin",
            "/Users/someone/MacOS",
            "/Applications/Tatolab.app/Contents/Resources",
        ] {
            assert_eq!(
                vulkan_loader_library_in_the_app_bundle_holding_the_image_directory(
                    std::path::Path::new(directory_of_the_image)
                ),
                None,
                "{directory_of_the_image}"
            );
        }
        let candidate_paths = macos_vulkan_loader_library_candidate_paths_for_the_image_in(Some(
            std::path::Path::new("/opt/tatolab-runtime/bin"),
        ));
        assert!(
            !candidate_paths
                .iter()
                .any(|path| path.to_string_lossy().contains("/Frameworks/")),
            "{candidate_paths:?}"
        );
    }

    /// The image's directory is canonical, as `tatolabd`'s own path is when it
    /// finds its lend, so both name the same runtime unit.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_directory_of_this_image_is_canonical() {
        let directory_of_this_image = directory_of_the_image_this_code_is_in()
            .expect("the dynamic linker names the image a function of this test binary is in");

        assert_eq!(
            std::fs::canonicalize(&directory_of_this_image).expect("the image's directory exists"),
            directory_of_this_image
        );
    }

    /// Linux tries the bare soname, then the loader beside the image, then the
    /// lend's beside `bin/` — and nothing else, so no package manager's prefix
    /// is ever named.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_linux_search_list_is_the_bare_soname_then_the_loader_beside_the_image_then_the_lends() {
        assert_eq!(LIBRARY, VERSIONED_VULKAN_LOADER_LIBRARY_FILE_NAME);
        assert_eq!(
            linux_vulkan_loader_library_candidate_paths_for_the_image_in(Some(
                std::path::Path::new("/opt/tatolab-runtime/bin")
            )),
            [
                "/opt/tatolab-runtime/bin/_vulkan_driver/libvulkan.so.1",
                "/opt/tatolab-runtime/lib/tatolab/lend/tatolab/runtime/_vulkan_driver/libvulkan.so.1",
            ]
            .map(std::ffi::OsString::from)
        );
        assert!(linux_vulkan_loader_library_candidate_paths_for_the_image_in(None).is_empty());

        let directory_of_this_image = directory_of_the_image_this_code_is_in()
            .expect("the dynamic linker names the image a function of this test binary is in");
        let mut expected_candidate_paths = vec![std::ffi::OsString::from(LIBRARY)];
        expected_candidate_paths.extend(
            vulkan_loader_libraries_the_runtime_unit_carries_for_the_image_in(
                &directory_of_this_image,
            ),
        );
        assert_eq!(
            vulkan_loader_library_candidate_paths(),
            expected_candidate_paths
        );
    }

    /// Carries the `argv[0]` the parent test started this test binary with.
    #[cfg(target_os = "linux")]
    const SUBSTITUTED_ARGV0_ENVIRONMENT_VARIABLE: &str =
        "STREAMLIB_VULKAN_LOADER_SEARCH_TEST_SUBSTITUTED_ARGV0";

    /// A `tatolabd` started by a bare name or a relative path still finds the
    /// lend beside its own `bin/`: the main executable's directory comes from
    /// the kernel, not from the `argv[0]` glibc's `dladdr` reports.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_main_executables_directory_is_its_own_whatever_argv0_started_it() {
        use std::os::unix::process::CommandExt;

        let substituted_argv0 = "a-bare-name-no-directory-holds";
        let child_test_run = std::process::Command::new(
            std::env::current_exe().expect("the test binary has a path"),
        )
        .arg0(substituted_argv0)
        .args([
            "--exact",
            "--ignored",
            "vulkan_loader_library::tests::the_main_executables_directory_under_a_substituted_argv0",
        ])
        .env(SUBSTITUTED_ARGV0_ENVIRONMENT_VARIABLE, substituted_argv0)
        .current_dir(std::env::temp_dir())
        .output()
        .expect("the test binary starts again");
        let child_test_output = format!(
            "{}{}",
            String::from_utf8_lossy(&child_test_run.stdout),
            String::from_utf8_lossy(&child_test_run.stderr)
        );

        assert!(child_test_run.status.success(), "{child_test_output}");
        assert!(
            child_test_output.contains("1 passed"),
            "the child test did not run: {child_test_output}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "run by the_main_executables_directory_is_its_own_whatever_argv0_started_it"]
    fn the_main_executables_directory_under_a_substituted_argv0() {
        if let Some(substituted_argv0) = std::env::var_os(SUBSTITUTED_ARGV0_ENVIRONMENT_VARIABLE) {
            let image_containing_this_code = image_containing_the_address(
                path_of_the_image_this_code_is_in as *const libc::c_void,
            )
            .expect("dladdr names the image");
            assert_eq!(
                image_containing_this_code
                    .path_as_the_dynamic_linker_names_it
                    .as_os_str(),
                substituted_argv0,
                "glibc names the main executable by its argv[0]"
            );
        }

        assert_eq!(
            directory_of_the_image_this_code_is_in(),
            std::env::current_exe()
                .expect("the test binary has a path")
                .parent()
                .map(std::fs::canonicalize)
                .transpose()
                .expect("the test binary's directory exists")
        );
    }
}
