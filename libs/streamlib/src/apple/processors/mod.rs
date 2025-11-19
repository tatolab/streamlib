// Sources
pub mod camera;
pub mod audio_capture;

// Sinks
pub mod display;
pub mod audio_output;
pub mod mp4_writer;
// pub mod webrtc_whip;  // WebRTC WHIP streaming (str0m-based - was an experiment, not working)
pub mod webrtc;  // WebRTC WHIP streaming (webrtc-rs based - COMMITTED VERSION, audio worked)

// Source exports
pub use camera::AppleCameraProcessor;
pub use audio_capture::AppleAudioCaptureProcessor;

// Sink exports
pub use display::AppleDisplayProcessor;
pub use audio_output::AppleAudioOutputProcessor;
pub use mp4_writer::{AppleMp4WriterProcessor, AppleMp4WriterConfig};
pub use webrtc::{
    WebRtcWhipProcessor, WebRtcWhipConfig,
    WhipConfig, AudioEncoderConfig,
};
// str0m version (experimental, not working):
// pub use webrtc_whip::{WebRtcWhipProcessor, WebRtcWhipConfig, WhipConfig, VideoEncoderConfig, AudioEncoderConfig, H264Profile};

