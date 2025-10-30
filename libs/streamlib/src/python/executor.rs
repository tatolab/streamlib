//! Python code executor - creates processors from Python code
//!
//! This module handles executing Python code and extracting processor instances.
//! Used by MCP for dynamic processor creation, but kept separate for testing/reuse.

use pyo3::prelude::*;
use crate::core::{StreamProcessor, StreamError, Result};
use super::PythonProcessor;

/// Create a StreamProcessor from Python code
///
/// This function:
/// 1. Executes the Python code in an isolated environment
/// 2. Looks for a decorated function/class (ProcessorProxy)
/// 3. Extracts metadata and creates appropriate processor
/// 4. Returns a boxed StreamProcessor ready to add to runtime
///
/// # Arguments
/// * `code` - Python source code containing a decorated processor
///
/// # Returns
/// Box<dyn StreamProcessor> that can be added to runtime
///
/// # Example
/// ```ignore
/// let code = r#"
/// @processor(description="Custom filter")
/// class MyFilter:
///     class InputPorts:
///         video = StreamInput(VideoFrame)
///     class OutputPorts:
///         video = StreamOutput(VideoFrame)
///     def process(self, tick):
///         frame = self.input_ports().video.read_latest()
///         if frame:
///             self.output_ports().video.write(frame)
/// "#;
///
/// let processor = create_processor_from_code(code)?;
/// runtime.add_processor_runtime(processor).await?;
/// ```
#[cfg(feature = "python-embed")]
pub fn create_processor_from_code(code: &str) -> Result<Box<dyn StreamProcessor>> {
    Python::with_gil(|py| -> Result<Box<dyn StreamProcessor>> {
        // 1. Register streamlib module into sys.modules so imports work
        // (only needed for python-embed mode, not extension-module mode)
        let streamlib_module = pyo3::types::PyModule::new_bound(py, "streamlib")
            .map_err(|e| StreamError::Configuration(
                format!("Failed to create streamlib module: {}", e)
            ))?;

        crate::python::register_python_module(&streamlib_module)
            .map_err(|e| StreamError::Configuration(
                format!("Failed to register streamlib module: {}", e)
            ))?;

        // Add streamlib to sys.modules
        py.import_bound("sys")
            .map_err(|e| StreamError::Configuration(
                format!("Failed to import sys: {}", e)
            ))?
            .getattr("modules")
            .map_err(|e| StreamError::Configuration(
                format!("Failed to get sys.modules: {}", e)
            ))?
            .set_item("streamlib", streamlib_module)
            .map_err(|e| StreamError::Configuration(
                format!("Failed to add streamlib to sys.modules: {}", e)
            ))?;

        // 2. Execute the user's code with proper globals/locals context
        // Use same dict for globals and locals so imports are visible to class bodies
        let namespace = pyo3::types::PyDict::new_bound(py);
        let builtins = py.import_bound("builtins")
            .map_err(|e| StreamError::Configuration(format!("Failed to import builtins: {}", e)))?;
        namespace.set_item("__builtins__", builtins)
            .map_err(|e| StreamError::Configuration(format!("Failed to set __builtins__: {}", e)))?;

        py.run_bound(code, Some(&namespace), Some(&namespace))
            .map_err(|e| StreamError::Configuration(
                format!("Failed to execute Python code: {}", e)
            ))?;

        // 3. Extract the ProcessorProxy from namespace (find the decorated function/class)
        let proxy = namespace.values()
            .iter()
            .find(|v| {
                // Check if this value has processor_name attribute (indicates ProcessorProxy)
                v.hasattr("processor_name").unwrap_or(false)
            })
            .ok_or_else(|| StreamError::Configuration(
                "Python code did not define a processor (no decorated function found)".to_string()
            ))?;

        // 4. Extract ProcessorProxy metadata
        let processor_name: String = proxy.getattr("processor_name")
            .map_err(|e| StreamError::Configuration(
                format!("Invalid processor: {}", e)
            ))?
            .extract()
            .map_err(|e| StreamError::Configuration(
                format!("Invalid processor_name: {}", e)
            ))?;

        let processor_type: String = proxy.getattr("processor_type")
            .map_err(|e| StreamError::Configuration(
                format!("Invalid processor: {}", e)
            ))?
            .extract()
            .map_err(|e| StreamError::Configuration(
                format!("Invalid processor_type: {}", e)
            ))?;

        // 5. Create the appropriate processor based on type
        // For Python processors, extract the python_class
        let python_class = proxy.getattr("python_class")
            .ok()
            .and_then(|c| if c.is_none() { None } else { Some(c.into()) });

        if let Some(python_class) = python_class {
            // Custom Python processor
            let input_ports: Vec<String> = proxy.getattr("input_port_names")
                .map_err(|e| StreamError::Configuration(format!("Missing input_port_names: {}", e)))?
                .extract()
                .map_err(|e| StreamError::Configuration(format!("Invalid input_port_names: {}", e)))?;
            let output_ports: Vec<String> = proxy.getattr("output_port_names")
                .map_err(|e| StreamError::Configuration(format!("Missing output_port_names: {}", e)))?
                .extract()
                .map_err(|e| StreamError::Configuration(format!("Invalid output_port_names: {}", e)))?;
            let description: Option<String> = proxy.getattr("description").ok().and_then(|d| d.extract().ok());
            let usage_context: Option<String> = proxy.getattr("usage_context").ok().and_then(|u| u.extract().ok());
            let tags: Vec<String> = proxy.getattr("tags").ok().and_then(|t| t.extract().ok()).unwrap_or_default();

            let py_processor = PythonProcessor::new(
                python_class,
                processor_name,
                input_ports,
                output_ports,
                description,
                usage_context,
                tags,
            )?;

            Ok(Box::new(py_processor) as Box<dyn StreamProcessor>)
        } else {
            // Pre-built processor (Camera, Display) - extract config and create Rust processor
            let config_dict = proxy.getattr("config")
                .ok()
                .and_then(|c| if c.is_none() { None } else { Some(c) });

            match processor_type.as_str() {
                #[cfg(any(target_os = "macos", target_os = "ios"))]
                "CameraProcessor" => {
                    use crate::apple::main_thread::execute_on_main_thread;
                    use crate::apple::processors::AppleCameraProcessor;

                    let device_id = config_dict
                        .and_then(|c| c.get_item("device_id").ok())
                        .and_then(|d| d.extract::<String>().ok());

                    execute_on_main_thread(move || {
                        let p = if let Some(device_id) = device_id {
                            AppleCameraProcessor::with_device_id(&device_id)?
                        } else {
                            AppleCameraProcessor::new()?
                        };
                        Ok(Box::new(p) as Box<dyn StreamProcessor>)
                    })
                }

                #[cfg(any(target_os = "macos", target_os = "ios"))]
                "DisplayProcessor" => {
                    use crate::apple::main_thread::execute_on_main_thread;
                    use crate::apple::processors::AppleDisplayProcessor;

                    execute_on_main_thread(|| {
                        let p = AppleDisplayProcessor::new()?;
                        Ok(Box::new(p) as Box<dyn StreamProcessor>)
                    })
                }

                _ => Err(StreamError::Configuration(
                    format!("Unknown pre-built processor type: {}", processor_type)
                ))
            }
        }
    })
}

