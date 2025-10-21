use streamlib_core::{
    StreamProcessor, StreamRuntime,
    StreamOutput, StreamInput, PortMessage, PortType,
    TimedTick, Result,
};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
struct Frame {
    data: String,
}

impl PortMessage for Frame {
    fn port_type() -> PortType {
        PortType::Video
    }
}

struct CameraOutputPorts {
    pub video: StreamOutput<Frame>,
}

struct CameraPorts {
    pub output: CameraOutputPorts,
}

struct Camera {
    count: u64,
    pub ports: CameraPorts,
}

impl Camera {
    fn new() -> Self {
        Self {
            count: 0,
            ports: CameraPorts {
                output: CameraOutputPorts {
                    video: StreamOutput::new("video"),
                }
            }
        }
    }
}

impl StreamProcessor for Camera {
    fn process(&mut self, _tick: TimedTick) -> Result<()> {
        let frame = Frame {
            data: format!("Frame {}", self.count),
        };
        self.ports.output.video.write(frame);
        self.count += 1;
        Ok(())
    }
}

struct DisplayInputPorts {
    pub video: StreamInput<Frame>,
}

struct DisplayPorts {
    pub input: DisplayInputPorts,
}

struct Display {
    pub ports: DisplayPorts,
    count: Arc<Mutex<u64>>,
}

impl Display {
    fn new(count: Arc<Mutex<u64>>) -> Self {
        Self {
            ports: DisplayPorts {
                input: DisplayInputPorts {
                    video: StreamInput::new("video"),
                }
            },
            count,
        }
    }
}

impl StreamProcessor for Display {
    fn process(&mut self, _tick: TimedTick) -> Result<()> {
        if let Some(frame) = self.ports.input.video.read_latest() {
            println!("Display: {}", frame.data);
            *self.count.lock().unwrap() += 1;
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut runtime = StreamRuntime::new(10.0);

    let count = Arc::new(Mutex::new(0));

    let mut camera = Camera::new();
    let mut display = Display::new(Arc::clone(&count));

    runtime.connect(&mut camera.ports.output.video, &mut display.ports.input.video)?;

    runtime.add_processor(Box::new(camera));
    runtime.add_processor(Box::new(display));

    runtime.start().await?;
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    runtime.stop().await?;

    let received = *count.lock().unwrap();
    println!("Received {} frames", received);

    Ok(())
}
