// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The table's virtual camera verbs: PipeWire camera nodes the engine's camera
//! door registers in this helper for one node's native code, each fed YUY2
//! rows the module already converted.

use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use tatolab_module_sdk::module_host_table_abi::{
    MODULE_HOST_TABLE_STATUS_ENDED, MODULE_HOST_TABLE_STATUS_OK, ModuleHostTableBytes,
    ModuleHostTableErrorMessage, ModuleHostTableStatus, ModuleHostTableUtf8,
    ModuleHostTableVirtualCameraStreamHandle,
};

use super::audio_capture_streams::locked;
use super::{node_host_of, refuse};

#[cfg(target_os = "linux")]
use streamlib::sdk::engine::linux_pipewire_camera::{
    PipeWireCameraFramePresentation, PipeWireCameraNode,
};
#[cfg(target_os = "linux")]
use tatolab_module_sdk::module_host_table_abi::{
    MODULE_HOST_TABLE_VIRTUAL_CAMERA_FRAME_EVERY_BUFFER_IS_HELD_BY_THE_CONSUMER,
    MODULE_HOST_TABLE_VIRTUAL_CAMERA_FRAME_NO_CONSUMER_IS_WATCHING,
    MODULE_HOST_TABLE_VIRTUAL_CAMERA_FRAME_PUBLISHED,
};

/// The camera nodes one node's native code opened, by handle.
#[derive(Default)]
pub(super) struct VirtualCameraStreamsOpenedForOneNode {
    /// Slot `n` is handle `n + 1`; a closed slot stays empty, so a stale handle
    /// names nothing rather than a later camera.
    opened_cameras: Mutex<Vec<Option<Arc<VirtualCameraOpenedForAModule>>>>,
}

pub(super) struct VirtualCameraOpenedForAModule {
    #[cfg(target_os = "linux")]
    pipewire_camera_node: Mutex<PipeWireCameraNode>,
}

