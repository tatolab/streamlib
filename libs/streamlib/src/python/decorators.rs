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

    // For pre-built processors (Camera, Display) - stores config dict
    #[pyo3(get)]
    pub config: Option<Py<PyDict>>,

    // For Python processors - stores the Python class
    #[pyo3(get)]
    pub python_class: Option<Py<PyAny>>,

    #[pyo3(get)]
    pub input_port_names: Vec<String>,
    #[pyo3(get)]
    pub output_port_names: Vec<String>,

    // Schema metadata for AI discovery
    #[pyo3(get)]
    pub description: Option<String>,
    #[pyo3(get)]
    pub usage_context: Option<String>,
    #[pyo3(get)]
    pub tags: Vec<String>,
}

impl Clone for ProcessorProxy {
    fn clone(&self) -> Self {
        Python::with_gil(|py| {
            Self {
                processor_name: self.processor_name.clone(),
                processor_type: self.processor_type.clone(),
                config: self.config.as_ref().map(|c| c.clone_ref(py)),
                python_class: self.python_class.as_ref().map(|c| c.clone_ref(py)),
                input_port_names: self.input_port_names.clone(),
                output_port_names: self.output_port_names.clone(),
                description: self.description.clone(),
                usage_context: self.usage_context.clone(),
                tags: self.tags.clone(),
            }
        })
    }
}

#[pymethods]
impl ProcessorProxy {
    #[new]
    #[pyo3(signature = (processor_name, processor_type, input_port_names, output_port_names, config=None, python_class=None, description=None, usage_context=None, tags=None))]
    fn new(
        processor_name: String,
        processor_type: String,
        input_port_names: Vec<String>,
        output_port_names: Vec<String>,
        config: Option<Py<PyDict>>,
        python_class: Option<Py<PyAny>>,
        description: Option<String>,
        usage_context: Option<String>,
        tags: Option<Vec<String>>,
    ) -> Self {
        Self {
            processor_name,
            processor_type,
            config,
            python_class,
            input_port_names,
            output_port_names,
            description,
            usage_context,
            tags: tags.unwrap_or_default(),
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
            input_port_names=[],
            output_port_names=['video'],
            config=config
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
            input_port_names=['video'],
            output_port_names=[],
            config=config
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
/// Wraps a Python class as a dynamic processor.
/// The class should define InputPorts and/or OutputPorts nested classes
/// (at least one is required), and implement a process(self, tick) method.
///
/// - Generators (sources): Only OutputPorts, no InputPorts
/// - Sinks: Only InputPorts, no OutputPorts
/// - Filters: Both InputPorts and OutputPorts
///
/// # Example
/// ```python
/// @processor(
///     description="Applies a custom filter to video frames",
///     usage_context="Use when you need custom frame processing",
///     tags=["filter", "video", "custom"]
/// )
/// class MyFilter:
///     class InputPorts:
///         video = StreamInput(VideoFrame)
///
///     class OutputPorts:
///         video = StreamOutput(VideoFrame)
///
///     def process(self, tick):
///         frame = self.input_ports().video.read_latest()
///         if frame:
///             self.output_ports().video.write(frame)
/// ```
#[pyfunction]
#[pyo3(signature = (description=None, usage_context=None, tags=None))]
pub fn processor(
    py: Python<'_>,
    description: Option<String>,
    usage_context: Option<String>,
    tags: Option<Vec<String>>,
) -> PyResult<Py<PyAny>> {
    // Create a Python decorator function that returns ProcessorProxy
    let decorator_code = r#"
def _make_decorator(description, usage_context, tags, ProcessorProxy):
    # Define marker classes for syntax sugar (not actually used at runtime)
    class StreamInput:
        def __init__(self, type_hint=None):
            pass
        def __repr__(self):
            return "StreamInput(VideoFrame)"

    class StreamOutput:
        def __init__(self, type_hint=None):
            pass
        def __repr__(self):
            return "StreamOutput(VideoFrame)"

    class VideoFrame:
        pass

    def _processor_decorator(cls):
        # Validate it's a class
        if not isinstance(cls, type):
            raise TypeError("@processor can only decorate classes, not functions")

        # Parse InputPorts (optional - for generators/sources)
        input_port_names = []
        if hasattr(cls, 'InputPorts'):
            for name in dir(cls.InputPorts):
                if not name.startswith('_'):
                    input_port_names.append(name)

        # Parse OutputPorts (optional - for sinks)
        output_port_names = []
        if hasattr(cls, 'OutputPorts'):
            for name in dir(cls.OutputPorts):
                if not name.startswith('_'):
                    output_port_names.append(name)

        # Create ProcessorProxy with Python class
        return ProcessorProxy(
            processor_name=cls.__name__,
            processor_type='PythonProcessor',
            input_port_names=input_port_names,
            output_port_names=output_port_names,
            python_class=cls,
            description=description,
            usage_context=usage_context,
            tags=tags or []
        )

    # Inject marker classes into decorator's closure so they're available
    _processor_decorator.StreamInput = StreamInput
    _processor_decorator.StreamOutput = StreamOutput
    _processor_decorator.VideoFrame = VideoFrame

    return _processor_decorator
"#;

    let locals = PyDict::new_bound(py);

    // Get ProcessorProxy class
    let proxy_class = py.get_type_bound::<ProcessorProxy>();
    locals.set_item("ProcessorProxy", &proxy_class)?;

    py.run_bound(decorator_code, None, Some(&locals))?;
    let decorator = locals.get_item("_make_decorator")?.unwrap().call((
        description,
        usage_context,
        tags,
        &proxy_class,
    ), None)?;

    Ok(decorator.into())
}
