// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use pyo3::prelude::*;
use pyo3::types::PyDict;

#[cfg(target_os = "linux")]
use super::hand_a_release_handle_to_the_release_worker;
use super::{HelperProcessGpuExchangeClient, escalate_round_trip_to_parent};

/// The `ColorInfo` attributes the conversion reads, each an H.273 axis name.
const COLOR_INFO_AXIS_ATTRIBUTES: [&str; 4] = ["primaries", "transfer", "matrix", "range"];

impl HelperProcessGpuExchangeClient {
    /// Have the engine convert one YCbCr pixel buffer into an RGBA texture
    /// with its colour converter, returning once the conversion has retired.
    ///
    /// The parent resolves an adopted source once and keeps it for this
    /// helper's later conversions, so this client marks each adoption it
    /// names and owes the parent a release when that adoption goes away.
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
        op.set_item("ycbcr_side_color", source_color)?;
        if let Some(named_by_a_conversion) = self
            .foreign_adoptions_and_whether_a_conversion_named_them
            .lock()
            .get_mut(source_surface_id)
        {
            *named_by_a_conversion = true;
        }
        escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        Ok(())
    }

    /// Record that this helper adopted the foreign DMA-BUF `surface_id`.
    #[cfg(target_os = "linux")]
    pub(crate) fn record_a_foreign_adoption(&self, surface_id: &str) {
        self.foreign_adoptions_and_whether_a_conversion_named_them
            .lock()
            .insert(surface_id.to_string(), false);
    }

    /// Forget the adoption `surface_id`, handing the parent the release of
    /// the conversion source it resolved for it; nothing more when no
    /// conversion ever named it.
    #[cfg(target_os = "linux")]
    pub(crate) fn release_the_color_conversion_source_the_parent_resolved(
        &self,
        python: Python<'_>,
        surface_id: &str,
    ) -> PyResult<()> {
        let named_by_a_conversion = self
            .foreign_adoptions_and_whether_a_conversion_named_them
            .lock()
            .remove(surface_id);
        if named_by_a_conversion != Some(true) {
            return Ok(());
        }
        hand_a_release_handle_to_the_release_worker(
            python,
            &self.release_to_parent_without_waiting,
            surface_id,
        )
    }
}
