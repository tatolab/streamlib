// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The iceoryx2 domain a test process gives every node it creates, disjoint from
//! every runtime's domain and every other test process's.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use iceoryx2::node::Node;
use iceoryx2::prelude::ipc;

use super::Iceoryx2Node;
use super::node::create_iceoryx2_node_in_domain;
use crate::core::runtime::{StreamlibRuntimeDirectory, current_process_uid};

/// The folder-name stem of a test process's domain root inside the runtime directory.
const TEST_DOMAIN_ROOT_NAME_STEM: &str = "iox2-test-";

/// Where Linux exposes POSIX shared memory as files.
#[cfg(target_os = "linux")]
const POSIX_SHARED_MEMORY_DIRECTORY: &str = "/dev/shm";

/// One test process's iceoryx2 domain: its own root and its own prefix.
#[derive(Debug)]
pub struct Iceoryx2DomainForThisTestProcess {
    root: PathBuf,
    prefix: String,
}

impl Iceoryx2DomainForThisTestProcess {
    /// The root this test process's nodes keep their files under.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The prefix this test process's nodes name their files and shared memory with.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }
}

/// This test process's domain, sweeping what dead test processes left behind on first use.
pub fn iceoryx2_domain_for_this_test_process() -> &'static Iceoryx2DomainForThisTestProcess {
    static ICEORYX2_DOMAIN_FOR_THIS_TEST_PROCESS: OnceLock<Iceoryx2DomainForThisTestProcess> =
        OnceLock::new();
    ICEORYX2_DOMAIN_FOR_THIS_TEST_PROCESS.get_or_init(|| {
        let runtime_directory = StreamlibRuntimeDirectory::resolve()
            .expect("a test process needs a runtime directory to hold its iceoryx2 domain");
        let uid = current_process_uid();
        #[cfg(target_os = "linux")]
        let posix_shared_memory_directory = Some(Path::new(POSIX_SHARED_MEMORY_DIRECTORY));
        #[cfg(not(target_os = "linux"))]
        let posix_shared_memory_directory: Option<&Path> = None;
        remove_iceoryx2_test_domains_whose_process_is_gone(
            runtime_directory.path(),
            posix_shared_memory_directory,
            uid,
        );
        test_domain_for_process(runtime_directory.path(), uid, std::process::id())
    })
}

/// A raw iceoryx2 node in this test process's own domain.
pub fn create_iceoryx2_node_for_this_test_process() -> Node<ipc::Service> {
    let domain = iceoryx2_domain_for_this_test_process();
    create_iceoryx2_node_in_domain(domain.root(), domain.prefix(), "streamlib-test")
        .expect("a test process opens iceoryx2 nodes in its own domain")
}

impl Iceoryx2Node {
    /// A node in this test process's own domain.
    pub fn for_this_test_process() -> Self {
        Self::wrapping(create_iceoryx2_node_for_this_test_process())
    }
}

fn test_domain_for_process(
    runtime_directory: &Path,
    uid: u32,
    process_id: u32,
) -> Iceoryx2DomainForThisTestProcess {
    Iceoryx2DomainForThisTestProcess {
        root: runtime_directory.join(format!("{TEST_DOMAIN_ROOT_NAME_STEM}{process_id}")),
        prefix: test_domain_prefix(uid, process_id),
    }
}

/// A test domain's prefix: this stem, then the owning pid, then `_`.
pub(crate) fn test_domain_prefix_stem(uid: u32) -> String {
    format!("sl{uid}t")
}

pub(crate) fn test_domain_prefix(uid: u32, process_id: u32) -> String {
    format!("{}{process_id}_", test_domain_prefix_stem(uid))
}

/// Remove the roots and shared memory of every test domain whose process is gone,
/// this process's own pid included, since whatever holds it was an earlier process.
fn remove_iceoryx2_test_domains_whose_process_is_gone(
    runtime_directory: &Path,
    posix_shared_memory_directory: Option<&Path>,
    uid: u32,
) {
    let this_process_id = std::process::id();
    let belongs_to_a_gone_process =
        |process_id: u32| process_id == this_process_id || !process_is_alive(process_id);

    if let Ok(entries) = std::fs::read_dir(runtime_directory) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let process_id = file_name
                .to_str()
                .and_then(|name| name.strip_prefix(TEST_DOMAIN_ROOT_NAME_STEM))
                .and_then(|process_id| process_id.parse::<u32>().ok());
            if process_id.is_some_and(belongs_to_a_gone_process) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }

    let Some(posix_shared_memory_directory) = posix_shared_memory_directory else {
        return;
    };
    let shared_memory_prefix_stem = test_domain_prefix_stem(uid);
    if let Ok(entries) = std::fs::read_dir(posix_shared_memory_directory) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let process_id = file_name
                .to_str()
                .and_then(|name| name.strip_prefix(&shared_memory_prefix_stem))
                .and_then(|rest| rest.split_once('_'))
                .and_then(|(process_id, _)| process_id.parse::<u32>().ok());
            if process_id.is_some_and(belongs_to_a_gone_process) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

