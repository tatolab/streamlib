// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The surface-sharing connection over the runtime's Mach service, with
//! memory crossing as Mach port rights.

use std::ffi::OsString;
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use streamlib_surface_client::{OwnedMachSendRight, SurfaceShareMachServiceConnection};

use crate::{
    PoolSlotImportsByPoolSlot, SURFACE_SHARE_CONNECTIONS_SET_ASIDE_AT_MOST,
    SURFACE_SHARE_RESPONSE_TIMEOUT,
};

/// What memory crosses the surface-share channel as: a Mach port right on
/// macOS.
pub type SurfaceShareTransferredHandle = OwnedMachSendRight;

/// One importer's connection to the surface-sharing Mach service, and the
/// pool slot imports made over it.
pub struct SurfaceShareConnectionOfThisImporter<PoolSlotImport> {
    /// The bootstrap name of the runtime's surface-share Mach service.
    surface_share_mach_service_name: OsString,
    /// Opened at the first request; the service releases every claim it
    /// holds when it closes.
    open_connection: Mutex<Option<Arc<SurfaceShareMachServiceConnection>>>,
    /// Connections whose request failed, held open and never used again: the
    /// service releases every lease a connection holds once it closes, and
    /// frames this importer still reads were checked out on them.
    connections_set_aside_after_a_failure: Mutex<Vec<Arc<SurfaceShareMachServiceConnection>>>,
    /// Each pool slot's import, made on the first frame over the slot and
    /// reused by every later one. Holding one does not pin a slot — only a
    /// frame's checkout lease or claim does — and the service names each slot
    /// the engine retires on this connection, which drops it here. Emptied
    /// whenever the connection is replaced or the service goes away.
    pool_slot_imports_by_pool_slot: Arc<PoolSlotImportsByPoolSlot<PoolSlotImport>>,
}

impl<PoolSlotImport: Clone + Send + Sync + 'static>
    SurfaceShareConnectionOfThisImporter<PoolSlotImport>
{
    /// A connection to the surface-sharing Mach service
    /// `surface_share_mach_service_name`, opened at the first request.
    pub fn new(surface_share_mach_service_name: OsString) -> Self {
        Self {
            surface_share_mach_service_name,
            open_connection: Mutex::new(None),
            connections_set_aside_after_a_failure: Mutex::new(Vec::new()),
            pool_slot_imports_by_pool_slot: Arc::default(),
        }
    }

    /// The pool slot imports made over this connection.
    pub fn pool_slot_imports_by_pool_slot(&self) -> &PoolSlotImportsByPoolSlot<PoolSlotImport> {
        &self.pool_slot_imports_by_pool_slot
    }

    /// One request and its answer, with no port sent.
    pub fn request(
        &self,
        request: &serde_json::Value,
    ) -> Result<(serde_json::Value, Vec<OwnedMachSendRight>), String> {
        let (reply, received_ports, ()) = self.exchange(request, Vec::new(), |_| ())?;
        Ok((reply, received_ports))
    }

    /// The one exchange every surface-share request makes, with
    /// `outbound_ports` riding the request. `while_the_connection_is_held`
    /// runs on the answer before another request can be sent, so what it
    /// records is ordered against the retirement notice of every later reply.
    ///
    /// Every pool slot import is dropped whenever the connection is replaced:
    /// the service lists a retired slot only on the connection it handed the
    /// slot's IOSurface to. A connection whose request failed is set aside,
    /// never used again, and kept open so the frames claimed on it stay held.
    pub fn exchange<DecidedWhileTheConnectionIsHeld>(
        &self,
        request: &serde_json::Value,
        outbound_ports: Vec<OwnedMachSendRight>,
        while_the_connection_is_held: impl FnOnce(&serde_json::Value) -> DecidedWhileTheConnectionIsHeld,
    ) -> Result<
        (
            serde_json::Value,
            Vec<OwnedMachSendRight>,
            DecidedWhileTheConnectionIsHeld,
        ),
        String,
    > {
        let mut open_connection = self.open_connection.lock();
        let connection = match open_connection.as_ref() {
            Some(connection) => Arc::clone(connection),
            None => {
                let connections_set_aside = self.connections_set_aside_after_a_failure.lock().len();
                if connections_set_aside >= SURFACE_SHARE_CONNECTIONS_SET_ASIDE_AT_MOST {
                    return Err(format!(
                        "the surface-share service has failed {connections_set_aside} requests, \
                         so this importer asks it nothing more; the connections it set aside \
                         stay open, keeping the frames claimed on them held, until this importer \
                         stops"
                    ));
                }
                let opened = self.open_a_fresh_connection()?;
                self.pool_slot_imports_by_pool_slot.drop_every_import();
                *open_connection = Some(Arc::clone(&opened));
                opened
            }
        };
        match connection.send_request_with_ports_answered_within(
            request,
            outbound_ports,
            Some(SURFACE_SHARE_RESPONSE_TIMEOUT),
        ) {
            Ok((reply, received_ports)) => {
                self.pool_slot_imports_by_pool_slot
                    .drop_the_pool_slots_a_reply_lists_as_retired(&reply);
                let decided_while_the_connection_is_held = while_the_connection_is_held(&reply);
                Ok((reply, received_ports, decided_while_the_connection_is_held))
            }
            Err(request_failure) => {
                if let Some(failed_connection) = open_connection.take() {
                    self.connections_set_aside_after_a_failure
                        .lock()
                        .push(failed_connection);
                }
                self.pool_slot_imports_by_pool_slot.drop_every_import();
                Err(format!(
                    "the surface-share request failed: {request_failure}; this importer set that \
                     connection aside and opens a new one on its next request"
                ))
            }
        }
    }

    /// Open a connection to the runtime's surface-share Mach service, together
    /// with the thread that drops every pool slot import when the service goes
    /// away — an IOSurface this importer still holds stays readable after the
    /// engine dies, so nothing else would release it.
    fn open_a_fresh_connection(&self) -> Result<Arc<SurfaceShareMachServiceConnection>, String> {
        let service_name = self.surface_share_mach_service_name.to_str().ok_or_else(|| {
            format!(
                "the surface-share Mach service name {:?} is not UTF-8, so it names no bootstrap \
                 service",
                self.surface_share_mach_service_name
            )
        })?;
        let opened = Arc::new(
            SurfaceShareMachServiceConnection::connect(
                service_name,
                SURFACE_SHARE_RESPONSE_TIMEOUT,
            )
            .map_err(|connect_failure| {
                format!(
                    "could not reach the surface-share Mach service '{service_name}': \
                         {connect_failure}. The runtime owns that service; if it is gone, so is \
                         the runtime",
                )
            })?,
        );
        let watched_connection = Arc::downgrade(&opened);
        let pool_slot_imports_by_pool_slot = Arc::downgrade(&self.pool_slot_imports_by_pool_slot);
        std::thread::Builder::new()
            .name("surface-share-service-watch".into())
            .spawn(move || {
                watch_for_the_service_going_away(watched_connection, pool_slot_imports_by_pool_slot)
            })
            .map_err(|spawn_failure| {
                format!(
                    "could not start the thread that watches the surface-share service: \
                     {spawn_failure}"
                )
            })?;
        Ok(opened)
    }
}

