//! Decorator functions for processor registration

use pyo3::prelude::*;
use pyo3::types::PyDict;

/// @camera_processor decorator
///
/// Binds to the Rust CameraProcessor implementation.
///
/// # Example
/// ```python
/// @camera_processor(device_id=0)
/// def camera():
///     pass  # Empty - uses Rust implementation
/// ```
#[pyfunction]
#[pyo3(signature = (**kwargs))]
pub fn camera_processor(
    py: Python<'_>,
    kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyAny>> {
    // Create a Python decorator function
    let decorator_code = r#"
def _camera_processor_decorator(func):
    import types

    # Create outputs dictionary
    outputs = {
        'video': None  # Will be populated by runtime
    }

    # Attach metadata to function
    func.__streamlib_type__ = 'CameraProcessor'
    func.__streamlib_config__ = kwargs
    func.__streamlib_is_prebuilt__ = True
    func.inputs = {}
    func.outputs = outputs

    return func

_camera_processor_decorator
"#;

    let locals = PyDict::new_bound(py);
    locals.set_item("kwargs", kwargs.unwrap_or(&PyDict::new_bound(py)))?;

    py.run_bound(decorator_code, None, Some(&locals))?;
    let decorator = locals.get_item("_camera_processor_decorator")?.unwrap();

    Ok(decorator.into())
}

/// @display_processor decorator
///
/// Binds to the Rust DisplayProcessor implementation.
///
/// # Example
/// ```python
/// @display_processor(title="Camera Feed")
/// def display():
///     pass  # Empty - uses Rust implementation
/// ```
#[pyfunction]
#[pyo3(signature = (**kwargs))]
pub fn display_processor(
    py: Python<'_>,
    kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyAny>> {
    // Create a Python decorator function
    let decorator_code = r#"
def _display_processor_decorator(func):
    import types

    # Create inputs dictionary
    inputs = {
        'video': None  # Will be populated by runtime
    }

    # Attach metadata to function
    func.__streamlib_type__ = 'DisplayProcessor'
    func.__streamlib_config__ = kwargs
    func.__streamlib_is_prebuilt__ = True
    func.inputs = inputs
    func.outputs = {}

    return func

_display_processor_decorator
"#;

    let locals = PyDict::new_bound(py);
    locals.set_item("kwargs", kwargs.unwrap_or(&PyDict::new_bound(py)))?;

    py.run_bound(decorator_code, None, Some(&locals))?;
    let decorator = locals.get_item("_display_processor_decorator")?.unwrap();

    Ok(decorator.into())
}

/// @processor decorator
///
/// Wraps a Python function as a dynamic processor.
/// The function will be called for each frame.
///
/// # Example
/// ```python
/// @processor(inputs=["video"], outputs=["video"])
/// def my_filter(frame):
///     # Process the frame
///     return frame
/// ```
#[pyfunction]
#[pyo3(signature = (inputs=None, outputs=None, **kwargs))]
pub fn processor(
    py: Python<'_>,
    inputs: Option<Vec<String>>,
    outputs: Option<Vec<String>>,
    kwargs: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyAny>> {
    // Create a Python decorator function
    let decorator_code = r#"
def _processor_decorator(func):
    import types

    # Create inputs and outputs dictionaries
    inputs_dict = {name: None for name in inputs}
    outputs_dict = {name: None for name in outputs}

    # Attach metadata to function
    func.__streamlib_type__ = 'PythonProcessor'
    func.__streamlib_config__ = kwargs
    func.__streamlib_is_prebuilt__ = False
    func.__streamlib_function__ = func
    func.inputs = inputs_dict
    func.outputs = outputs_dict

    return func

_processor_decorator
"#;

    let locals = PyDict::new_bound(py);
    locals.set_item("inputs", inputs.unwrap_or_default())?;
    locals.set_item("outputs", outputs.unwrap_or_default())?;
    locals.set_item("kwargs", kwargs.unwrap_or(&PyDict::new_bound(py)))?;

    py.run_bound(decorator_code, None, Some(&locals))?;
    let decorator = locals.get_item("_processor_decorator")?.unwrap();

    Ok(decorator.into())
}
