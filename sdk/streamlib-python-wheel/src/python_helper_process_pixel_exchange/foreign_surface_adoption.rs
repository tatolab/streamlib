// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use super::HelperProcessGpuExchangeClient;

impl HelperProcessGpuExchangeClient {
    /// Remove an adopted foreign surface's registration — a DMA-BUF's or an
    /// IOSurface's — dropping the service's hold on the foreign memory. The
    /// one place the `release` op is spelled for adoptions.
    pub(crate) fn unregister_foreign_surface(&self, surface_id: &str) -> PyResult<()> {
        let (response, _no_handles) = self.surface_share_request(&serde_json::json!({
            "op": "release",
            "surface_id": surface_id,
            "runtime_id": self.foreign_surface_registration_runtime_id,
        }))?;
        match response.get("success").and_then(|value| value.as_bool()) {
            Some(true) => Ok(()),
            _ => Err(PyRuntimeError::new_err(format!(
                "the surface-share service did not release adopted surface {surface_id:?}"
            ))),
        }
    }
}
