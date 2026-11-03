// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of `tatolab-browser`, imported as `tatolab.browser._native`.
//!
//! The engine never calls anything here: the wheel's `@node` hands this module
//! its module host table, and CEF's own threads paint, play and publish
//! through it with no Python attached.

#[cfg(not(target_os = "linux"))]
compile_error!(
    "tatolab-browser paints through host table 1.5's Linux upload; its macOS half follows"
);

pub mod cef_browser_process;
pub mod page_audio_blocks;
pub mod page_data_messages;
pub mod video_frame_record;

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use tatolab_module_sdk::ModuleHost;

use crate::cef_browser_process::{BrowserPageConfig, BrowserPagePorts, BrowserPageRendering};

/// A web page rendering offscreen in this helper, publishing its frames on
/// `video`, its audio on `audio` and its published objects on `events`, and
/// handed every `data` message as a DOM event.
#[pyclass(name = "BrowserPageRendering")]
struct PythonBrowserPageRendering {
    rendering: BrowserPageRendering,
}

#[pymethods]
impl PythonBrowserPageRendering {
    #[new]
    #[pyo3(signature = (
        module_host_table,
        cef_runtime_directory,
        video_output_port,
        events_output_port,
        audio_output_port,
        data_input_port,
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
        cef_runtime_directory: PathBuf,
        video_output_port: String,
        events_output_port: String,
        audio_output_port: String,
        data_input_port: String,
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
                    module_host,
                };
                BrowserPageRendering::open(
                    &cef_runtime_directory,
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

    /// Stop delivering data and close the page. Idempotent.
    fn close(&mut self, python: Python<'_>) {
        python.detach(|| self.rendering.close());
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PythonBrowserPageRendering>()?;
    module.add(
        "BROWSER_SUBPROCESS_EXECUTABLE_NAME",
        cef_browser_process::BROWSER_SUBPROCESS_EXECUTABLE_NAME,
    )?;
    Ok(())
}
