use pyo3::prelude::*;

#[pyclass(name = "Port", module = "streamlib")]
#[derive(Clone)]
pub struct ProcessorPort {
    #[pyo3(get)]
    pub processor_name: String,
    #[pyo3(get)]
    pub port_name: String,
    #[pyo3(get)]
    pub is_input: bool,
}

impl ProcessorPort {
    pub fn create(processor_name: String, port_name: String, is_input: bool) -> Self {
        Self {
            processor_name,
            port_name,
            is_input,
        }
    }
}

#[pymethods]
impl ProcessorPort {
    #[new]
    fn new(processor_name: String, port_name: String, is_input: bool) -> Self {
        Self {
            processor_name,
            port_name,
            is_input,
        }
    }

    fn __repr__(&self) -> String {
        let direction = if self.is_input { "input" } else { "output" };
        format!(
            "Port({}.{}, {})",
            self.processor_name, self.port_name, direction
        )
    }
}
