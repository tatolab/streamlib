// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! What a viewer reads an Image port's frames through: each frame an Image
//! message names, claimed in its pool's claim table and handed over as
//! memory the viewer reads, imported once per registration — on macOS the
//! pool slot's IOSurface, on Linux the slot itself or the engine's GPU copy
//! of the frame, a tiled DMA-BUF image for a display import or an OPAQUE_FD
//! buffer for a CPU read.

use std::sync::Arc;

use streamlib_ipc_types::GpuPoolSlotFrameDescriptorBody;
#[cfg(target_os = "macos")]
use streamlib_surface_client::SURFACE_SHARE_IMPORT_POOL_SLOT_OP;
#[cfg(target_os = "linux")]
use streamlib_surface_client::{
    SURFACE_SHARE_FRAME_IMAGE_LAYOUT_FIELD, SURFACE_SHARE_IMPORT_VIEWED_FRAME_OP,
    SURFACE_SHARE_VIEWED_FRAME_IMPORTS_HELD_FIELD, SURFACE_SHARE_VIEWED_FRAME_READ_BY_FIELD,
    ViewedFrameReadBy, published_frame_id_of_pool_slot,
};
use streamlib_surface_client::{
    gpu_pool_uuid_wire_name, refusal_the_surface_share_service_answered,
};

use crate::{
    GpuPoolClaimTablesOfThisImporter, GpuPoolSlotFrameClaimOfThisImporter, GpuPoolSlotOfAViewer,
    SurfaceShareConnectionOfThisImporter, pool_slots_a_reply_lists_as_retired,
};

/// A viewer's importer of the frames a runtime's GPU pools publish, over the
/// surface-sharing address `open_viewer` answered.
pub struct GpuPoolFrameImporterOfAViewer {
    surface_share_connection: SurfaceShareConnectionOfThisImporter<Arc<GpuPoolSlotOfAViewer>>,
    gpu_pool_claim_tables: GpuPoolClaimTablesOfThisImporter,
    #[cfg(target_os = "linux")]
    viewed_frame_read_by: ViewedFrameReadBy,
}

/// One frame a viewer holds: memory the viewer reads holding it, its pool
/// slot kept from being handed out again until this drops.
pub struct GpuPoolFrameHeldByAViewer {
    frame: GpuPoolSlotFrameDescriptorBody,
    #[cfg(target_os = "macos")]
    _iosurface_use_count_claim: crate::apple::IOSurfaceUseCountClaimOfAViewer,
    pool_slot: Arc<GpuPoolSlotOfAViewer>,
    _claim: Arc<GpuPoolSlotFrameClaimOfThisImporter>,
}

impl GpuPoolFrameHeldByAViewer {
    /// The descriptor the frame was imported by.
    pub fn frame(&self) -> &GpuPoolSlotFrameDescriptorBody {
        &self.frame
    }

    /// The memory the frame lies in: its pool slot's, or on Linux the
    /// engine's copy of it in memory the viewer reads.
    pub fn pool_slot(&self) -> &GpuPoolSlotOfAViewer {
        &self.pool_slot
    }
}

impl GpuPoolFrameImporterOfAViewer {
    /// An importer over the surface-share socket at `surface_share_address`
    /// whose frames are read the way `viewed_frame_read_by` says.
    #[cfg(target_os = "linux")]
    pub fn new(
        surface_share_address: impl Into<std::ffi::OsString>,
        viewed_frame_read_by: ViewedFrameReadBy,
    ) -> Self {
        Self {
            surface_share_connection: SurfaceShareConnectionOfThisImporter::new(
                std::path::PathBuf::from(surface_share_address.into()),
            ),
            gpu_pool_claim_tables: GpuPoolClaimTablesOfThisImporter::default(),
            viewed_frame_read_by,
        }
    }

    /// An importer over the surface-share Mach service `surface_share_address`.
    #[cfg(target_os = "macos")]
    pub fn new(surface_share_address: impl Into<std::ffi::OsString>) -> Self {
        Self {
            surface_share_connection: SurfaceShareConnectionOfThisImporter::new(
                surface_share_address.into(),
            ),
            gpu_pool_claim_tables: GpuPoolClaimTablesOfThisImporter::default(),
        }
    }

    /// Claim and import the frame `frame` names: the first frame of a pool
    /// fetches its claim table, the first frame over a registration imports
    /// it, and every later one reuses that import. On Linux each frame is
    /// asked of the engine, which copies it on its GPU when its slot is not
    /// memory the viewer reads the way it was made to. Refused when the slot moved past the frame,
    /// when its pool's claim table was refused to this viewer, when the
    /// table's request failed, which the pool's next frame asks again, and
    /// when the engine refuses the copy.
    pub fn hold_the_frame(
        &self,
        frame: GpuPoolSlotFrameDescriptorBody,
    ) -> Result<GpuPoolFrameHeldByAViewer, String> {
        let claim = self
            .gpu_pool_claim_tables
            .note_and_claim_a_pooled_frame(frame, &self.surface_share_connection)?
            .ok_or_else(|| {
                let pool_wire_name = gpu_pool_uuid_wire_name(&frame.pool);
                if self
                    .gpu_pool_claim_tables
                    .the_hand_over_was_refused(&frame.pool)
                {
                    format!(
                        "the surface-share service refused GPU pool {pool_wire_name}'s claim \
                         table to this viewer, so none of its frames can be held against reuse"
                    )
                } else {
                    format!(
                        "GPU pool {pool_wire_name}'s claim table could not be fetched; the next \
                         frame of the pool asks for it again"
                    )
                }
            })?;
        #[cfg(target_os = "linux")]
        let pool_slot = self.the_viewed_frame_import(&frame, claim.pool_slot_key())?;
        #[cfg(target_os = "macos")]
        let pool_slot = self.the_pool_slot_imported(claim.pool_slot_key())?;
        Ok(GpuPoolFrameHeldByAViewer {
            frame,
            #[cfg(target_os = "macos")]
            _iosurface_use_count_claim: crate::apple::IOSurfaceUseCountClaimOfAViewer::claiming(
                Arc::clone(&pool_slot),
            ),
            pool_slot,
            _claim: claim,
        })
    }

