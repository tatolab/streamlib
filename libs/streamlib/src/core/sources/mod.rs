
pub mod chord_generator;
pub mod camera;
pub mod audio_capture;

pub use chord_generator::{ChordGeneratorProcessor, ChordGeneratorOutputPorts, ChordGeneratorConfig};
pub use camera::{CameraProcessor, CameraDevice, CameraOutputPorts, CameraConfig};
pub use audio_capture::{AudioCaptureProcessor, AudioInputDevice, AudioCaptureOutputPorts, AudioCaptureConfig};