/// Placeholder for when python-embed feature is not enabled
#[cfg(not(feature = "python-embed"))]
pub fn create_processor_from_code(_code: &str) -> Result<Box<dyn StreamProcessor>> {
    Err(StreamError::Configuration(
        "Python processors require the 'python-embed' feature to be enabled. \
         Rebuild with --features python-embed to use dynamic Python processors.".to_string()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "python-embed")]
    fn test_create_custom_python_processor() {
        let code = r#"
from streamlib import processor, StreamInput, StreamOutput, VideoFrame

@processor(description="Test processor")
class TestFilter:
    class InputPorts:
        video = StreamInput(VideoFrame)
    class OutputPorts:
        video = StreamOutput(VideoFrame)
    def process(self, tick):
        pass
"#;

        let result = create_processor_from_code(code);
        assert!(result.is_ok(), "Failed to create processor: {:?}", result.err());
    }

    #[test]
    #[cfg(feature = "python-embed")]
    fn test_invalid_python_code() {
        let code = "this is not valid python!!!";
        let result = create_processor_from_code(code);
        assert!(result.is_err());
    }

    #[test]
    #[cfg(feature = "python-embed")]
    fn test_no_decorator() {
        let code = "x = 42";  // Valid Python, but no processor
        let result = create_processor_from_code(code);
        assert!(result.is_err());
    }

    #[test]
    #[cfg(feature = "python-embed")]
    fn test_processor_with_multiple_ports() {
        let code = r#"
from streamlib import processor, StreamInput, StreamOutput, VideoFrame, AudioFrame

@processor(description="Multi-port processor")
class MultiPortProcessor:
    class InputPorts:
        video = StreamInput(VideoFrame)
        audio = StreamInput(AudioFrame)
    class OutputPorts:
        video = StreamOutput(VideoFrame)
        audio = StreamOutput(AudioFrame)
    def process(self, tick):
        pass
"#;

        let result = create_processor_from_code(code);
        assert!(result.is_ok(), "Failed to create multi-port processor: {:?}", result.err());
    }

    #[test]
    #[cfg(feature = "python-embed")]
    fn test_processor_generator_no_inputs() {
        let code = r#"
from streamlib import processor, StreamOutput, VideoFrame

@processor(description="Generator processor")
class GeneratorProcessor:
    class OutputPorts:
        video = StreamOutput(VideoFrame)
    def process(self, tick):
        pass
"#;

        let result = create_processor_from_code(code);
        assert!(result.is_ok(), "Failed to create generator processor: {:?}", result.err());
    }

    #[test]
    #[cfg(feature = "python-embed")]
    fn test_processor_sink_no_outputs() {
        let code = r#"
from streamlib import processor, StreamInput, VideoFrame

@processor(description="Sink processor")
class SinkProcessor:
    class InputPorts:
        video = StreamInput(VideoFrame)
    def process(self, tick):
        pass
"#;

        let result = create_processor_from_code(code);
        assert!(result.is_ok(), "Failed to create sink processor: {:?}", result.err());
    }

    #[test]
    #[cfg(all(feature = "python-embed", any(target_os = "macos", target_os = "ios")))]
    fn test_pre_built_camera_processor() {
        let code = r#"
from streamlib import processor

@processor
class CameraProcessor:
    pass
"#;

        let result = create_processor_from_code(code);
        // This might fail if no camera is available, but should not panic
        // and should return a proper error if it fails
        match result {
            Ok(_) => {}, // Success
            Err(e) => {
                // Should be a configuration error, not a panic
                assert!(matches!(e, StreamError::Configuration(_) | StreamError::Runtime(_)));
            }
        }
    }

    #[test]
    #[cfg(all(feature = "python-embed", any(target_os = "macos", target_os = "ios")))]
    fn test_pre_built_display_processor() {
        let code = r#"
from streamlib import processor

@processor
class DisplayProcessor:
    pass
"#;

        let result = create_processor_from_code(code);
        // Should succeed on macOS/iOS
        match result {
            Ok(_) => {}, // Success
            Err(e) => {
                // Should be a configuration error, not a panic
                assert!(matches!(e, StreamError::Configuration(_) | StreamError::Runtime(_)));
            }
        }
    }

    #[test]
    #[cfg(feature = "python-embed")]
    fn test_missing_process_method() {
        let code = r#"
@processor(description="Invalid processor")
class BadProcessor:
    class InputPorts:
        video = StreamInput(VideoFrame)
    # Missing process method!
"#;

        let result = create_processor_from_code(code);
        // Should fail but not panic
        assert!(result.is_err());
    }

    #[test]
    #[cfg(not(feature = "python-embed"))]
    fn test_without_python_embed_feature() {
        let code = "any code";
        let result = create_processor_from_code(code);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), StreamError::Configuration(_)));
    }
}