fn process_is_alive(process_id: u32) -> bool {
    let Ok(process_id) = libc::pid_t::try_from(process_id) else {
        return false;
    };
    // SAFETY: signal 0 delivers nothing; kill only checks existence and permission.
    let signalled = unsafe { libc::kill(process_id, 0) };
    signalled == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::iceoryx2::ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES;

    pub(crate) fn a_process_id_that_has_exited() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let process_id = child.id();
        child.wait().unwrap();
        process_id
    }

    #[test]
    fn this_test_process_names_its_root_and_prefix_after_its_own_pid() {
        let domain = iceoryx2_domain_for_this_test_process();
        let uid = current_process_uid();

        // Which arm resolved the runtime directory depends on `XDG_RUNTIME_DIR` at
        // first use, which a `#[serial]` runner test in this process may have unset.
        let runtime_directory_name = domain
            .root()
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str());
        assert!(
            matches!(runtime_directory_name, Some(name) if name == "streamlib" || name == format!("streamlib-{uid}")),
            "{}",
            domain.root().display()
        );
        assert_eq!(
            domain.root().file_name().and_then(|name| name.to_str()),
            Some(format!("iox2-test-{}", std::process::id()).as_str())
        );
        assert_eq!(domain.prefix(), format!("sl{uid}t{}_", std::process::id()));
    }

    /// `/run/user/<uid>/streamlib` is the Linux arm of the runtime directory,
    /// and 4 194 304 is the largest pid `pid_max` allows there. macOS resolves
    /// the directory elsewhere and caps pids far lower, so it gets its own
    /// bound below rather than a shared one that would hold on neither.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_test_domain_at_the_longest_pid_fits_the_socket_path_budget_under_a_typical_xdg_runtime_dir()
     {
        let longest_linux_pid = 4_194_304;
        let domain = test_domain_for_process(
            Path::new("/run/user/100000/streamlib"),
            100_000,
            longest_linux_pid,
        );

        assert!(
            domain.root().as_os_str().len() + domain.prefix().len()
                <= ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES,
            "{} + {}",
            domain.root().display(),
            domain.prefix()
        );
    }

    /// The Apple bound, against the tighter `sun_path` macOS leaves and the
    /// `/tmp/streamlib-<uid>/` the runtime directory always resolves to there.
    /// `PID_MAX` is 99 999 on Darwin.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_test_domain_at_the_longest_pid_fits_the_socket_path_budget_under_the_apple_runtime_directory()
     {
        let longest_darwin_pid = 99_999;
        let domain = test_domain_for_process(
            Path::new("/tmp/streamlib-100000"),
            100_000,
            longest_darwin_pid,
        );

        assert!(
            domain.root().as_os_str().len() + domain.prefix().len()
                <= ICEORYX2_DOMAIN_ROOT_AND_PREFIX_BUDGET_BYTES,
            "{} + {}",
            domain.root().display(),
            domain.prefix()
        );
    }

    #[test]
    fn only_the_domains_of_gone_processes_are_swept() {
        let runtime_directory = tempfile::tempdir().unwrap();
        let posix_shared_memory_directory = tempfile::tempdir().unwrap();
        let uid = current_process_uid();
        let gone = a_process_id_that_has_exited();
        let alive = std::os::unix::process::parent_id();
        for process_id in [gone, alive] {
            std::fs::create_dir_all(
                runtime_directory
                    .path()
                    .join(format!("iox2-test-{process_id}"))
                    .join("nodes"),
            )
            .unwrap();
            std::fs::write(
                posix_shared_memory_directory
                    .path()
                    .join(format!("sl{uid}t{process_id}_abc_service.dynamic")),
                b"",
            )
            .unwrap();
        }
        std::fs::create_dir(runtime_directory.path().join("iox2")).unwrap();
        std::fs::write(
            posix_shared_memory_directory
                .path()
                .join(format!("sl{uid}_abc_service.dynamic")),
            b"",
        )
        .unwrap();

        remove_iceoryx2_test_domains_whose_process_is_gone(
            runtime_directory.path(),
            Some(posix_shared_memory_directory.path()),
            uid,
        );

        assert!(
            !runtime_directory
                .path()
                .join(format!("iox2-test-{gone}"))
                .exists()
        );
        assert!(
            runtime_directory
                .path()
                .join(format!("iox2-test-{alive}"))
                .exists()
        );
        assert!(
            runtime_directory.path().join("iox2").exists(),
            "a runtime's own domain is never a test domain"
        );
        let remaining_shared_memory: Vec<String> =
            std::fs::read_dir(posix_shared_memory_directory.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
        assert!(!remaining_shared_memory.contains(&format!("sl{uid}t{gone}_abc_service.dynamic")));
        assert!(remaining_shared_memory.contains(&format!("sl{uid}t{alive}_abc_service.dynamic")));
        assert!(remaining_shared_memory.contains(&format!("sl{uid}_abc_service.dynamic")));
    }
}