impl VirtualCameraStreamsOpenedForOneNode {
    #[cfg(target_os = "linux")]
    fn open(
        &self,
        node_id: &str,
        camera_name: &str,
    ) -> Result<ModuleHostTableVirtualCameraStreamHandle, String> {
        let pipewire_camera_node =
            PipeWireCameraNode::open(camera_name).map_err(|refusal| refusal.to_string())?;
        tracing::info!(
            node_id,
            camera_name,
            "a module opened a PipeWire virtual camera"
        );
        let mut opened_cameras = locked(&self.opened_cameras);
        opened_cameras.push(Some(Arc::new(VirtualCameraOpenedForAModule {
            pipewire_camera_node: Mutex::new(pipewire_camera_node),
        })));
        Ok(ModuleHostTableVirtualCameraStreamHandle {
            value: opened_cameras.len() as u64,
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn open(
        &self,
        _node_id: &str,
        camera_name: &str,
    ) -> Result<ModuleHostTableVirtualCameraStreamHandle, String> {
        Err(format!(
            "VirtualCameraSink \"{camera_name}\": the PipeWire camera door is Linux only, and \
             this is {}",
            std::env::consts::OS
        ))
    }

    pub(super) fn opened_camera(
        &self,
        camera: ModuleHostTableVirtualCameraStreamHandle,
    ) -> Result<Arc<VirtualCameraOpenedForAModule>, String> {
        usize::try_from(camera.value)
            .ok()
            .and_then(|value| value.checked_sub(1))
            .and_then(|slot| locked(&self.opened_cameras).get(slot).cloned().flatten())
            .ok_or_else(|| {
                format!(
                    "virtual camera handle {} names no camera this table has open",
                    camera.value
                )
            })
    }

    fn close(&self, camera: ModuleHostTableVirtualCameraStreamHandle) {
        let closed_camera = usize::try_from(camera.value)
            .ok()
            .and_then(|value| value.checked_sub(1))
            .and_then(|slot| {
                locked(&self.opened_cameras)
                    .get_mut(slot)
                    .and_then(Option::take)
            });
        drop(closed_camera);
    }
}

impl VirtualCameraOpenedForAModule {
    #[cfg(target_os = "linux")]
    fn offer_extent(&self, width: u32, height: u32, frames_per_second: u32) -> Result<(), String> {
        locked(&self.pipewire_camera_node)
            .offer_extent(
                width,
                height,
                Some(frames_per_second).filter(|&rate| rate > 0),
            )
            .map_err(|refusal| refusal.to_string())
    }

    #[cfg(target_os = "linux")]
    fn write_yuyv_frame(
        &self,
        yuyv_rows: &[u8],
        stride_bytes: u32,
        timestamp_ns: i64,
    ) -> Result<u32, String> {
        let presentation = locked(&self.pipewire_camera_node)
            .write_yuyv_frame(yuyv_rows, stride_bytes as usize, timestamp_ns)
            .map_err(|refusal| refusal.to_string())?;
        Ok(match presentation {
            PipeWireCameraFramePresentation::Published => {
                MODULE_HOST_TABLE_VIRTUAL_CAMERA_FRAME_PUBLISHED
            }
            PipeWireCameraFramePresentation::NoConsumerIsWatching => {
                MODULE_HOST_TABLE_VIRTUAL_CAMERA_FRAME_NO_CONSUMER_IS_WATCHING
            }
            PipeWireCameraFramePresentation::EveryBufferIsHeldByTheConsumer => {
                MODULE_HOST_TABLE_VIRTUAL_CAMERA_FRAME_EVERY_BUFFER_IS_HELD_BY_THE_CONSUMER
            }
        })
    }

    #[cfg(target_os = "linux")]
    fn failure_that_ended_the_camera(&self) -> Option<String> {
        locked(&self.pipewire_camera_node).failure()
    }

    #[cfg(not(target_os = "linux"))]
    fn offer_extent(&self, _: u32, _: u32, _: u32) -> Result<(), String> {
        Err("the PipeWire camera door is Linux only".to_string())
    }

    #[cfg(not(target_os = "linux"))]
    fn write_yuyv_frame(&self, _: &[u8], _: u32, _: i64) -> Result<u32, String> {
        Err("the PipeWire camera door is Linux only".to_string())
    }

    #[cfg(not(target_os = "linux"))]
    fn failure_that_ended_the_camera(&self) -> Option<String> {
        None
    }
}

pub(super) unsafe extern "C" fn virtual_camera_stream_open(
    node_host: *mut c_void,
    camera_name: ModuleHostTableUtf8,
    opened_camera: *mut ModuleHostTableVirtualCameraStreamHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    // SAFETY: the SDK hands a slot live for the call, or null.
    let Some(opened_camera) = (unsafe { opened_camera.as_mut() }) else {
        return unsafe {
            refuse(
                error_message,
                "a virtual camera open is handed a slot for the camera",
            )
        };
    };
    // SAFETY: the SDK hands a view live for the call.
    let camera_name = match unsafe { camera_name.as_str() } {
        Ok("") => {
            return unsafe { refuse(error_message, "a virtual camera is opened by name") };
        }
        Ok(camera_name) => camera_name,
        Err(not_utf8) => {
            return unsafe {
                refuse(
                    error_message,
                    &format!("a camera name is UTF-8: {not_utf8}"),
                )
            };
        }
    };
    match node_host
        .virtual_camera_streams
        .open(&node_host.node_id, camera_name)
    {
        Ok(handle) => {
            *opened_camera = handle;
            MODULE_HOST_TABLE_STATUS_OK
        }
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

pub(super) unsafe extern "C" fn virtual_camera_stream_offer_extent(
    node_host: *mut c_void,
    camera: ModuleHostTableVirtualCameraStreamHandle,
    width: u32,
    height: u32,
    frames_per_second: u32,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    let offered = node_host
        .virtual_camera_streams
        .opened_camera(camera)
        .and_then(|opened_camera| opened_camera.offer_extent(width, height, frames_per_second));
    match offered {
        Ok(()) => MODULE_HOST_TABLE_STATUS_OK,
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

pub(super) unsafe extern "C" fn virtual_camera_stream_write_yuyv_frame(
    node_host: *mut c_void,
    camera: ModuleHostTableVirtualCameraStreamHandle,
    yuyv_rows: ModuleHostTableBytes,
    stride_bytes: u32,
    timestamp_ns: i64,
    frame_presentation: *mut u32,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    // SAFETY: the SDK hands a slot live for the call, or null.
    let Some(frame_presentation) = (unsafe { frame_presentation.as_mut() }) else {
        return unsafe {
            refuse(
                error_message,
                "a virtual camera write is handed a slot for what became of the frame",
            )
        };
    };
    // SAFETY: the SDK hands rows live for the call.
    let yuyv_rows = unsafe { yuyv_rows.as_slice() };
    let written = node_host
        .virtual_camera_streams
        .opened_camera(camera)
        .and_then(|opened_camera| {
            opened_camera.write_yuyv_frame(yuyv_rows, stride_bytes, timestamp_ns)
        });
    match written {
        Ok(presentation) => {
            *frame_presentation = presentation;
            MODULE_HOST_TABLE_STATUS_OK
        }
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

pub(super) unsafe extern "C" fn virtual_camera_stream_liveness(
    node_host: *mut c_void,
    camera: ModuleHostTableVirtualCameraStreamHandle,
    error_message: *mut ModuleHostTableErrorMessage,
) -> ModuleHostTableStatus {
    let node_host = unsafe { node_host_of(node_host) };
    match node_host.virtual_camera_streams.opened_camera(camera) {
        Ok(opened_camera) => match opened_camera.failure_that_ended_the_camera() {
            None => MODULE_HOST_TABLE_STATUS_OK,
            Some(failure) => {
                // SAFETY: the SDK hands a buffer live for the call, or null.
                if let Some(error_message) = unsafe { error_message.as_mut() } {
                    error_message.write(&failure);
                }
                MODULE_HOST_TABLE_STATUS_ENDED
            }
        },
        Err(refusal) => unsafe { refuse(error_message, &refusal) },
    }
}

pub(super) unsafe extern "C" fn virtual_camera_stream_close(
    node_host: *mut c_void,
    camera: ModuleHostTableVirtualCameraStreamHandle,
) {
    let node_host = unsafe { node_host_of(node_host) };
    node_host.virtual_camera_streams.close(camera);
}
