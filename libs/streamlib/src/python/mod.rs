
mod runtime;
mod types;
mod types_ext;
mod decorators;
mod error;
mod port;
mod processor;
mod gpu_wrappers;
mod events;

use pyo3::prelude::*;

pub use error::{PyStreamError, Result};
pub use runtime::{PyStreamRuntime, PyProcessorHandle};
pub use port::ProcessorPort;
pub use types::{
    PyVideoFrame,
    PyAudioFrame1, PyAudioFrame2, PyAudioFrame4, PyAudioFrame6, PyAudioFrame8,
    PyDataFrame
};
pub use decorators::{processor as processor_decorator, ProcessorProxy};
pub use processor::PythonProcessor;

pub fn register_python_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Frame types
    m.add_class::<types::PyVideoFrame>()?;
    m.add_class::<types::PyAudioFrame1>()?;
    m.add_class::<types::PyAudioFrame2>()?;
    m.add_class::<types::PyAudioFrame4>()?;
    m.add_class::<types::PyAudioFrame6>()?;
    m.add_class::<types::PyAudioFrame8>()?;
    m.add_class::<types::PyDataFrame>()?;

    // Runtime and processors
    m.add_class::<runtime::PyStreamRuntime>()?;
    m.add_class::<runtime::PyProcessorHandle>()?;
    m.add_class::<port::ProcessorPort>()?;
    m.add_class::<decorators::ProcessorProxy>()?;

    m.add_class::<types_ext::PyStreamInput>()?;
    m.add_class::<types_ext::PyStreamOutput>()?;
    m.add_class::<types_ext::PyStreamInputAudio1>()?;
    m.add_class::<types_ext::PyStreamOutputAudio1>()?;
    m.add_class::<types_ext::PyStreamInputAudio2>()?;
    m.add_class::<types_ext::PyStreamOutputAudio2>()?;
    m.add_class::<types_ext::PyStreamInputAudio4>()?;
    m.add_class::<types_ext::PyStreamOutputAudio4>()?;
    m.add_class::<types_ext::PyStreamInputAudio6>()?;
    m.add_class::<types_ext::PyStreamOutputAudio6>()?;
    m.add_class::<types_ext::PyStreamInputAudio8>()?;
    m.add_class::<types_ext::PyStreamOutputAudio8>()?;
    m.add_class::<types_ext::PyStreamInputData>()?;
    m.add_class::<types_ext::PyStreamOutputData>()?;
    m.add_class::<types_ext::PyGpuContext>()?;

    m.add_class::<gpu_wrappers::PyWgpuDevice>()?;
    m.add_class::<gpu_wrappers::PyWgpuQueue>()?;
    m.add_class::<gpu_wrappers::PyWgpuShaderModule>()?;
    m.add_class::<gpu_wrappers::PyWgpuBuffer>()?;
    m.add_class::<gpu_wrappers::PyWgpuBindGroupLayout>()?;
    m.add_class::<gpu_wrappers::PyWgpuPipelineLayout>()?;
    m.add_class::<gpu_wrappers::PyWgpuComputePipeline>()?;
    m.add_class::<gpu_wrappers::PyWgpuBindGroup>()?;
    m.add_class::<gpu_wrappers::PyWgpuCommandEncoder>()?;
    m.add_class::<gpu_wrappers::PyWgpuComputePass>()?;
    m.add_class::<gpu_wrappers::PyWgpuTexture>()?;
    m.add_class::<gpu_wrappers::PyWgpuTextureView>()?;

    m.add_class::<gpu_wrappers::PyBufferUsage>()?;
    m.add_class::<gpu_wrappers::PyShaderStage>()?;
    m.add_class::<gpu_wrappers::PyTextureSampleType>()?;
    m.add_class::<gpu_wrappers::PyTextureViewDimension>()?;
    m.add_class::<gpu_wrappers::PyStorageTextureAccess>()?;
    m.add_class::<gpu_wrappers::PyTextureFormat>()?;
    m.add_class::<gpu_wrappers::PyBufferBindingType>()?;

    // Event bus
    events::register_events(m)?;

    // Only keep the @processor decorator for custom Python processors
    m.add_function(wrap_pyfunction!(decorators::processor, m)?)?;

    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("__doc__", "Real-time streaming infrastructure for AI agents")?;

    // Built-in processor type constants
    m.add("CAMERA_PROCESSOR", "CameraProcessor")?;
    m.add("DISPLAY_PROCESSOR", "DisplayProcessor")?;

    let py = m.py();
    let type_fn = py.eval_bound("type", None, None)?;

    let stream_input_dict = pyo3::types::PyDict::new_bound(py);
    stream_input_dict.set_item("__init__", py.eval_bound(
        "lambda self, type_hint=None: None",
        None,
        None
    )?)?;
    stream_input_dict.set_item("__repr__", py.eval_bound(
        "lambda self: 'StreamInput(VideoFrame)'",
        None,
        None
    )?)?;

    let stream_input = type_fn.call1((
        "StreamInput",
        py.eval_bound("(object,)", None, None)?,
        stream_input_dict,
    ))?;

    m.add("StreamInput", stream_input)?;

    let stream_output_dict = pyo3::types::PyDict::new_bound(py);
    stream_output_dict.set_item("__init__", py.eval_bound(
        "lambda self, type_hint=None: None",
        None,
        None
    )?)?;
    stream_output_dict.set_item("__repr__", py.eval_bound(
        "lambda self: 'StreamOutput(VideoFrame)'",
        None,
        None
    )?)?;

    let stream_output = type_fn.call1((
        "StreamOutput",
        py.eval_bound("(object,)", None, None)?,
        stream_output_dict,
    ))?;

    m.add("StreamOutput", stream_output)?;

    let video_frame_dict = pyo3::types::PyDict::new_bound(py);
    video_frame_dict.set_item("__repr__", py.eval_bound(
        "lambda self: 'VideoFrame'",
        None,
        None
    )?)?;

    let video_frame = type_fn.call1((
        "VideoFrame",
        py.eval_bound("(object,)", None, None)?,
        video_frame_dict,
    ))?;

    m.add("VideoFrame", video_frame)?;

    let audio_frame_dict = pyo3::types::PyDict::new_bound(py);
    audio_frame_dict.set_item("__repr__", py.eval_bound(
        "lambda self: 'AudioFrame'",
        None,
        None
    )?)?;

    let audio_frame = type_fn.call1((
        "AudioFrame",
        py.eval_bound("(object,)", None, None)?,
        audio_frame_dict,
    ))?;

    m.add("AudioFrame", audio_frame)?;

    Ok(())
}
