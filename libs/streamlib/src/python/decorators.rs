//! Decorator functions for processor registration

use pyo3::prelude::*;
use pyo3::types::PyDict;
use super::port::ProcessorPort;

/// Python wrapper for output/input ports collection
#[pyclass(module = "streamlib")]
#[derive(Clone)]
pub struct PortsProxy {
    processor_name: String,
    port_names: Vec<String>,
    is_input: bool,
}

#[pymethods]
impl PortsProxy {
    fn __getattr__(&self, _py: Python<'_>, name: String) -> PyResult<ProcessorPort> {
        if self.port_names.contains(&name) {
            Ok(ProcessorPort::create(self.processor_name.clone(), name, self.is_input))
        } else {
            Err(pyo3::exceptions::PyAttributeError::new_err(
                format!("Port '{}' not found. Available ports: {:?}", name, self.port_names)
            ))
        }
    }

    fn __repr__(&self) -> String {
        let direction = if self.is_input { "InputPorts" } else { "OutputPorts" };
        format!("{}({})", direction, self.port_names.join(", "))
    }
}

/// Python wrapper for a processor (returned by decorators)
#[pyclass(module = "streamlib")]
pub struct ProcessorProxy {
    #[pyo3(get)]
    pub processor_name: String,
    #[pyo3(get)]
    pub processor_type: String,
    pub config: Py<PyDict>,
    input_port_names: Vec<String>,
    output_port_names: Vec<String>,
}

impl Clone for ProcessorProxy {
    fn clone(&self) -> Self {
        Python::with_gil(|py| {
            Self {
                processor_name: self.processor_name.clone(),
                processor_type: self.processor_type.clone(),
                config: self.config.clone_ref(py),
                input_port_names: self.input_port_names.clone(),
                output_port_names: self.output_port_names.clone(),
            }
        })
    }
}

#[pymethods]
impl ProcessorProxy {
    #[new]
    #[pyo3(signature = (processor_name, processor_type, config, input_port_names, output_port_names))]
    fn new(
        processor_name: String,
        processor_type: String,
        config: Py<PyDict>,
        input_port_names: Vec<String>,
        output_port_names: Vec<String>,
    ) -> Self {
        Self {
            processor_name,
            processor_type,
            config,
            input_port_names,
            output_port_names,
        }
    }

    fn output_ports(&self, _py: Python<'_>) -> PyResult<PortsProxy> {
        Ok(PortsProxy {
            processor_name: self.processor_name.clone(),
            port_names: self.output_port_names.clone(),
            is_input: false,
        })
    }

    fn input_ports(&self, _py: Python<'_>) -> PyResult<PortsProxy> {
        Ok(PortsProxy {
            processor_name: self.processor_name.clone(),
            port_names: self.input_port_names.clone(),
            is_input: true,
        })
    }

    fn __repr__(&self) -> String {
        format!("{}(name={})", self.processor_type, self.processor_name)
    }
}

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
    // Create a Python decorator function that returns ProcessorProxy
    let decorator_code = r#"
def _make_decorator(config, ProcessorProxy):
    def _camera_processor_decorator(func):
        # Get processor name from function
        processor_name = func.__name__

        # Create ProcessorProxy with camera processor metadata
        return ProcessorProxy(
            processor_name=processor_name,
            processor_type='CameraProcessor',
            config=config,
            input_port_names=[],
            output_port_names=['video']
        )
    return _camera_processor_decorator
"#;

    let locals = PyDict::new_bound(py);
    locals.set_item("kwargs", kwargs.unwrap_or(&PyDict::new_bound(py)))?;

    // Get ProcessorProxy class
    let proxy_class = py.get_type_bound::<ProcessorProxy>();
    locals.set_item("ProcessorProxy", &proxy_class)?;

    py.run_bound(decorator_code, None, Some(&locals))?;
    let decorator = locals.get_item("_make_decorator")?.unwrap().call((
        kwargs.unwrap_or(&PyDict::new_bound(py)),
        &proxy_class,
    ), None)?;

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
    // Create a Python decorator function that returns ProcessorProxy
    let decorator_code = r#"
def _make_decorator(config, ProcessorProxy):
    def _display_processor_decorator(func):
        # Get processor name from function
        processor_name = func.__name__

        # Create ProcessorProxy with display processor metadata
        return ProcessorProxy(
            processor_name=processor_name,
            processor_type='DisplayProcessor',
            config=config,
            input_port_names=['video'],
            output_port_names=[]
        )
    return _display_processor_decorator
"#;

    let locals = PyDict::new_bound(py);
    locals.set_item("kwargs", kwargs.unwrap_or(&PyDict::new_bound(py)))?;

    // Get ProcessorProxy class
    let proxy_class = py.get_type_bound::<ProcessorProxy>();
    locals.set_item("ProcessorProxy", &proxy_class)?;

    py.run_bound(decorator_code, None, Some(&locals))?;
    let decorator = locals.get_item("_make_decorator")?.unwrap().call((
        kwargs.unwrap_or(&PyDict::new_bound(py)),
        &proxy_class,
    ), None)?;

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
    // Create a Python decorator function that returns ProcessorProxy
    let decorator_code = r#"
def _make_decorator(input_names, output_names, config, ProcessorProxy):
    def _processor_decorator(func):
        # Get processor name from function
        processor_name = func.__name__

        # Store the function in config for later execution
        config['__python_function__'] = func

        # Create ProcessorProxy with Python processor metadata
        return ProcessorProxy(
            processor_name=processor_name,
            processor_type='PythonProcessor',
            config=config,
            input_port_names=input_names,
            output_port_names=output_names
        )
    return _processor_decorator
"#;

    let locals = PyDict::new_bound(py);
    let inputs_list = inputs.unwrap_or_default();
    let outputs_list = outputs.unwrap_or_default();
    let empty_dict = PyDict::new_bound(py);
    let config_dict = kwargs.unwrap_or(&empty_dict);

    locals.set_item("inputs", &inputs_list)?;
    locals.set_item("outputs", &outputs_list)?;
    locals.set_item("kwargs", config_dict)?;

    // Get ProcessorProxy class
    let proxy_class = py.get_type_bound::<ProcessorProxy>();
    locals.set_item("ProcessorProxy", &proxy_class)?;

    py.run_bound(decorator_code, None, Some(&locals))?;
    let decorator = locals.get_item("_make_decorator")?.unwrap().call((
        inputs_list,
        outputs_list,
        config_dict,
        &proxy_class,
    ), None)?;

    Ok(decorator.into())
}
