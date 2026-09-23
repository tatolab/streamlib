// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::python_helper_process_pixel_exchange::{
    HelperProcessGpuExchangeClient, escalate_round_trip_to_parent,
};
use crate::python_processor_owned_window::{
    PythonProcessorOwnedWindowEvents, SurfaceNamedForTheWindowsPresentLoop, wire_name_of_primaries,
    wire_name_of_transfer,
};

use super::response_field;

impl HelperProcessGpuExchangeClient {
    /// Mint a window this processor owns, and take back the id every other
    /// present-class op names.
    ///
    /// Refused outside `setup()`, and refused with the pump's own account
    /// when the process can get no window at all — which is the refusal a
    /// Python author wraps in `try/except` for an optional window.
    pub(crate) fn create_processor_owned_window(
        &self,
        python: Python<'_>,
        window_title: &str,
        initial_width_in_logical_pixels: u32,
        initial_height_in_logical_pixels: u32,
    ) -> PyResult<String> {
        let op = PyDict::new(python);
        op.set_item("op", "create_processor_owned_window")?;
        op.set_item("window_title", window_title)?;
        op.set_item(
            "initial_width_in_logical_pixels",
            initial_width_in_logical_pixels,
        )?;
        op.set_item(
            "initial_height_in_logical_pixels",
            initial_height_in_logical_pixels,
        )?;
        let response =
            escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        response_field(&response, "handle_id")?.extract()
    }

    /// Name the frame a window shows next, without waiting for it to be shown.
    ///
    /// Answers whether the engine has closed the window, which is how a closed
    /// window stays a no-op rather than an error.
    pub(crate) fn show_surface_on_processor_owned_window(
        &self,
        python: Python<'_>,
        window_id: &str,
        named_surface: &SurfaceNamedForTheWindowsPresentLoop,
    ) -> PyResult<bool> {
        let op = PyDict::new(python);
        op.set_item("op", "show_surface_on_processor_owned_window")?;
        op.set_item("window_id", window_id)?;
        op.set_item("surface_id", &named_surface.surface_id)?;
        op.set_item(
            "source_width_in_pixels",
            named_surface.source_width_in_pixels,
        )?;
        op.set_item(
            "source_height_in_pixels",
            named_surface.source_height_in_pixels,
        )?;
        // Omitted rather than sent as null, matching the host's own encoding
        // — serde reads either as `None`, so this is about the document being
        // recognisable, not about behaviour. What *is* load-bearing is one
        // level up: a frame describing neither colour axis carries no
        // `color_traits_of_frame` at all, because a description with both axes
        // empty renegotiates the window's swapchain to the default pick.
        if let Some(producer_published_texture_layout) =
            named_surface.producer_published_texture_layout
        {
            op.set_item(
                "producer_published_texture_layout",
                producer_published_texture_layout,
            )?;
        }
        if let Some(color_traits_of_frame) = named_surface.color_traits_of_frame {
            if let Some(primaries) = color_traits_of_frame.primaries {
                op.set_item(
                    "color_primaries_of_frame",
                    wire_name_of_primaries(primaries),
                )?;
            }
            if let Some(transfer) = color_traits_of_frame.transfer {
                op.set_item("color_transfer_of_frame", wire_name_of_transfer(transfer))?;
            }
        }
        if let Some(hdr_static_metadata_of_frame) = &named_surface.hdr_static_metadata_of_frame {
            op.set_item(
                "hdr_static_metadata_of_frame",
                hdr_static_metadata_wire_entry(python, hdr_static_metadata_of_frame)?,
            )?;
        }
        let response =
            escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        processor_owned_window_is_closed_in(&response)
    }

    /// Take a window's coalesced state: its current extent, whether the user
    /// asked to close it since the last drain, and whether it has closed.
    pub(crate) fn drain_processor_owned_window_events(
        &self,
        python: Python<'_>,
        window_id: &str,
    ) -> PyResult<PythonProcessorOwnedWindowEvents> {
        let op = PyDict::new(python);
        op.set_item("op", "drain_processor_owned_window_events")?;
        op.set_item("window_id", window_id)?;
        let response =
            escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        Ok(PythonProcessorOwnedWindowEvents::drained(
            response_field(&response, "width")?.extract()?,
            response_field(&response, "height")?.extract()?,
            response_field(&response, "close_requested_by_user")?.extract()?,
            processor_owned_window_is_closed_in(&response)?,
        ))
    }

    /// Release a window this processor owns. Never an error for one already
    /// closed, and answers what is true rather than what was asked for: a
    /// window server still holding the present thread past the close's grace
    /// window leaves the window open, and says so.
    pub(crate) fn close_processor_owned_window(
        &self,
        python: Python<'_>,
        window_id: &str,
    ) -> PyResult<bool> {
        let op = PyDict::new(python);
        op.set_item("op", "close_processor_owned_window")?;
        op.set_item("window_id", window_id)?;
        let response =
            escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        processor_owned_window_is_closed_in(&response)
    }
}

/// Whether the parent's answer says the window has closed. Every present-class
/// op carries it, and reading it off each answer is what keeps the object's own
/// closed state current without a poll of its own.
fn processor_owned_window_is_closed_in(response: &Bound<'_, PyAny>) -> PyResult<bool> {
    response_field(response, "processor_owned_window_is_closed")?.extract()
}

/// The HDR sidecar as the wire's own nested document. Chromaticities travel as
/// two-element arrays, in the f32 units the driver takes.
fn hdr_static_metadata_wire_entry<'py>(
    python: Python<'py>,
    hdr_static_metadata_of_frame: &streamlib::sdk::color::HdrStaticMetadata,
) -> PyResult<Bound<'py, PyDict>> {
    let entry = PyDict::new(python);
    entry.set_item(
        "display_primary_red",
        hdr_static_metadata_of_frame.display_primary_red,
    )?;
    entry.set_item(
        "display_primary_green",
        hdr_static_metadata_of_frame.display_primary_green,
    )?;
    entry.set_item(
        "display_primary_blue",
        hdr_static_metadata_of_frame.display_primary_blue,
    )?;
    entry.set_item("white_point", hdr_static_metadata_of_frame.white_point)?;
    entry.set_item(
        "min_luminance_cd_m2",
        hdr_static_metadata_of_frame.min_luminance_cd_m2,
    )?;
    entry.set_item(
        "max_luminance_cd_m2",
        hdr_static_metadata_of_frame.max_luminance_cd_m2,
    )?;
    entry.set_item(
        "max_content_light_level",
        hdr_static_metadata_of_frame.max_content_light_level,
    )?;
    entry.set_item(
        "max_frame_average_light_level",
        hdr_static_metadata_of_frame.max_frame_average_light_level,
    )?;
    Ok(entry)
}
