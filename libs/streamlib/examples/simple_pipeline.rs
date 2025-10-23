//! Simple Pipeline Example
//!
//! Demonstrates the platform-agnostic API - same code works on macOS, Linux, Windows.
//! No platform-specific imports or code visible to the user.

use streamlib::{
    StreamProcessor, StreamRuntime,
    StreamOutput, StreamInput, PortMessage, PortType,
    TimedTick,
};
use anyhow::Result;
use std::sync::{Arc, Mutex};

// Define a simple frame message
#[derive(Clone, Debug)]
struct Frame {
    data: String,
}

impl PortMessage for Frame {
    fn port_type() -> PortType {
        PortType::Video
    }
}

// Simple source processor (generates frames)
struct SourceOutputPorts {
    pub video: StreamOutput<Frame>,
}

struct SourcePorts {
    pub output: SourceOutputPorts,
}

struct Source {
    count: u64,
    pub ports: SourcePorts,
}

impl Source {
    fn new() -> Self {
        Self {
            count: 0,
            ports: SourcePorts {
                output: SourceOutputPorts {
                    video: StreamOutput::new("video"),
                }
            }
        }
    }
}

impl StreamProcessor for Source {
    fn process(&mut self, _tick: TimedTick) -> Result<()> {
        let frame = Frame {
            data: format!("Frame {}", self.count),
        };
        self.ports.output.video.write(frame);
        self.count += 1;
        Ok(())
    }
}

// Simple sink processor (consumes frames)
struct SinkInputPorts {
    pub video: StreamInput<Frame>,
}

struct SinkPorts {
    pub input: SinkInputPorts,
}

struct Sink {
    pub ports: SinkPorts,
    count: Arc<Mutex<u64>>,
}

impl Sink {
    fn new(count: Arc<Mutex<u64>>) -> Self {
        Self {
            ports: SinkPorts {
                input: SinkInputPorts {
                    video: StreamInput::new("video"),
                }
            },
            count,
        }
    }
}

impl StreamProcessor for Sink {
    fn process(&mut self, _tick: TimedTick) -> Result<()> {
        if let Some(frame) = self.ports.input.video.read_latest() {
            println!("Received: {}", frame.data);
            *self.count.lock().unwrap() += 1;
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Platform-Agnostic Pipeline Example ===");
    println!("Platform: {}", streamlib::platform::name());
    println!("GPU Backend: {}\n", streamlib::platform::gpu_backend());

    // Create runtime (works on all platforms)
    let mut runtime = StreamRuntime::new(10.0);

    let count = Arc::new(Mutex::new(0));

    // Create processors (platform-agnostic)
    let mut source = Source::new();
    let mut sink = Sink::new(Arc::clone(&count));

    // Connect processors (platform-agnostic)
    runtime.connect(&mut source.ports.output.video, &mut sink.ports.input.video)?;

    runtime.add_processor(Box::new(source));
    runtime.add_processor(Box::new(sink));

    // Run pipeline
    println!("Starting pipeline...");
    runtime.start().await?;
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    runtime.stop().await?;

    let received = *count.lock().unwrap();
    println!("\n✓ Pipeline complete");
    println!("✓ Processed {} frames", received);
    println!("✓ Same code works on macOS, Linux, Windows!");

    Ok(())
}
