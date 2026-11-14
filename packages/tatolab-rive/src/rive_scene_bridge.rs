// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The C++ bridge to rive-runtime (`native/tatolab_rive_external_renderer_bridge.h`):
//! a `.riv` scene on a render context built from the external-renderer
//! device's raw handles, recording each frame into the command buffer the
//! device begins.

use std::ffi::{CStr, CString, c_char};

use streamlib_consumer_rhi::{
    ExternalRendererFrameRawVulkanHandles, ExternalRendererRawVulkanHandles, VulkanLayout,
};

use crate::view_model_data_message::{
    ViewModelPropertyAssignment, ViewModelPropertyTarget, ViewModelPropertyType,
};

const BRIDGE_STATUS_OK: i32 = 0;
const VIEW_MODEL_PROPERTY_ABSENT: i32 = -1;
const BRIDGE_ERROR_MESSAGE_CAPACITY: usize = 1024;

#[repr(C)]
struct TatolabRiveScene {
    _opaque: [u8; 0],
}

unsafe extern "C" {
    fn tatolab_rive_scene_create(
        device: *const ExternalRendererRawVulkanHandles,
        riv_bytes: *const u8,
        riv_byte_count: usize,
        artboard_name: *const c_char,
        state_machine_name: *const c_char,
        error_message: *mut c_char,
        error_message_capacity: usize,
    ) -> *mut TatolabRiveScene;
    fn tatolab_rive_scene_destroy(scene: *mut TatolabRiveScene);
    fn tatolab_rive_scene_artboard_name(scene: *const TatolabRiveScene) -> *const c_char;
    fn tatolab_rive_scene_state_machine_name(scene: *const TatolabRiveScene) -> *const c_char;
    fn tatolab_rive_scene_view_model_property_type(
        scene: *const TatolabRiveScene,
        property_path: *const c_char,
    ) -> i32;
    fn tatolab_rive_scene_set_number(
        scene: *mut TatolabRiveScene,
        property_path: *const c_char,
        value: f32,
    ) -> i32;
    fn tatolab_rive_scene_set_string(
        scene: *mut TatolabRiveScene,
        property_path: *const c_char,
        value_utf8: *const c_char,
        value_byte_count: usize,
    ) -> i32;
    fn tatolab_rive_scene_set_boolean(
        scene: *mut TatolabRiveScene,
        property_path: *const c_char,
        value: u32,
    ) -> i32;
    fn tatolab_rive_scene_set_color(
        scene: *mut TatolabRiveScene,
        property_path: *const c_char,
        argb: u32,
    ) -> i32;
    fn tatolab_rive_scene_set_enum(
        scene: *mut TatolabRiveScene,
        property_path: *const c_char,
        value_utf8: *const c_char,
        value_byte_count: usize,
    ) -> i32;
    fn tatolab_rive_scene_fire_trigger(
        scene: *mut TatolabRiveScene,
        property_path: *const c_char,
    ) -> i32;
    fn tatolab_rive_scene_advance_and_record(
        scene: *mut TatolabRiveScene,
        elapsed_seconds: f32,
        frame: *const ExternalRendererFrameRawVulkanHandles,
        target_layout_after_recording: *mut i32,
        error_message: *mut c_char,
        error_message_capacity: usize,
    ) -> i32;
}

/// A `.riv` file's artboard and state machine, playing on rive-runtime's
/// Vulkan render context. Drop it only once the device's queue holds none of
/// its work.
pub struct RiveSceneOnExternalRendererDevice {
    scene: *mut TatolabRiveScene,
}

impl RiveSceneOnExternalRendererDevice {
    /// Build the render context on `device` and instance the named artboard
    /// and state machine, or the file's defaults for `None`.
    pub fn create(
        device: &ExternalRendererRawVulkanHandles,
        riv_bytes: &[u8],
        artboard_name: Option<&str>,
        state_machine_name: Option<&str>,
    ) -> Result<Self, String> {
        let artboard_name = c_string_of("artboard name", artboard_name.unwrap_or_default())?;
        let state_machine_name =
            c_string_of("state machine name", state_machine_name.unwrap_or_default())?;
        let mut error_message = [0 as c_char; BRIDGE_ERROR_MESSAGE_CAPACITY];
        // SAFETY: every pointer is valid for the call; the bridge copies what
        // it keeps.
        let scene = unsafe {
            tatolab_rive_scene_create(
                device,
                riv_bytes.as_ptr(),
                riv_bytes.len(),
                artboard_name.as_ptr(),
                state_machine_name.as_ptr(),
                error_message.as_mut_ptr(),
                error_message.len(),
            )
        };
        if scene.is_null() {
            return Err(text_of(&error_message));
        }
        Ok(Self { scene })
    }

