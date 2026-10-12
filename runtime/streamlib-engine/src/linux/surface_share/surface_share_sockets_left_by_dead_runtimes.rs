// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Removing the surface-sharing sockets runtimes that died without unbinding left behind.

use std::collections::HashSet;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

use streamlib_runtime_client_contract::streamlib_runtime_directory::StreamlibRuntimeDirectory;

use crate::core::unix_socket_path_cleared_for_bind::{
    UnixSocketPathClearedForBind, clear_unix_socket_path_for_bind,
};

/// The kernel's table of every Unix socket a live process in this network
/// namespace holds, with the path each is bound to.
const UNIX_SOCKETS_HELD_BY_LIVE_PROCESSES_TABLE: &str = "/proc/net/unix";

/// The fields `/proc/net/unix` prints before a socket's bound path.
const UNIX_SOCKET_TABLE_FIELDS_BEFORE_THE_PATH: usize = 7;

/// Remove each surface-sharing socket in `runtime_directory` that no live process holds, returning the paths removed.
pub fn remove_the_surface_share_sockets_left_by_dead_runtimes(
    runtime_directory: &StreamlibRuntimeDirectory,
) -> std::io::Result<Vec<PathBuf>> {
    // Listed before the table is read: a socket bound after the listing is
    // never a candidate, and one bound before it is in the table if its
    // process still lives.
    let candidate_socket_paths = runtime_directory.surface_share_socket_paths_of_every_runtime()?;
    let unix_socket_table = std::fs::read(UNIX_SOCKETS_HELD_BY_LIVE_PROCESSES_TABLE)?;
    let unix_sockets_held_by_live_processes =
        bound_paths_in_the_unix_socket_table(&String::from_utf8_lossy(&unix_socket_table));
    Ok(remove_the_socket_files_no_live_process_holds(
        candidate_socket_paths,
        &unix_sockets_held_by_live_processes,
    ))
}

/// The paths every socket in `/proc/net/unix`'s text is bound to.
fn bound_paths_in_the_unix_socket_table(unix_socket_table: &str) -> HashSet<PathBuf> {
    unix_socket_table
        .lines()
        .skip(1)
        .filter_map(bound_path_of_one_unix_socket_table_line)
        .map(PathBuf::from)
        .collect()
}

/// The path after the seven fields of one table line, or `None` for an unbound socket.
// The kernel pads the inode field (`%5lu`), so fields are split on runs of
// spaces; the path follows the inode after one space and may itself hold spaces.
fn bound_path_of_one_unix_socket_table_line(line: &str) -> Option<&str> {
    let mut rest_of_the_line = line;
    for _ in 0..UNIX_SOCKET_TABLE_FIELDS_BEFORE_THE_PATH {
        let (_field, after_the_field) = rest_of_the_line.trim_start_matches(' ').split_once(' ')?;
        rest_of_the_line = after_the_field;
    }
    (!rest_of_the_line.is_empty()).then_some(rest_of_the_line)
}

/// Remove each socket file the table does not list and no process answers on.
// Both signals, because the table misses a live socket bound under another
// spelling of the directory or in another network namespace, and a connect
// alone takes a socket bound but not yet listening for dead.
fn remove_the_socket_files_no_live_process_holds(
    candidate_socket_paths: Vec<PathBuf>,
    unix_sockets_held_by_live_processes: &HashSet<PathBuf>,
) -> Vec<PathBuf> {
    let mut removed_socket_paths = Vec::new();
    for socket_path in candidate_socket_paths {
        if unix_sockets_held_by_live_processes.contains(&socket_path)
            || !is_a_socket_file(&socket_path)
        {
            continue;
        }
        match clear_unix_socket_path_for_bind(&socket_path) {
            Ok(UnixSocketPathClearedForBind::StaleSocketFileRemoved) => {
                removed_socket_paths.push(socket_path)
            }
            Ok(UnixSocketPathClearedForBind::NothingWasThere) => {}
            Err(refusal) => tracing::debug!(
                "a surface-sharing socket the kernel lists no holder for is left in place: {refusal}"
            ),
        }
    }
    removed_socket_paths
}

