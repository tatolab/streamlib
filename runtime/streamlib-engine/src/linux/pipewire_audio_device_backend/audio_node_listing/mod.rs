// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The PipeWire arm's capture devices: the session's audio sources, and each sink's monitor under
//! the `<sink>.monitor` id a capture stream opens it by.

use std::ffi::{CStr, CString, c_char, c_int, c_void};

use crate::core::context::{
    AudioCaptureDevice, AudioCaptureDeviceKind, AudioCaptureDeviceListing, moved_to_the_front,
};
use crate::core::{Error, Result};
use crate::linux::pipewire_runtime_library::{PipeWireLibraryEntryPoints, ShimFailureText};

mod listing_shim {
    use std::ffi::{c_char, c_int, c_void};

    pub(super) type AudioNodeListed = unsafe extern "C" fn(
        context: *mut c_void,
        registry_id: u32,
        node_name: *const c_char,
        node_description: *const c_char,
        media_class: *const c_char,
    );

    /// What the listing calls, on PipeWire's thread loop, for each global the session removes.
    pub(super) type GlobalRemoved = unsafe extern "C" fn(context: *mut c_void, registry_id: u32);

    unsafe extern "C" {
        pub(super) fn streamlib_pipewire_list_audio_nodes(
            entry_points: *const *mut c_void,
            on_node_listed: AudioNodeListed,
            on_global_removed: GlobalRemoved,
            context: *mut c_void,
            default_sink_name: *mut c_char,
            default_source_name: *mut c_char,
            default_node_name_capacity: usize,
            failure_text: *mut c_char,
            failure_text_capacity: usize,
        ) -> c_int;
        pub(super) fn streamlib_pipewire_max_monitored_sink_name_bytes() -> usize;
        pub(super) fn streamlib_pipewire_sink_name_length_of_monitor_device_id(
            device_id: *const c_char,
        ) -> usize;
    }

    #[cfg(test)]
    unsafe extern "C" {
        pub(super) fn streamlib_pipewire_node_name_of_default_metadata_value(
            value: *const c_char,
            node_name: *mut c_char,
            node_name_capacity: usize,
        ) -> bool;
    }
}

/// What a capture `device_id` appends to a sink's name to open that sink's monitor, as the
/// audio shim reads it.
const MONITOR_DEVICE_ID_SUFFIX: &str = ".monitor";

/// The room each default node name is read into. A longer name reads as no default.
const DEFAULT_NODE_NAME_CAPACITY: usize = 256;

/// Whether a capture stream can open `sink_name`'s monitor: the audio shim refuses a sink name
/// that does not fit its buffer, so such a sink's monitor is not listed.
/// Whether the audio shim reads `device_id` as a sink's monitor, as it does any id ending in
/// `.monitor`. A source named that way cannot be opened by its own name, so it is not listed.
fn opens_as_a_monitor(device_id: &str) -> bool {
    let Ok(device_id) = CString::new(device_id) else {
        return false;
    };
    // SAFETY: `device_id` is NUL-terminated and outlives the call.
    unsafe {
        listing_shim::streamlib_pipewire_sink_name_length_of_monitor_device_id(device_id.as_ptr())
            > 0
    }
}

fn a_monitor_of_it_opens(sink_name: &str) -> bool {
    // SAFETY: the shim returns a compile-time constant.
    sink_name.len() < unsafe { listing_shim::streamlib_pipewire_max_monitored_sink_name_bytes() }
}

/// One audio node the session's registry holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PipeWireAudioNode {
    /// `node.name`: what a capture stream targets.
    pub(super) node_name: String,
    /// `node.description`, else `node.nick`.
    pub(super) node_description: Option<String>,
    /// `media.class`, `Audio/Source`, `Audio/Sink` and the like.
    pub(super) media_class: String,
}

impl PipeWireAudioNode {
    fn captures(&self) -> bool {
        self.media_class.starts_with("Audio/Source") || self.media_class == "Audio/Duplex"
    }

    fn plays(&self) -> bool {
        self.media_class.starts_with("Audio/Sink") || self.media_class == "Audio/Duplex"
    }
}

/// The session's audio nodes and the default sink and source its `default` metadata names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct PipeWireAudioNodesListed {
    pub(super) nodes: Vec<PipeWireAudioNode>,
    pub(super) default_sink_name: Option<String>,
    pub(super) default_source_name: Option<String>,
}

