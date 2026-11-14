// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The native half of `tatolab-browser`, imported as `tatolab.browser._native`.
//!
//! The engine never calls anything here: the wheel's `@node.process` hands this module
//! its module host table, and the page opens as a tab in the machine's browser
//! host, whose paints, audio and published objects this module's threads
//! publish through the table with no Python attached.

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("tatolab-browser runs on Linux and macOS");

pub mod browser_host_tab_client;
pub mod page_audio_blocks;
pub mod page_data_messages;
pub mod page_input_messages;
pub mod painted_page_image_frame;
pub mod painted_textures_adopted_by_the_node;

use std::sync::atomic::Ordering;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use tatolab_module_sdk::ModuleHost;

use tatolab_browser_tab_protocol::{PageInputTrack, PageInputTrackKind};

use crate::browser_host_tab_client::{
    BrowserPageConfig, BrowserPagePorts, BrowserPageRendering, PageInputPort,
};

/// A web page rendering as a tab in the machine's browser host, publishing its frames on
/// `video`, its audio on `audio` and its published objects on `events`, and
/// handed every `data` message as a DOM event and every wired input as a track.
#[pyclass(name = "BrowserPageRendering")]
struct PythonBrowserPageRendering {
    rendering: BrowserPageRendering,
}

#[pymethods]
impl PythonBrowserPageRendering {
    #[new]
    #[pyo3(signature = (
        module_host_table,
        video_output_port,
        events_output_port,
        audio_output_port,
        data_input_port,
        video_input_ports,
        audio_input_ports,
        url,
        width,
        height,
        frame_rate,
        transparent_background,
        audio_sample_rate,
        output_pool_rotation_depth,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        python: Python<'_>,
        module_host_table: &Bound<'_, PyAny>,
        video_output_port: String,
        events_output_port: String,
        audio_output_port: String,
        data_input_port: String,
        video_input_ports: Vec<String>,
        audio_input_ports: Vec<String>,
        url: String,
        width: u32,
        height: u32,
        frame_rate: u32,
        transparent_background: bool,
        audio_sample_rate: u32,
        output_pool_rotation_depth: u32,
    ) -> PyResult<Self> {
        let module_host = ModuleHost::from_python_module_host_table_capsule(module_host_table)?;
        let rendering = python
            .detach(|| -> Result<BrowserPageRendering, String> {
                let tracks = video_input_ports
                    .into_iter()
                    .map(|input_port_name| (input_port_name, PageInputTrackKind::Video))
                    .chain(
                        audio_input_ports
                            .into_iter()
                            .map(|input_port_name| (input_port_name, PageInputTrackKind::Audio)),
                    );
                let mut page_input_ports = Vec::new();
                for (input_port_name, kind) in tracks {
                    page_input_ports.push(PageInputPort {
                        input_port: module_host
                            .open_input_port(&input_port_name)
                            .map_err(|refusal| refusal.to_string())?,
                        track: PageInputTrack {
                            input_port_name,
                            kind,
                        },
                    });
                }
                let ports = BrowserPagePorts {
                    video_output_port: module_host
                        .open_output_port(&video_output_port)
                        .map_err(|refusal| refusal.to_string())?,
                    events_output_port: module_host
                        .open_output_port(&events_output_port)
                        .map_err(|refusal| refusal.to_string())?,
                    audio_output_port: module_host
                        .open_output_port(&audio_output_port)
                        .map_err(|refusal| refusal.to_string())?,
                    data_input_port: module_host
                        .open_input_port(&data_input_port)
                        .map_err(|refusal| refusal.to_string())?,
                    page_input_ports,
                    module_host,
                };
                BrowserPageRendering::open(
                    BrowserPageConfig {
                        url,
                        width,
                        height,
                        frame_rate,
                        transparent_background,
                        audio_sample_rate,
                        output_pool_rotation_depth,
                    },
                    ports,
                )
            })
            .map_err(PyRuntimeError::new_err)?;
        Ok(Self { rendering })
    }

    #[getter]
    fn frames_published(&self) -> u64 {
        self.rendering
            .counters()
            .frames_published
            .load(Ordering::Relaxed)
    }

    #[getter]
    fn frames_published_from_painted_textures(&self) -> u64 {
        self.rendering
            .counters()
            .frames_published_from_painted_textures
            .load(Ordering::Relaxed)
    }

    #[getter]
    fn frames_dropped(&self) -> u64 {
        self.rendering
            .counters()
            .frames_dropped
            .load(Ordering::Relaxed)
    }

    #[getter]
    fn audio_blocks_published(&self) -> u64 {
        self.rendering
            .counters()
            .audio_blocks_published
            .load(Ordering::Relaxed)
    }

    #[getter]
    fn events_published(&self) -> u64 {
        self.rendering
            .counters()
            .events_published
            .load(Ordering::Relaxed)
    }

    #[getter]
    fn data_messages_delivered(&self) -> u64 {
        self.rendering
            .counters()
            .data_messages_delivered
            .load(Ordering::Relaxed)
    }

    #[getter]
    fn data_messages_refused(&self) -> u64 {
        self.rendering
            .counters()
            .data_messages_refused
            .load(Ordering::Relaxed)
    }

    #[getter]
    fn page_input_video_frames_delivered(&self) -> u64 {
        self.rendering
            .counters()
            .page_input_video_frames_delivered
            .load(Ordering::Relaxed)
    }

    #[getter]
    fn page_input_audio_blocks_delivered(&self) -> u64 {
        self.rendering
            .counters()
            .page_input_audio_blocks_delivered
            .load(Ordering::Relaxed)
    }

    #[getter]
    fn page_input_chunks_refused(&self) -> u64 {
        self.rendering
            .counters()
            .page_input_chunks_refused
            .load(Ordering::Relaxed)
    }

    #[getter]
    fn browser_host_reconnects(&self) -> u64 {
        self.rendering
            .counters()
            .browser_host_reconnects
            .load(Ordering::Relaxed)
    }

    /// Stop delivering data and close the tab. Idempotent.
    fn close(&mut self, python: Python<'_>) {
        python.detach(|| self.rendering.close());
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PythonBrowserPageRendering>()?;
    Ok(())
}
