//! streamlib-py: Python bindings via PyO3

use pyo3::prelude::*;

#[pyfunction]
fn hello() -> PyResult<String> {
    Ok("Hello from streamlib Rust core!".to_string())
}

#[pymodule]
fn streamlib(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(hello, m)?)?;
    Ok(())
}
