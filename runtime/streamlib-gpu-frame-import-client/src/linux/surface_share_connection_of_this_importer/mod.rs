// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The surface-sharing connection over the runtime's Unix socket, with memory
//! crossing as descriptors over SCM_RIGHTS.

use std::os::fd::{FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::{
    PoolSlotImportsByPoolSlot, SURFACE_SHARE_CONNECTIONS_SET_ASIDE_AT_MOST,
    SURFACE_SHARE_RESPONSE_TIMEOUT,
};

/// What memory crosses the surface-share channel as: a descriptor over
/// SCM_RIGHTS on Linux.
pub type SurfaceShareTransferredHandle = OwnedFd;

/// One importer's connection to the surface-sharing socket, and the pool slot
/// imports made over it.
pub struct SurfaceShareConnectionOfThisImporter<PoolSlotImport> {
    surface_share_socket_path: PathBuf,
    /// Opened at the first request. Taken out for each exchange and put back
    /// only on success, so a stream with half a frame in it is dropped rather
    /// than reused.
    open_connection: Mutex<Option<UnixStream>>,
    /// Connections whose request failed while the service still held them
    /// open — an answer that outwaited [`SURFACE_SHARE_RESPONSE_TIMEOUT`], or
    /// one this process could not take in — held open and never used again:
    /// the service releases every claim a connection holds once it closes,
    /// and frames this importer still reads were claimed on them.
    connections_set_aside_after_a_failure: Mutex<Vec<UnixStream>>,
    /// Each pool slot's import, made on the first frame over the slot and
    /// reused by every later one. Holding one does not pin a slot — only a
    /// frame's checkout lease or claim does — and the service names each slot
    /// the engine retires on this connection, which drops it here. Emptied
    /// whenever the connection is replaced, since the notices of the slots it
    /// held would have come on the connection that is gone.
    pool_slot_imports_by_pool_slot: Arc<PoolSlotImportsByPoolSlot<PoolSlotImport>>,
}

impl<PoolSlotImport: Clone + Send + Sync + 'static>
    SurfaceShareConnectionOfThisImporter<PoolSlotImport>
{
    /// A connection to the surface-sharing socket at
    /// `surface_share_socket_path`, opened at the first request.
    pub fn new(surface_share_socket_path: PathBuf) -> Self {
        Self {
            surface_share_socket_path,
            open_connection: Mutex::new(None),
            connections_set_aside_after_a_failure: Mutex::new(Vec::new()),
            pool_slot_imports_by_pool_slot: Arc::default(),
        }
    }

    /// The socket this connection reaches.
    pub fn surface_share_socket_path(&self) -> &Path {
        &self.surface_share_socket_path
    }

    /// The pool slot imports made over this connection.
    pub fn pool_slot_imports_by_pool_slot(&self) -> &PoolSlotImportsByPoolSlot<PoolSlotImport> {
        &self.pool_slot_imports_by_pool_slot
    }

    /// One request and its answer, with no handles sent.
    pub fn request(
        &self,
        request: &serde_json::Value,
    ) -> Result<(serde_json::Value, Vec<OwnedFd>), String> {
        let (response, received_fds, ()) = self.exchange(request, &[], |_| ())?;
        Ok((response, received_fds))
    }

    /// The one exchange every surface-share request makes, with
    /// `outbound_fds` riding the request via SCM_RIGHTS — the kernel dups
    /// each into the service's table and the caller keeps its own.
    /// `while_the_connection_is_held` runs on the answer before the
    /// connection is free for another request, so what it records is ordered
    /// against the retirement notice of every later reply.
    ///
    /// Every pool slot import is dropped whenever the connection is replaced:
    /// the service lists a retired slot only on the connection it handed the
    /// slot's memory to.
    pub fn exchange<DecidedWhileTheConnectionIsHeld>(
        &self,
        request: &serde_json::Value,
        outbound_fds: &[RawFd],
        while_the_connection_is_held: impl FnOnce(&serde_json::Value) -> DecidedWhileTheConnectionIsHeld,
    ) -> Result<
        (
            serde_json::Value,
            Vec<OwnedFd>,
            DecidedWhileTheConnectionIsHeld,
        ),
        String,
    > {
        let mut open_connection = self.open_connection.lock();
        let stream = match open_connection.take() {
            Some(open_stream) => open_stream,
            None => self.open_a_fresh_connection()?,
        };
        let (response, received_raw_fds) = match streamlib_surface_client::send_request_with_fds(
            &stream,
            request,
            outbound_fds,
            streamlib_surface_client::MAX_SCM_RIGHTS_FDS,
        ) {
            Ok(answered) => answered,
            Err(io_failure) if io_failure.kind() == std::io::ErrorKind::UnexpectedEof => {
                self.pool_slot_imports_by_pool_slot.drop_every_import();
                return Err(format!(
                    "the surface-share service closed the connection mid-request: {io_failure}"
                ));
            }
            Err(io_failure) => {
                self.connections_set_aside_after_a_failure
                    .lock()
                    .push(stream);
                self.pool_slot_imports_by_pool_slot.drop_every_import();
                let timed_out = matches!(
                    io_failure.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                );
                return Err(if timed_out {
                    format!(
                        "the surface-share service did not answer within {} s; this importer set \
                         that connection aside, keeping the frames it claimed on it held until it \
                         stops, and opens a new one on its next request",
                        SURFACE_SHARE_RESPONSE_TIMEOUT.as_secs()
                    )
                } else {
                    format!(
                        "the surface-share request failed: {io_failure}; this importer set that \
                         connection aside, keeping the frames it claimed on it held until it \
                         stops, and opens a new one on its next request"
                    )
                });
            }
        };
        // SAFETY: adopting kernel-delivered fds the recvmsg just placed in
        // this process's fd table; nothing else holds them.
        let received_fds = received_raw_fds
            .into_iter()
            .map(|raw_fd| unsafe { OwnedFd::from_raw_fd(raw_fd) })
            .collect();
        self.pool_slot_imports_by_pool_slot
            .drop_the_pool_slots_a_reply_lists_as_retired(&response);
        let decided_while_the_connection_is_held = while_the_connection_is_held(&response);
        *open_connection = Some(stream);
        Ok((response, received_fds, decided_while_the_connection_is_held))
    }

    fn open_a_fresh_connection(&self) -> Result<UnixStream, String> {
        let connections_set_aside = self.connections_set_aside_after_a_failure.lock().len();
        if connections_set_aside >= SURFACE_SHARE_CONNECTIONS_SET_ASIDE_AT_MOST {
            return Err(format!(
                "the surface-share service has failed {connections_set_aside} requests, so \
                 this importer asks it nothing more; the connections it set aside stay open, \
                 keeping the frames claimed on them held, until this importer stops"
            ));
        }
        let opened_stream = streamlib_surface_client::connect_to_surface_share_socket(
            &self.surface_share_socket_path,
        )
        .map_err(|connect_failure| {
            format!(
                "could not reach the surface-share socket at {}: {connect_failure}. The \
                         runtime owns that socket; if it is gone, so is the runtime",
                self.surface_share_socket_path.display(),
            )
        })?;
        opened_stream
            .set_read_timeout(Some(SURFACE_SHARE_RESPONSE_TIMEOUT))
            .and_then(|()| opened_stream.set_write_timeout(Some(SURFACE_SHARE_RESPONSE_TIMEOUT)))
            .map_err(|timeout_failure| {
                format!(
                    "could not bound how long the surface-share socket at {} may take to accept a request and answer it: \
                     {timeout_failure}",
                    self.surface_share_socket_path.display(),
                )
            })?;
        self.pool_slot_imports_by_pool_slot.drop_every_import();
        Ok(opened_stream)
    }
}

#[cfg(feature = "test-support")]
impl<PoolSlotImport: Clone + Send + Sync + 'static>
    SurfaceShareConnectionOfThisImporter<PoolSlotImport>
{
    /// Take the open connection out, so the next request opens a fresh one.
    pub fn take_the_open_connection(&self) -> Option<UnixStream> {
        self.open_connection.lock().take()
    }
}

#[cfg(test)]
mod surface_share_response_timeout_tests;
