// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The one root a test build of the runtime and its clients keeps the
//! machine's directories under: the runtime directory, the state directory and,
//! on Linux, the machine runtime lock; and the port a test build's loopback
//! HTTP listener binds. On macOS the loopback HTTP listener's bind
//! is the machine runtime lock.
//! A root too long for iceoryx2's socket paths keeps its iceoryx2 domain under
//! it still, reached through a short symlink in `/tmp` named for the root.
//! Compiled only with the `machine-directories-under-a-test-root` feature,
//! which a release build never enables, so a test build never touches the real
//! machine's.

use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::directory_at_an_explicit_mode::{
    OWNER_ONLY_DIRECTORY_MODE, create_directory_and_its_missing_parents_at_mode,
};
use crate::streamlib_runtime_directory::{
    ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES, SHARED_TEMPORARY_DIRECTORY_FOR_THE_FALLBACK,
    StreamlibRuntimeDirectoryRefusal, engine_owned_iceoryx2_prefix_for_uid,
    iceoryx2_domain_folder_inside,
};

/// The environment variable a test build reads its machine root from.
pub const TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE: &str = "TATOLAB_TEST_MACHINE_ROOT";

/// The environment variable a test build reads its loopback HTTP listener's
/// port from; unset, a test build's runtime serves no loopback HTTP on Linux and
/// is refused at the machine runtime lock on macOS.
pub const TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE: &str = "TATOLAB_TEST_LOOPBACK_HTTP_PORT";

/// Why a test build refused its machine root or its loopback HTTP port, naming the variable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TestMachineRootRefusal {
    /// The variable is unset or empty.
    #[error(
        "{TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE} is not set: this build has the \
         machine-directories-under-a-test-root feature and never touches the real machine's \
         directories; set it to a short absolute path such as /tmp/tl-XXXX"
    )]
    NotSet,
    /// The variable names a relative path.
    #[error("{TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE} is {value:?}, which is not an absolute path")]
    NotAnAbsolutePath { value: OsString },
    /// The loopback HTTP port's variable is unset or empty.
    #[error(
        "{TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE} is not set: this build has the \
         machine-directories-under-a-test-root feature and never binds the machine's own loopback \
         HTTP port; set it to a free port"
    )]
    LoopbackHttpPortIsNotSet,
    /// The loopback HTTP port's variable names no port from 1 to 65535.
    #[error(
        "{TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE} is {value:?}, which is not a port from 1 \
         to 65535"
    )]
    LoopbackHttpPortIsNotAPort { value: OsString },
}

/// Where a test build's runtime keeps its iceoryx2 domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Iceoryx2DomainRootOfATestMachineRoot {
    /// `<root>/run/iox2`, whose socket paths fit `sun_path`.
    InsideTheRuntimeDirectory(PathBuf),
    /// `<root>/run/iox2` is too long for its socket paths, so iceoryx2 is
    /// configured with `link`, a short symlink in `/tmp` named for the root and
    /// the uid, which names `directory_it_names` (`<root>/run/iox2`).
    ShortLinkInTheSharedTemporaryDirectory {
        link: PathBuf,
        directory_it_names: PathBuf,
    },
}

/// The root every machine directory sits under in this test build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestMachineRoot {
    path: PathBuf,
}

impl TestMachineRoot {
    /// Read the root from [`TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE`], refusing it unset or relative.
    pub fn from_the_environment() -> Result<Self, TestMachineRootRefusal> {
        test_machine_root_from(std::env::var_os(TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE))
    }

    /// The root itself.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `<root>/run/`, the runtime directory.
    pub fn runtime_directory(&self) -> PathBuf {
        self.path.join("run")
    }

    /// `<root>/state/`, the state directory.
    pub fn state_directory(&self) -> PathBuf {
        self.path.join("state")
    }

    /// Where the runtime of `uid` keeps its iceoryx2 domain under this root.
    ///
    /// Every process reading the same root under the same uid names the same
    /// link, so a runtime and its clients agree on the domain.
    pub fn iceoryx2_domain_root(&self, uid: u32) -> Iceoryx2DomainRootOfATestMachineRoot {
        let inside_the_runtime_directory = iceoryx2_domain_folder_inside(&self.runtime_directory());
        let root_and_prefix_bytes = inside_the_runtime_directory.as_os_str().len()
            + engine_owned_iceoryx2_prefix_for_uid(uid).len();
        if root_and_prefix_bytes <= ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES {
            return Iceoryx2DomainRootOfATestMachineRoot::InsideTheRuntimeDirectory(
                inside_the_runtime_directory,
            );
        }
        let root_fingerprint = fnv1a_64(self.path.as_os_str().as_encoded_bytes());
        Iceoryx2DomainRootOfATestMachineRoot::ShortLinkInTheSharedTemporaryDirectory {
            link: Path::new(SHARED_TEMPORARY_DIRECTORY_FOR_THE_FALLBACK)
                .join(format!("tl-iox2-{uid}-{root_fingerprint:016x}")),
            directory_it_names: inside_the_runtime_directory,
        }
    }