impl PipeWireAudioNodesListed {
    /// Every capture device among the nodes: the sources, the default one first, then the monitor
    /// of each sink a capture stream can open, the default sink's first, and that one's id as the
    /// default output's monitor.
    pub(super) fn capture_device_listing(&self) -> AudioCaptureDeviceListing {
        let inputs = moved_to_the_front(
            self.nodes
                .iter()
                .filter(|node| node.captures() && !opens_as_a_monitor(&node.node_name))
                .collect(),
            |node| Some(node.node_name.as_str()) == self.default_source_name.as_deref(),
        )
        .into_iter()
        .map(|node| {
            AudioCaptureDevice::named_or_by_id(
                node.node_name.clone(),
                node.node_description.clone(),
                AudioCaptureDeviceKind::Input,
            )
        });
        let sinks = moved_to_the_front(
            self.nodes
                .iter()
                .filter(|node| node.plays() && a_monitor_of_it_opens(&node.node_name))
                .collect::<Vec<_>>(),
            |node| Some(node.node_name.as_str()) == self.default_sink_name.as_deref(),
        );
        let default_output_monitor_device_id = sinks
            .first()
            .filter(|sink| Some(sink.node_name.as_str()) == self.default_sink_name.as_deref())
            .map(|sink| monitor_device_id_of(&sink.node_name));
        let monitors = sinks.into_iter().map(|sink| {
            let sink = AudioCaptureDevice::named_or_by_id(
                sink.node_name.clone(),
                sink.node_description.clone(),
                AudioCaptureDeviceKind::OutputMonitor,
            );
            AudioCaptureDevice {
                id: monitor_device_id_of(&sink.id),
                name: format!("Monitor of {}", sink.name),
                kind: sink.kind,
            }
        });
        AudioCaptureDeviceListing {
            devices: inputs.chain(monitors).collect(),
            default_output_monitor_device_id,
        }
    }
}

fn monitor_device_id_of(sink_name: &str) -> String {
    format!("{sink_name}{MONITOR_DEVICE_ID_SUFFIX}")
}

/// The audio nodes announced so far, each by its registry id, in the order they were announced.
type AudioNodesByRegistryId = Vec<(u32, PipeWireAudioNode)>;

/// What the shim calls on PipeWire's thread loop for each audio node, with the loop's lock held
/// while the caller waits.
///
/// Must not unwind: this is a plain `extern "C"` boundary.
unsafe extern "C" fn record_a_listed_audio_node(
    context: *mut c_void,
    registry_id: u32,
    node_name: *const c_char,
    node_description: *const c_char,
    media_class: *const c_char,
) {
    // SAFETY: the context is the listing's own nodes, which only these callbacks touch until the
    // shim returns; each string is NUL-terminated and valid for the length of the call, and
    // only the description may be null.
    unsafe {
        let nodes = &mut *context.cast::<AudioNodesByRegistryId>();
        let owned = |text: *const c_char| CStr::from_ptr(text).to_string_lossy().into_owned();
        nodes.push((
            registry_id,
            PipeWireAudioNode {
                node_name: owned(node_name),
                node_description: (!node_description.is_null()).then(|| owned(node_description)),
                media_class: owned(media_class),
            },
        ));
    }
}

/// What the shim calls on PipeWire's thread loop for each global the session removes while it
/// is listed: a node gone before the listing ends is not listed.
///
/// Must not unwind: this is a plain `extern "C"` boundary.
unsafe extern "C" fn forget_a_removed_global(context: *mut c_void, registry_id: u32) {
    // SAFETY: as for `record_a_listed_audio_node`.
    let nodes = unsafe { &mut *context.cast::<AudioNodesByRegistryId>() };
    nodes.retain(|(listed_id, _)| *listed_id != registry_id);
}

/// Ask the session for its audio nodes and default nodes, in one round trip.
pub(super) fn audio_nodes_of_the_session(
    entry_points: &PipeWireLibraryEntryPoints,
) -> Result<PipeWireAudioNodesListed> {
    let mut nodes: AudioNodesByRegistryId = Vec::new();
    let mut default_sink_name = [0u8; DEFAULT_NODE_NAME_CAPACITY];
    let mut default_source_name = [0u8; DEFAULT_NODE_NAME_CAPACITY];
    let mut failure_text = ShimFailureText::new();
    let (failure_text_ptr, failure_text_capacity) = failure_text.as_shim_out_parameters();
    // SAFETY: the table is fully resolved; `nodes` is touched only by the callback until the shim
    // returns; the default-name buffers each hold the capacity passed beside them, and the failure
    // buffer's pointer and capacity come from the buffer itself.
    let refused: c_int = unsafe {
        listing_shim::streamlib_pipewire_list_audio_nodes(
            entry_points.as_ptr(),
            record_a_listed_audio_node,
            forget_a_removed_global,
            (&raw mut nodes).cast::<c_void>(),
            default_sink_name.as_mut_ptr().cast::<c_char>(),
            default_source_name.as_mut_ptr().cast::<c_char>(),
            DEFAULT_NODE_NAME_CAPACITY,
            failure_text_ptr,
            failure_text_capacity,
        )
    };
    if refused != 0 {
        return Err(Error::Runtime(format!(
            "PipeWire could not list its audio devices: {}",
            failure_text.read()
        )));
    }
    Ok(PipeWireAudioNodesListed {
        nodes: nodes.into_iter().map(|(_, node)| node).collect(),
        default_sink_name: node_name_written_into(&default_sink_name),
        default_source_name: node_name_written_into(&default_source_name),
    })
}

fn node_name_written_into(buffer: &[u8]) -> Option<String> {
    CStr::from_bytes_until_nul(buffer)
        .ok()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests;
