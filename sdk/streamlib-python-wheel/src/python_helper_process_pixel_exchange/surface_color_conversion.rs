// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use pyo3::prelude::*;
use pyo3::types::PyDict;

use super::{
    HelperProcessGpuExchangeClient, escalate_round_trip_to_parent,
    hand_a_release_handle_to_the_release_worker,
};

/// The `ColorInfo` attributes the conversion reads, each an H.273 axis name.
const COLOR_INFO_AXIS_ATTRIBUTES: [&str; 4] = ["primaries", "transfer", "matrix", "range"];

impl HelperProcessGpuExchangeClient {
    /// Have the engine convert one YCbCr pixel buffer into an RGBA texture
    /// with its colour converter, returning once the conversion has retired.
    ///
    /// The parent resolves the source once and keeps it for this helper's
    /// later conversions, so this client remembers each source it named and
    /// owes the parent a release when that source goes away.
    pub(crate) fn convert_surface_into(
        &self,
        python: Python<'_>,
        source_surface_id: &str,
        destination_surface_id: &str,
        source_color_info: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let source_color = PyDict::new(python);
        if let Some(source_color_info) = source_color_info {
            for axis in COLOR_INFO_AXIS_ATTRIBUTES {
                let axis_name = source_color_info.getattr(axis)?;
                if !axis_name.is_none() {
                    source_color.set_item(axis, axis_name.extract::<String>()?)?;
                }
            }
        }
        let op = PyDict::new(python);
        op.set_item("op", "convert_surface_into")?;
        op.set_item("source_surface_id", source_surface_id)?;
        op.set_item("destination_surface_id", destination_surface_id)?;
        op.set_item("source_color", source_color)?;
        self.color_conversion_sources_the_parent_resolved
            .lock()
            .insert(source_surface_id.to_string());
        escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        Ok(())
    }

    /// Hand the parent the release a conversion source it resolved for this
    /// helper is owed, once `surface_id` is going away; nothing when no
    /// conversion ever named it.
    pub(crate) fn release_the_color_conversion_source_the_parent_resolved(
        &self,
        python: Python<'_>,
        surface_id: &str,
    ) -> PyResult<()> {
        if !self
            .color_conversion_sources_the_parent_resolved
            .lock()
            .remove(surface_id)
        {
            return Ok(());
        }
        hand_a_release_handle_to_the_release_worker(
            python,
            &self.release_to_parent_without_waiting,
            surface_id,
        )
    }
}
