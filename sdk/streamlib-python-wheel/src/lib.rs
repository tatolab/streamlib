// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native half of the streamlib wheel — the extension module CPython
//! imports as `streamlib._engine`.

use pyo3::prelude::*;

mod python_added_processor;
mod python_bag_conversion;
mod python_logging;
mod python_processor_declaration;
mod python_processor_host;
mod python_processor_link_data_access;
mod python_processor_registration;
mod python_runtime_lifecycle;

pub use python_runtime_lifecycle::PythonRuntimeHandle;

#[pymodule]
fn _engine(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PythonRuntimeHandle>()?;
    module.add_class::<python_added_processor::PythonAddedProcessor>()?;
    module.add_class::<python_added_processor::PythonProcessorOutputPortReference>()?;
    module.add_class::<python_added_processor::PythonProcessorInputPortReference>()?;
    module.add_class::<python_processor_link_data_access::PythonProcessorLinkDataAccess>()?;
    module.add_function(wrap_pyfunction!(
        python_logging::media_clock_now_ns,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(python_logging::log_event, module)?)?;
    Ok(())
}