    /// `tatolab-runtime:<root>`, the Linux machine runtime lock's abstract socket name.
    pub fn machine_runtime_lock_abstract_socket_name(&self, production_name: &str) -> String {
        format!("{production_name}:{}", self.path.display())
    }
}

/// FNV-1a, 64-bit: a hash every build of every client computes alike, which
/// `std`'s hasher does not promise across toolchains.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const FNV_64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_64_PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(FNV_64_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_64_PRIME)
    })
}

/// Create `directory_it_names` and the symlink `link` naming it, or accept a
/// link `uid` already made to it.
///
/// `/tmp` is shared, so whatever else stands at `link` — another user's link, a
/// link elsewhere, a file — is refused rather than followed.
pub fn link_the_short_iceoryx2_domain_root_to_its_directory(
    link: &Path,
    directory_it_names: &Path,
    uid: u32,
) -> Result<(), StreamlibRuntimeDirectoryRefusal> {
    let refusal =
        |what_is_wrong: String| StreamlibRuntimeDirectoryRefusal::Iceoryx2DomainRootLinkRefused {
            link: link.to_path_buf(),
            directory_it_names: directory_it_names.to_path_buf(),
            what_is_wrong,
        };
    create_directory_and_its_missing_parents_at_mode(directory_it_names, OWNER_ONLY_DIRECTORY_MODE)
        .map_err(|source| refusal(format!("the directory could not be created ({source})")))?;
    match std::os::unix::fs::symlink(directory_it_names, link) {
        Ok(()) => return Ok(()),
        Err(failure) if failure.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(failure) => return Err(refusal(format!("the link could not be made ({failure})"))),
    }
    let metadata = std::fs::symlink_metadata(link).map_err(|source| {
        refusal(format!(
            "what stands there could not be inspected ({source})"
        ))
    })?;
    if !metadata.file_type().is_symlink() {
        return Err(refusal(
            "something other than a symlink stands there".to_string(),
        ));
    }
    if metadata.uid() != uid {
        return Err(refusal(format!(
            "a link owned by uid {}, not uid {uid}, stands there",
            metadata.uid()
        )));
    }
    let named = std::fs::read_link(link)
        .map_err(|source| refusal(format!("the link there could not be read ({source})")))?;
    if named != directory_it_names {
        return Err(refusal(format!(
            "a link naming {} stands there",
            named.display()
        )));
    }
    Ok(())
}

fn test_machine_root_from(
    value: Option<OsString>,
) -> Result<TestMachineRoot, TestMachineRootRefusal> {
    let value = value
        .filter(|value| !value.is_empty())
        .ok_or(TestMachineRootRefusal::NotSet)?;
    let path = PathBuf::from(&value);
    if !path.is_absolute() {
        return Err(TestMachineRootRefusal::NotAnAbsolutePath { value });
    }
    Ok(TestMachineRoot { path })
}

/// The port [`TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE`] names, refusing it unset or not a port.
pub fn test_loopback_http_port_from_the_environment() -> Result<u16, TestMachineRootRefusal> {
    test_loopback_http_port_from(std::env::var_os(
        TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE,
    ))
}

