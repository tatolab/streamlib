// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The start-up look at how much POSIX shared memory this machine will actually
//! hand a runtime's iceoryx2 segments.
//!
//! iceoryx2 creates a data segment at its apparent size and the kernel backs
//! only the pages a producer touches, so a segment far larger than the tmpfs
//! behind it is created without complaint. The producer then takes SIGBUS
//! mid-copy the first time its resident need crosses the limit — a crash, with
//! nothing named. Docker's default 64 MB `/dev/shm` is under one trusted
//! channel's ceiling, so the warning below is the only notice a containerised
//! run gets before that crash.

use std::path::Path;

use streamlib_ipc_types::TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES;

/// The tmpfs every POSIX `shm_open` on Linux lands in, which is where iceoryx2
/// puts every data segment and every dynamic-config storage.
#[cfg(target_os = "linux")]
const POSIX_SHARED_MEMORY_MOUNT_POINT: &str = "/dev/shm";

/// Trusted-ceiling-sized chunks of headroom a runtime wants before it starts.
///
/// One chunk is what a single channel at the trusted ceiling occupies per slot,
/// and a graph has several channels with several slots each — so this is not a
/// sufficiency bound, it is the floor below which a run is certain to hit the
/// limit rather than merely likely to.
const TRUSTED_CEILING_CHUNKS_OF_HEADROOM_A_RUNTIME_WANTS: usize = 4;

/// Free shared-memory bytes below which a runtime warns at start.
pub const POSIX_SHARED_MEMORY_FREE_BYTES_A_RUNTIME_WANTS: usize =
    TRUSTED_CEILING_CHUNKS_OF_HEADROOM_A_RUNTIME_WANTS * TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES;

/// Warn once, at runtime start, when this machine's POSIX shared memory holds
/// less than [`POSIX_SHARED_MEMORY_FREE_BYTES_A_RUNTIME_WANTS`].
///
/// Never a refusal: a graph of small bags runs fine in a 64 MB container, and
/// refusing one would strand exactly the environments — CI, a multi-host test
/// fixture — this engine is meant to run in.
pub fn warn_when_posix_shared_memory_is_short_for_a_runtime() {
    #[cfg(target_os = "linux")]
    {
        let mount_point = Path::new(POSIX_SHARED_MEMORY_MOUNT_POINT);
        match free_bytes_on_the_filesystem_holding(mount_point) {
            Some(free_bytes) => {
                emit_the_shared_memory_headroom_reading(mount_point, free_bytes);
            }
            None => tracing::debug!(
                "could not read the free space on {}; a runtime does not depend on the reading",
                mount_point.display()
            ),
        }
    }
}

/// Raise the warning when `free_bytes` is short of what a runtime wants.
///
/// Split from the `statvfs` call so the threshold is testable without a
/// filesystem of a chosen size.
#[cfg_attr(not(target_os = "linux"), expect(dead_code))]
fn emit_the_shared_memory_headroom_reading(mount_point: &Path, free_bytes: u64) {
    if free_bytes >= POSIX_SHARED_MEMORY_FREE_BYTES_A_RUNTIME_WANTS as u64 {
        tracing::debug!(
            shared_memory = %mount_point.display(),
            free_bytes,
            "POSIX shared memory has the headroom a runtime wants"
        );
        return;
    }
    tracing::warn!(
        shared_memory = %mount_point.display(),
        free_bytes,
        wanted_free_bytes = POSIX_SHARED_MEMORY_FREE_BYTES_A_RUNTIME_WANTS,
        trusted_channel_ceiling_bytes = TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
        "POSIX shared memory is smaller than this runtime's channels may need; a producer \
         whose bags outgrow it dies of SIGBUS mid-write rather than being refused. Give the \
         container more (docker `--shm-size`, Kubernetes an `emptyDir` medium `Memory` volume) \
         or keep every bag well under the per-channel ceiling"
    );
}

/// Free bytes on the filesystem holding `path`, or `None` where it cannot be read.
#[cfg(target_os = "linux")]
fn free_bytes_on_the_filesystem_holding(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;

    let path_with_terminator = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut filesystem_statistics = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: the path is NUL-terminated and the out-parameter is a live,
    // correctly-sized allocation `statvfs` fills on success.
    let read_succeeded = unsafe {
        libc::statvfs(
            path_with_terminator.as_ptr(),
            filesystem_statistics.as_mut_ptr(),
        )
    } == 0;
    if !read_succeeded {
        return None;
    }
    // SAFETY: `statvfs` returned success, so it initialized the struct.
    let filesystem_statistics = unsafe { filesystem_statistics.assume_init() };
    (filesystem_statistics.f_bsize as u64).checked_mul(filesystem_statistics.f_bavail as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Docker's default `/dev/shm` is 64 MB, which is under one trusted channel's
    /// ceiling: the case HYG-1 measured a producer SIGBUS in. Raise the threshold
    /// test to a number a default container passes and the warning stops firing
    /// exactly where it is owed.
    #[test]
    fn dockers_default_shared_memory_is_short_of_what_a_runtime_wants() {
        const DOCKER_DEFAULT_SHARED_MEMORY_BYTES: u64 = 64 * 1000 * 1000;
        assert!(
            DOCKER_DEFAULT_SHARED_MEMORY_BYTES
                < POSIX_SHARED_MEMORY_FREE_BYTES_A_RUNTIME_WANTS as u64,
            "a default container must trip the headroom warning"
        );
        assert!(
            POSIX_SHARED_MEMORY_FREE_BYTES_A_RUNTIME_WANTS > TRUSTED_CHANNEL_PAYLOAD_CEILING_BYTES,
            "the threshold must want more than one ceiling-sized chunk"
        );
    }

    /// The reading runs on whatever this machine has, and says nothing that could
    /// stop a start either way.
    #[test]
    fn reading_this_machines_shared_memory_never_refuses_a_start() {
        warn_when_posix_shared_memory_is_short_for_a_runtime();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_filesystem_that_is_not_there_reads_as_no_reading_rather_than_a_zero() {
        assert_eq!(
            free_bytes_on_the_filesystem_holding(Path::new(
                "/nonexistent-streamlib-shared-memory-mount"
            )),
            None,
            "an unreadable mount point must not read as a full one"
        );
        assert!(
            free_bytes_on_the_filesystem_holding(Path::new("/dev/shm")).is_some(),
            "this machine's POSIX shared memory must read"
        );
    }
}
