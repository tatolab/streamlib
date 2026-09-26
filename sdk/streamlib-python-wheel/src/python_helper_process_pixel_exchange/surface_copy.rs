// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use pyo3::prelude::*;
use pyo3::types::PyDict;

use super::{HelperProcessGpuExchangeClient, escalate_round_trip_to_parent};

impl HelperProcessGpuExchangeClient {
    /// Have the engine copy one surface's pixels into another.
    ///
    /// Returns once the parent's copy has retired: the parent records,
    /// submits and waits before it answers, so the destination's next reader
    /// sees the copied pixels and no timeline value crosses back.
    pub(crate) fn copy_surface_to_surface(
        &self,
        python: Python<'_>,
        source_surface_id: &str,
        destination_surface_id: &str,
    ) -> PyResult<()> {
        let op = PyDict::new(python);
        op.set_item("op", "copy_surface_to_surface")?;
        op.set_item("source_surface_id", source_surface_id)?;
        op.set_item("destination_surface_id", destination_surface_id)?;
        escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        Ok(())
    }
}