fn is_a_socket_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_yields_each_bound_path_whatever_the_inode_padding_and_skips_unbound_sockets() {
        let unix_socket_table = "\
Num       RefCount Protocol Flags    Type St Inode Path
0000000000000000: 00000002 00000000 00010000 0001 01 3700053 /run/user/1000/streamlib/surface-share-Rlive.sock
0000000000000000: 00000003 00000000 00000000 0001 03 3611608
0000000000000000: 00000002 00000000 00010000 0001 01  8915 /run/dbus/system_bus_socket
0000000000000000: 00000003 00000000 00000000 0001 03   749
0000000000000000: 00000002 00000000 00010000 0001 01 3700099 /tmp/a dir/with spaces.sock
0000000000000000: 00000002 00000000 00010000 0001 01 3700100 @abstract-name
";

        let bound_paths = bound_paths_in_the_unix_socket_table(unix_socket_table);

        assert_eq!(
            bound_paths,
            HashSet::from([
                PathBuf::from("/run/user/1000/streamlib/surface-share-Rlive.sock"),
                PathBuf::from("/run/dbus/system_bus_socket"),
                PathBuf::from("/tmp/a dir/with spaces.sock"),
                PathBuf::from("@abstract-name"),
            ])
        );
    }

    #[test]
    fn a_socket_neither_listed_nor_answering_is_removed_and_every_other_file_stays() {
        let directory = tempfile::tempdir().unwrap();
        let dead_runtimes_socket = directory.path().join("surface-share-Rdead.sock");
        drop(std::os::unix::net::UnixListener::bind(&dead_runtimes_socket).unwrap());
        let listed_socket = directory.path().join("surface-share-Rlisted.sock");
        drop(std::os::unix::net::UnixListener::bind(&listed_socket).unwrap());
        let answering_unlisted_socket = directory.path().join("surface-share-Ranswering.sock");
        let _answering_listener =
            std::os::unix::net::UnixListener::bind(&answering_unlisted_socket).unwrap();
        let regular_file = directory.path().join("surface-share-Rfile.sock");
        std::fs::write(&regular_file, b"").unwrap();

        let removed = remove_the_socket_files_no_live_process_holds(
            vec![
                dead_runtimes_socket.clone(),
                listed_socket.clone(),
                answering_unlisted_socket.clone(),
                regular_file.clone(),
            ],
            &HashSet::from([listed_socket.clone()]),
        );

        assert_eq!(removed, vec![dead_runtimes_socket.clone()]);
        assert!(!dead_runtimes_socket.exists());
        assert!(listed_socket.exists());
        assert!(answering_unlisted_socket.exists());
        assert!(regular_file.exists());
    }

    #[test]
    fn the_kernels_table_lists_a_socket_bound_but_not_yet_listening() {
        let directory = tempfile::tempdir().unwrap();
        let bound_not_listening = directory.path().join("surface-share-Rstarting.sock");
        // SAFETY: plain socket, bind and close calls on a `sockaddr_un` this
        // test owns.
        let socket_descriptor = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        assert!(socket_descriptor >= 0);
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let path_bytes = bound_not_listening.as_os_str().as_encoded_bytes();
        for (slot, byte) in address.sun_path.iter_mut().zip(path_bytes) {
            *slot = *byte as libc::c_char;
        }
        let bound = unsafe {
            libc::bind(
                socket_descriptor,
                (&address as *const libc::sockaddr_un).cast(),
                std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
            )
        };
        assert_eq!(bound, 0, "{}", std::io::Error::last_os_error());

        let held = bound_paths_in_the_unix_socket_table(&String::from_utf8_lossy(
            &std::fs::read(UNIX_SOCKETS_HELD_BY_LIVE_PROCESSES_TABLE).unwrap(),
        ));

        assert!(held.contains(&bound_not_listening));
        unsafe { libc::close(socket_descriptor) };
    }
}