/// How often the watcher looks up from its wait to see whether the importer
/// and the connection it watches are still there.
const HOW_OFTEN_THE_SERVICE_WATCHER_CHECKS_ITS_IMPORTER_IS_LIVE: Duration = Duration::from_secs(1);

/// Wait for the service behind `watched_connection` to go away, then drop
/// every import; give up once the importer drops its imports or the
/// connection, so a watcher never outlives what it watches.
fn watch_for_the_service_going_away<PoolSlotImport: Clone + Send + Sync + 'static>(
    watched_connection: Weak<SurfaceShareMachServiceConnection>,
    pool_slot_imports_by_pool_slot: Weak<PoolSlotImportsByPoolSlot<PoolSlotImport>>,
) {
    loop {
        let Some(connection) = watched_connection.upgrade() else {
            return;
        };
        if pool_slot_imports_by_pool_slot.strong_count() == 0 {
            return;
        }
        let went_away = connection.wait_for_the_service_to_go_away(Some(
            HOW_OFTEN_THE_SERVICE_WATCHER_CHECKS_ITS_IMPORTER_IS_LIVE,
        ));
        drop(connection);
        match went_away {
            Ok(false) => {}
            Ok(true) => {
                if let Some(pool_slot_imports_by_pool_slot) =
                    pool_slot_imports_by_pool_slot.upgrade()
                {
                    pool_slot_imports_by_pool_slot.drop_every_import();
                    tracing::info!(
                        "the surface-share service went away; released every pool slot this \
                         importer had imported"
                    );
                }
                return;
            }
            Err(watch_failure) => {
                tracing::warn!(
                    "could not watch the surface-share service for its going away \
                     ({watch_failure}); this importer keeps its imported pool slots until its \
                     connection is replaced"
                );
                return;
            }
        }
    }
}

#[cfg(feature = "test-support")]
impl<PoolSlotImport: Clone + Send + Sync + 'static>
    SurfaceShareConnectionOfThisImporter<PoolSlotImport>
{
    /// Take the open connection out, so the next request opens a fresh one.
    pub fn take_the_open_connection(&self) -> Option<Arc<SurfaceShareMachServiceConnection>> {
        self.open_connection.lock().take()
    }
}
