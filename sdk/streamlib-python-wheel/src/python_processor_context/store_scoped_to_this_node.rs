// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyFloat, PyList, PyTuple};

use crate::python_bag_conversion::{json_value_to_python_object, python_object_to_json_value};

/// The values this node keeps in its project's store, by key, as `ctx.store()`.
///
/// A value outlives the run and a restart of the runtime.
#[pyclass(name = "StoreScopedToThisNode", module = "tatolab.stream", frozen)]
pub(crate) struct PythonStoreScopedToThisNode {
    pub(super) escalate_request_to_parent: Option<Py<PyAny>>,
}

#[pymethods]
impl PythonStoreScopedToThisNode {
    /// The value kept under `key`, or `None` when the node keeps none.
    fn read<'py>(&self, python: Python<'py>, key: &str) -> PyResult<Bound<'py, PyAny>> {
        let op = self.node_store_op(python, "read_node_store_key", key)?;
        let answer = self.round_trip_to_parent(python, &op)?;
        match answer.cast::<PyDict>()?.get_item("node_store_value")? {
            Some(node_store_value) => Ok(node_store_value),
            None => Ok(python.None().into_bound(python)),
        }
    }

    /// Keep `value`, anything JSON can hold, under `key`, replacing what was there.
    fn write(&self, python: Python<'_>, key: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let op = self.node_store_op(python, "write_node_store_key", key)?;
        refuse_a_float_json_cannot_hold(value)?;
        let value_as_json = python_object_to_json_value(value)?;
        op.set_item(
            "value",
            json_value_to_python_object(python, &value_as_json)?,
        )?;
        self.round_trip_to_parent(python, &op)?;
        Ok(())
    }

    /// Forget the value kept under `key`; forgetting an absent key does nothing.
    fn remove(&self, python: Python<'_>, key: &str) -> PyResult<()> {
        let op = self.node_store_op(python, "remove_node_store_key", key)?;
        self.round_trip_to_parent(python, &op)?;
        Ok(())
    }
}

impl PythonStoreScopedToThisNode {
    fn node_store_op<'py>(
        &self,
        python: Python<'py>,
        op_name: &str,
        key: &str,
    ) -> PyResult<Bound<'py, PyDict>> {
        let op = PyDict::new(python);
        op.set_item("op", op_name)?;
        op.set_item("key", key)?;
        Ok(op)
    }

    fn round_trip_to_parent<'py>(
        &self,
        python: Python<'py>,
        op: &Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.escalate_request_to_parent
            .as_ref()
            .ok_or_else(|| {
                PyRuntimeError::new_err(
                    "the node's store lives in its project, held by the parent, and this context \
                     has no route to the parent",
                )
            })?
            .bind(python)
            .call1((op,))
    }
}

/// Refuse a NaN or an infinity anywhere in `value`, which the shared JSON conversion
/// would otherwise keep as `null`.
fn refuse_a_float_json_cannot_hold(value: &Bound<'_, PyAny>) -> PyResult<()> {
    if let Ok(float) = value.cast::<PyFloat>() {
        if !float.value().is_finite() {
            return Err(PyValueError::new_err(format!(
                "a store keeps what JSON can hold, and JSON holds no {}",
                float.value()
            )));
        }
    } else if let Ok(mapping) = value.cast::<PyDict>() {
        for (_, item) in mapping.iter() {
            refuse_a_float_json_cannot_hold(&item)?;
        }
    } else if let Ok(sequence) = value.cast::<PyList>() {
        for item in sequence.iter() {
            refuse_a_float_json_cannot_hold(&item)?;
        }
    } else if let Ok(sequence) = value.cast::<PyTuple>() {
        for item in sequence.iter() {
            refuse_a_float_json_cannot_hold(&item)?;
        }
    }
    Ok(())
}
