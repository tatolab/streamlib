// Sources
pub mod audio_capture;
pub mod camera;
pub mod webrtc_whep; // WebRTC WHEP receive (egress)

// Sinks
pub mod audio_output;
pub mod display;
pub mod mp4_writer;
pub mod webrtc_whip; // WebRTC WHIP streaming (ingress, webrtc-rs based)

// Source exports
pub use audio_capture::AppleAudioCaptureProcessor;
pub use camera::AppleCameraProcessor;
pub use webrtc_whep::{WebRtcWhepConfig, WebRtcWhepProcessor};

// Sink exports
pub use audio_output::AppleAudioOutputProcessor;
pub use display::AppleDisplayProcessor;
pub use mp4_writer::AppleMp4WriterProcessor;
pub use webrtc_whip::{WebRtcWhipConfig, WebRtcWhipProcessor};
// str0m version (experimental, not working):
// pub use webrtc_whip::{WebRtcWhipProcessor, WebRtcWhipConfig, WhipConfig, VideoEncoderConfig, AudioEncoderConfig, H264Profile};
