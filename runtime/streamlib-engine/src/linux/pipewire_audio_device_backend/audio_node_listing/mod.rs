// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The PipeWire arm's capture devices: the session's audio sources, and each sink's monitor under
//! the `<sink>.monitor` id a capture stream opens it by.

use std::ffi::{CStr, c_char, c_int, c_void};

use crate::core::context::{AudioCaptureDevice, AudioCaptureDeviceKind};
use crate::core::{Error, Result};
use crate::linux::pipewire_runtime_library::{PipeWireLibraryEntryPoints, ShimFailureText};

mod listing_shim {
    use std::ffi::{c_char, c_int, c_void};

    /// `STREAMLIB_PIPEWIRE_MAX_DEFAULT_NODE_NAME_BYTES`.
    pub(super) const MAX_DEFAULT_NODE_NAME_BYTES: usize = 256;

    pub(super) type AudioNodeListed = unsafe extern "C" fn(
        context: *mut c_void,
        node_name: *const c_char,
        node_description: *const c_char,
        media_class: *const c_char,
    );

    unsafe extern "C" {
        pub(super) fn streamlib_pipewire_list_audio_nodes(
            entry_points: *const *mut c_void,
            on_node_listed: AudioNodeListed,
            context: *mut c_void,
            default_sink_name: *mut c_char,
            default_source_name: *mut c_char,
            failure_text: *mut c_char,
            failure_text_capacity: usize,
        ) -> c_int;
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

    fn name_a_person_reads(&self) -> &str {
        self.node_description
            .as_deref()
            .filter(|description| !description.is_empty())
            .unwrap_or(&self.node_name)
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
    /// Every capture device among the nodes: the sources, the default one first, then each
    /// sink's monitor, the default sink's first.
    pub(super) fn capture_devices(&self) -> Vec<AudioCaptureDevice> {
        let inputs = defaults_first(
            self.nodes.iter().filter(|node| node.captures()),
            self.default_source_name.as_deref(),
        )
        .map(|node| AudioCaptureDevice {
            id: node.node_name.clone(),
            name: node.name_a_person_reads().to_owned(),
            kind: AudioCaptureDeviceKind::Input,
        });
        let monitors = defaults_first(
            self.nodes.iter().filter(|node| node.plays()),
            self.default_sink_name.as_deref(),
        )
        .map(|node| AudioCaptureDevice {
            id: monitor_device_id_of(&node.node_name),
            name: format!("Monitor of {}", node.name_a_person_reads()),
            kind: AudioCaptureDeviceKind::OutputMonitor,
        });
        inputs.chain(monitors).collect()
    }

    /// The default sink's monitor id, where the default sink is among the nodes.
    pub(super) fn default_output_monitor_device_id(&self) -> Option<String> {
        let default_sink_name = self.default_sink_name.as_deref()?;
        self.nodes
            .iter()
            .find(|node| node.plays() && node.node_name == default_sink_name)
            .map(|node| monitor_device_id_of(&node.node_name))
    }
}

fn monitor_device_id_of(sink_name: &str) -> String {
    format!("{sink_name}{MONITOR_DEVICE_ID_SUFFIX}")
}

/// `nodes` with the one named `default_name` moved to the front, the rest in registry order.
fn defaults_first<'a>(
    nodes: impl Iterator<Item = &'a PipeWireAudioNode>,
    default_name: Option<&str>,
) -> impl Iterator<Item = &'a PipeWireAudioNode> {
    let mut nodes: Vec<&PipeWireAudioNode> = nodes.collect();
    if let Some(default_position) = nodes
        .iter()
        .position(|node| Some(node.node_name.as_str()) == default_name)
    {
        let default_node = nodes.remove(default_position);
        nodes.insert(0, default_node);
    }
    nodes.into_iter()
}

/// What the shim calls on PipeWire's thread loop for each audio node, with the loop's lock held
/// while the caller waits.
///
/// Must not unwind: this is a plain `extern "C"` boundary.
unsafe extern "C" fn record_a_listed_audio_node(
    context: *mut c_void,
    node_name: *const c_char,
    node_description: *const c_char,
    media_class: *const c_char,
) {
    // SAFETY: the context is the listing's own `Vec`, which only this callback touches until the
    // shim returns; each string is NUL-terminated and valid for the length of the call, and
    // only the description may be null.
    unsafe {
        let nodes = &mut *context.cast::<Vec<PipeWireAudioNode>>();
        let owned = |text: *const c_char| CStr::from_ptr(text).to_string_lossy().into_owned();
        nodes.push(PipeWireAudioNode {
            node_name: owned(node_name),
            node_description: (!node_description.is_null()).then(|| owned(node_description)),
            media_class: owned(media_class),
        });
    }
}

/// Ask the session for its audio nodes and default nodes, in one round trip.
pub(super) fn audio_nodes_of_the_session(
    entry_points: &PipeWireLibraryEntryPoints,
) -> Result<PipeWireAudioNodesListed> {
    let mut nodes: Vec<PipeWireAudioNode> = Vec::new();
    let mut default_sink_name = [0u8; listing_shim::MAX_DEFAULT_NODE_NAME_BYTES];
    let mut default_source_name = [0u8; listing_shim::MAX_DEFAULT_NODE_NAME_BYTES];
    let mut failure_text = ShimFailureText::new();
    let (failure_text_ptr, failure_text_capacity) = failure_text.as_shim_out_parameters();
    // SAFETY: the table is fully resolved; `nodes` is touched only by the callback until the shim
    // returns; the default-name buffers are the capacity the shim writes into, and the failure
    // buffer's pointer and capacity come from the buffer itself.
    let refused: c_int = unsafe {
        listing_shim::streamlib_pipewire_list_audio_nodes(
            entry_points.as_ptr(),
            record_a_listed_audio_node,
            (&raw mut nodes).cast::<c_void>(),
            default_sink_name.as_mut_ptr().cast::<c_char>(),
            default_source_name.as_mut_ptr().cast::<c_char>(),
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
        nodes,
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