    #[cfg(target_os = "linux")]
    fn the_viewed_frame_import(
        &self,
        frame: &GpuPoolSlotFrameDescriptorBody,
        pool_slot_key: &str,
    ) -> Result<Arc<GpuPoolSlotOfAViewer>, String> {
        let imports = self
            .surface_share_connection
            .pool_slot_imports_by_pool_slot();
        let frame_id = published_frame_id_of_pool_slot(pool_slot_key, frame.generation);
        let viewed_frame_imports_held = imports.pool_slot_keys_imported();
        let (reply, received_handles, import_answered) = self.surface_share_connection.exchange(
            &serde_json::json!({
                "op": SURFACE_SHARE_IMPORT_VIEWED_FRAME_OP,
                "surface_id": frame_id,
                SURFACE_SHARE_VIEWED_FRAME_READ_BY_FIELD: self.viewed_frame_read_by.wire_name(),
                SURFACE_SHARE_VIEWED_FRAME_IMPORTS_HELD_FIELD: viewed_frame_imports_held,
                SURFACE_SHARE_FRAME_IMAGE_LAYOUT_FIELD: frame.image_layout,
            }),
            Default::default(),
            |reply| {
                if refusal_the_surface_share_service_answered(reply).is_some() {
                    return None;
                }
                let import_id = reply.get("surface_id")?.as_str()?.to_string();
                if viewed_frame_imports_held.contains(&import_id) {
                    return Some(ViewedFrameImportAnswered::AlreadyHeld(import_id));
                }
                let retired_in_this_reply = pool_slots_a_reply_lists_as_retired(reply)
                    .any(|retired_pool_slot| retired_pool_slot == import_id);
                let fresh_import_reservation = (!retired_in_this_reply)
                    .then(|| imports.reserve_for_a_fresh_import(&import_id));
                Some(ViewedFrameImportAnswered::Fresh {
                    import_id,
                    fresh_import_reservation,
                })
            },
        )?;
        if let Some(refusal) = refusal_the_surface_share_service_answered(&reply) {
            return Err(format!(
                "the surface-share service refused frame {frame_id:?} to this viewer: {refusal}"
            ));
        }
        match import_answered {
            None => Err(format!(
                "the surface-share service answered frame {frame_id:?} without naming the \
                 registration that holds it"
            )),
            Some(ViewedFrameImportAnswered::AlreadyHeld(import_id)) => {
                imports.import_held_for(&import_id).ok_or_else(|| {
                    format!(
                        "the import of {import_id:?} that frame {frame_id:?} lies in was retired \
                         as it was answered; the next frame imports it again"
                    )
                })
            }
            Some(ViewedFrameImportAnswered::Fresh {
                import_id,
                fresh_import_reservation,
            }) => {
                let imported = Arc::new(GpuPoolSlotOfAViewer::from_the_import_reply(
                    &import_id,
                    &reply,
                    received_handles,
                )?);
                if let Some(fresh_import_reservation) = fresh_import_reservation {
                    let _ = fresh_import_reservation.set(Arc::clone(&imported));
                }
                Ok(imported)
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn the_pool_slot_imported(
        &self,
        pool_slot_key: &str,
    ) -> Result<Arc<GpuPoolSlotOfAViewer>, String> {
        let imports = self
            .surface_share_connection
            .pool_slot_imports_by_pool_slot();
        if let Some(held) = imports.import_held_for(pool_slot_key) {
            return Ok(held);
        }
        let (reply, received_handles, fresh_import_reservation) =
            self.surface_share_connection.exchange(
                &serde_json::json!({
                    "op": SURFACE_SHARE_IMPORT_POOL_SLOT_OP,
                    "surface_id": pool_slot_key,
                }),
                Default::default(),
                |reply| {
                    (refusal_the_surface_share_service_answered(reply).is_none()
                        && !pool_slots_a_reply_lists_as_retired(reply)
                            .any(|retired_pool_slot| retired_pool_slot == pool_slot_key))
                    .then(|| imports.reserve_for_a_fresh_import(pool_slot_key))
                },
            )?;
        if let Some(refusal) = refusal_the_surface_share_service_answered(&reply) {
            return Err(format!(
                "the surface-share service refused to import pool slot {pool_slot_key:?}: \
                 {refusal}"
            ));
        }
        let imported = Arc::new(GpuPoolSlotOfAViewer::from_the_import_reply(
            pool_slot_key,
            &reply,
            received_handles,
        )?);
        if let Some(fresh_import_reservation) = fresh_import_reservation {
            let _ = fresh_import_reservation.set(Arc::clone(&imported));
        }
        Ok(imported)
    }
}

/// What the service answered a viewer's ask for a frame with, decided while
/// the reply holds the connection.
#[cfg(target_os = "linux")]
enum ViewedFrameImportAnswered {
    /// A registration this viewer already imported; no handle rode the reply.
    AlreadyHeld(String),
    /// A registration's handles, imported fresh and kept where reserved.
    Fresh {
        import_id: String,
        fresh_import_reservation:
            Option<crate::PoolSlotImportReservation<Arc<GpuPoolSlotOfAViewer>>>,
    },
}