fn test_loopback_http_port_from(value: Option<OsString>) -> Result<u16, TestMachineRootRefusal> {
    let value = value
        .filter(|value| !value.is_empty())
        .ok_or(TestMachineRootRefusal::LoopbackHttpPortIsNotSet)?;
    value
        .to_str()
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or(TestMachineRootRefusal::LoopbackHttpPortIsNotAPort { value })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_or_empty_root_is_refused_naming_the_variable() {
        for value in [None, Some(OsString::new())] {
            let refusal = test_machine_root_from(value).unwrap_err();
            assert_eq!(refusal, TestMachineRootRefusal::NotSet);
            assert!(
                refusal
                    .to_string()
                    .contains(TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE)
            );
        }
    }

    #[test]
    fn a_relative_root_is_refused_by_name() {
        let refusal = test_machine_root_from(Some(OsString::from("tl-relative"))).unwrap_err();
        assert!(refusal.to_string().contains("\"tl-relative\""), "{refusal}");
        assert!(
            refusal.to_string().contains("not an absolute path"),
            "{refusal}"
        );
    }

    #[test]
    fn every_machine_directory_sits_under_the_root() {
        let root = test_machine_root_from(Some(OsString::from("/tmp/tl-abcd"))).unwrap();
        assert_eq!(root.runtime_directory(), PathBuf::from("/tmp/tl-abcd/run"));
        assert_eq!(root.state_directory(), PathBuf::from("/tmp/tl-abcd/state"));
        assert_eq!(
            root.machine_runtime_lock_abstract_socket_name("tatolab-runtime"),
            "tatolab-runtime:/tmp/tl-abcd"
        );
    }

    #[test]
    fn a_short_root_keeps_its_iceoryx2_domain_inside_its_runtime_directory() {
        let root = test_machine_root_from(Some(OsString::from("/tmp/tl-abcd"))).unwrap();
        assert_eq!(
            root.iceoryx2_domain_root(501),
            Iceoryx2DomainRootOfATestMachineRoot::InsideTheRuntimeDirectory(PathBuf::from(
                "/tmp/tl-abcd/run/iox2"
            ))
        );
    }

    #[test]
    fn a_long_root_keeps_its_iceoryx2_domain_behind_a_link_inside_the_budget_named_for_the_root() {
        let long_root = "/Users/someone-with-a-long-name/scratch/app176/proof-root";
        let root = test_machine_root_from(Some(OsString::from(long_root))).unwrap();
        let other_root =
            test_machine_root_from(Some(OsString::from(format!("{long_root}-two")))).unwrap();

        let Iceoryx2DomainRootOfATestMachineRoot::ShortLinkInTheSharedTemporaryDirectory {
            link,
            directory_it_names,
        } = root.iceoryx2_domain_root(u32::MAX)
        else {
            panic!("{long_root} is past the budget");
        };
        assert_eq!(
            directory_it_names,
            PathBuf::from(format!("{long_root}/run/iox2"))
        );
        assert!(link.starts_with("/tmp"), "{}", link.display());
        assert!(
            link.as_os_str().len() + engine_owned_iceoryx2_prefix_for_uid(u32::MAX).len()
                <= ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES,
            "{}",
            link.display()
        );
        assert_eq!(
            root.iceoryx2_domain_root(u32::MAX),
            Iceoryx2DomainRootOfATestMachineRoot::ShortLinkInTheSharedTemporaryDirectory {
                link: link.clone(),
                directory_it_names: directory_it_names.clone(),
            },
            "every process reading the root names the same link"
        );
        assert_ne!(
            other_root.iceoryx2_domain_root(u32::MAX),
            root.iceoryx2_domain_root(u32::MAX)
        );
        assert_ne!(
            root.iceoryx2_domain_root(1000),
            root.iceoryx2_domain_root(1001)
        );
    }

    #[test]
    fn the_root_fingerprint_is_fnv1a_64() {
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn the_link_is_made_once_and_accepted_again_and_names_its_directory() {
        let scratch = crate::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
        let link = scratch.path().join("tl-iox2-link");
        let directory_it_names = scratch.path().join("root").join("run").join("iox2");
        let uid = crate::streamlib_runtime_directory::current_process_uid();

        link_the_short_iceoryx2_domain_root_to_its_directory(&link, &directory_it_names, uid)
            .unwrap();
        link_the_short_iceoryx2_domain_root_to_its_directory(&link, &directory_it_names, uid)
            .expect("a second process accepts the link the first made");

        assert_eq!(std::fs::read_link(&link).unwrap(), directory_it_names);
        assert!(directory_it_names.is_dir());
        assert_eq!(
            std::fs::metadata(&directory_it_names).unwrap().mode() & 0o777,
            OWNER_ONLY_DIRECTORY_MODE
        );
    }

    #[test]
    fn whatever_else_stands_at_the_link_is_refused_by_name() {
        let scratch = crate::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
        let directory_it_names = scratch.path().join("root").join("run").join("iox2");
        let uid = crate::streamlib_runtime_directory::current_process_uid();

        let link_elsewhere = scratch.path().join("link-elsewhere");
        std::os::unix::fs::symlink(scratch.path().join("elsewhere"), &link_elsewhere).unwrap();
        let file_at_the_link = scratch.path().join("a-file");
        std::fs::write(&file_at_the_link, b"").unwrap();

        for (link, what_is_wrong) in [
            (&link_elsewhere, "a link naming"),
            (&file_at_the_link, "something other than a symlink"),
        ] {
            let refusal = link_the_short_iceoryx2_domain_root_to_its_directory(
                link,
                &directory_it_names,
                uid,
            )
            .unwrap_err()
            .to_string();
            assert!(refusal.contains(&link.display().to_string()), "{refusal}");
            assert!(refusal.contains(what_is_wrong), "{refusal}");
        }

        let refusal = link_the_short_iceoryx2_domain_root_to_its_directory(
            &link_elsewhere,
            &directory_it_names,
            uid.wrapping_add(1),
        )
        .unwrap_err()
        .to_string();
        assert!(refusal.contains("owned by uid"), "{refusal}");
    }

    #[test]
    fn a_named_port_is_read_and_an_unset_or_unparsable_one_is_refused_naming_the_variable() {
        assert_eq!(
            test_loopback_http_port_from(Some(OsString::from("14446"))),
            Ok(14446)
        );
        for value in [None, Some(OsString::new())] {
            let refusal = test_loopback_http_port_from(value).unwrap_err();
            assert_eq!(refusal, TestMachineRootRefusal::LoopbackHttpPortIsNotSet);
            assert!(
                refusal.to_string().starts_with(&format!(
                    "{TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE} is not set"
                )),
                "{refusal}"
            );
        }
        for value in ["0", "65536", "http", "-1"] {
            let refusal = test_loopback_http_port_from(Some(OsString::from(value))).unwrap_err();
            assert_eq!(
                refusal.to_string(),
                format!(
                    "{TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE} is \"{value}\", which is not a \
                     port from 1 to 65535"
                )
            );
        }
    }
}
