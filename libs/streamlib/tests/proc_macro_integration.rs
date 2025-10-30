//! Integration tests for #[processor] proc macro
//!
//! These tests verify that the proc macro generates correct code
//! and integrates properly with the streamlib API.

use streamlib::{processor, StreamInput, StreamOutput, VideoFrame, AudioFrame, TimedTick, Result};

/// Simple processor with both input and output
#[processor]
struct SimpleProcessor {
    input: StreamInput<VideoFrame>,
    output: StreamOutput<VideoFrame>,
}

impl SimpleProcessor {
    fn process(&mut self, _tick: TimedTick) -> Result<()> {
        if let Some(frame) = self.input.read_latest() {
            self.output.write(frame);
        }
        Ok(())
    }
}

/// Generator with only output
#[processor]
struct GeneratorProcessor {
    output: StreamOutput<AudioFrame>,
}

impl GeneratorProcessor {
    fn process(&mut self, _tick: TimedTick) -> Result<()> {
        // Would generate audio here
        Ok(())
    }
}

/// Sink with only input
#[processor]
struct SinkProcessor {
    input: StreamInput<VideoFrame>,
}

impl SinkProcessor {
    fn process(&mut self, _tick: TimedTick) -> Result<()> {
        if let Some(_frame) = self.input.read_latest() {
            // Would display or save frame here
        }
        Ok(())
    }
}

/// Multi-input processor
#[processor]
struct MultiInputProcessor {
    video_in: StreamInput<VideoFrame>,
    audio_in: StreamInput<AudioFrame>,
    output: StreamOutput<VideoFrame>,
}

impl MultiInputProcessor {
    fn process(&mut self, _tick: TimedTick) -> Result<()> {
        // Would combine video and audio here
        Ok(())
    }
}

/// Processor with non-port fields mixed in
#[processor]
struct ProcessorWithState {
    // Port fields
    input: StreamInput<VideoFrame>,
    output: StreamOutput<VideoFrame>,

    // Non-port fields (should be ignored)
    counter: u64,
    name: String,
    config: Option<Vec<u8>>,
}

impl ProcessorWithState {
    fn process(&mut self, _tick: TimedTick) -> Result<()> {
        self.counter += 1;
        Ok(())
    }
}

#[test]
fn test_simple_processor_port_names() {
    // Test that proc macro correctly discovers ports
    let input_ports = SimpleProcessor::input_port_names();
    let output_ports = SimpleProcessor::output_port_names();

    assert_eq!(input_ports, &["input"]);
    assert_eq!(output_ports, &["output"]);
}

#[test]
fn test_generator_processor_port_names() {
    let input_ports = GeneratorProcessor::input_port_names();
    let output_ports = GeneratorProcessor::output_port_names();

    assert_eq!(input_ports.len(), 0);
    assert_eq!(output_ports, &["output"]);
}

#[test]
fn test_sink_processor_port_names() {
    let input_ports = SinkProcessor::input_port_names();
    let output_ports = SinkProcessor::output_port_names();

    assert_eq!(input_ports, &["input"]);
    assert_eq!(output_ports.len(), 0);
}

#[test]
fn test_multi_input_processor_port_names() {
    let input_ports = MultiInputProcessor::input_port_names();
    let output_ports = MultiInputProcessor::output_port_names();

    assert_eq!(input_ports, &["video_in", "audio_in"]);
    assert_eq!(output_ports, &["output"]);
}

#[test]
fn test_processor_with_state_port_names() {
    // Non-port fields should be ignored
    let input_ports = ProcessorWithState::input_port_names();
    let output_ports = ProcessorWithState::output_port_names();

    assert_eq!(input_ports, &["input"]);
    assert_eq!(output_ports, &["output"]);

    // Should NOT include: counter, name, config
}

#[test]
fn test_port_names_are_const() {
    // Verify that the generated functions are const
    // This should compile without issues
    const _INPUT: &[&str] = SimpleProcessor::input_port_names();
    const _OUTPUT: &[&str] = SimpleProcessor::output_port_names();
}

#[test]
fn test_multiple_processors_dont_conflict() {
    // Verify that multiple processors with different port configurations
    // don't interfere with each other

    let simple_in = SimpleProcessor::input_port_names();
    let gen_in = GeneratorProcessor::input_port_names();
    let sink_in = SinkProcessor::input_port_names();

    assert_eq!(simple_in, &["input"]);
    assert_eq!(gen_in.len(), 0);
    assert_eq!(sink_in, &["input"]);
}