    /// The artboard playing.
    pub fn artboard_name(&self) -> String {
        // SAFETY: the bridge answers a NUL-terminated string the scene owns.
        unsafe { CStr::from_ptr(tatolab_rive_scene_artboard_name(self.scene)) }
            .to_string_lossy()
            .into_owned()
    }

    /// The state machine playing; `None` when the artboard has none and is
    /// drawn still.
    pub fn state_machine_name(&self) -> Option<String> {
        // SAFETY: the bridge answers a NUL-terminated string the scene owns.
        let name = unsafe { CStr::from_ptr(tatolab_rive_scene_state_machine_name(self.scene)) }
            .to_string_lossy()
            .into_owned();
        (!name.is_empty()).then_some(name)
    }

    /// Advance by `elapsed_seconds` and record the frame into `frame`'s
    /// command buffer, answering the layout the commands leave the target in.
    pub fn advance_and_record(
        &mut self,
        elapsed_seconds: f32,
        frame: &ExternalRendererFrameRawVulkanHandles,
    ) -> Result<VulkanLayout, String> {
        let mut target_layout_after_recording = 0_i32;
        let mut error_message = [0 as c_char; BRIDGE_ERROR_MESSAGE_CAPACITY];
        // SAFETY: the frame's handles are live until it is submitted, and
        // every out-pointer is valid for the call.
        let status = unsafe {
            tatolab_rive_scene_advance_and_record(
                self.scene,
                elapsed_seconds,
                frame,
                &mut target_layout_after_recording,
                error_message.as_mut_ptr(),
                error_message.len(),
            )
        };
        if status != BRIDGE_STATUS_OK {
            return Err(text_of(&error_message));
        }
        Ok(VulkanLayout(target_layout_after_recording))
    }
}

impl ViewModelPropertyTarget for RiveSceneOnExternalRendererDevice {
    fn view_model_property_type(&self, property_path: &str) -> Option<ViewModelPropertyType> {
        let property_path = CString::new(property_path).ok()?;
        // SAFETY: the path is NUL-terminated and outlives the call.
        let data_type = unsafe {
            tatolab_rive_scene_view_model_property_type(self.scene, property_path.as_ptr())
        };
        (data_type != VIEW_MODEL_PROPERTY_ABSENT)
            .then(|| ViewModelPropertyType::from_rive_data_type(data_type))
    }

    fn assign_view_model_property(
        &mut self,
        property_path: &str,
        assignment: &ViewModelPropertyAssignment,
    ) -> bool {
        let Ok(property_path) = CString::new(property_path) else {
            return false;
        };
        let path = property_path.as_ptr();
        // SAFETY: the path and any text are valid for the call, and text is
        // passed with its length.
        let status = unsafe {
            match assignment {
                ViewModelPropertyAssignment::Number(value) => {
                    tatolab_rive_scene_set_number(self.scene, path, *value)
                }
                ViewModelPropertyAssignment::String(value) => tatolab_rive_scene_set_string(
                    self.scene,
                    path,
                    value.as_ptr().cast(),
                    value.len(),
                ),
                ViewModelPropertyAssignment::Boolean(value) => {
                    tatolab_rive_scene_set_boolean(self.scene, path, u32::from(*value))
                }
                ViewModelPropertyAssignment::Color(argb) => {
                    tatolab_rive_scene_set_color(self.scene, path, *argb)
                }
                ViewModelPropertyAssignment::Enum(value) => tatolab_rive_scene_set_enum(
                    self.scene,
                    path,
                    value.as_ptr().cast(),
                    value.len(),
                ),
                ViewModelPropertyAssignment::FireTrigger => {
                    tatolab_rive_scene_fire_trigger(self.scene, path)
                }
            }
        };
        status == BRIDGE_STATUS_OK
    }
}

impl Drop for RiveSceneOnExternalRendererDevice {
    fn drop(&mut self) {
        // SAFETY: the scene came from `tatolab_rive_scene_create` and is
        // destroyed once.
        unsafe { tatolab_rive_scene_destroy(self.scene) };
    }
}

fn c_string_of(what: &str, text: &str) -> Result<CString, String> {
    CString::new(text).map_err(|_| format!("the {what} {text:?} holds a NUL byte"))
}

fn text_of(error_message: &[c_char]) -> String {
    let bytes: Vec<u8> = error_message
        .iter()
        .take_while(|&&byte| byte != 0)
        .map(|&byte| byte as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}
