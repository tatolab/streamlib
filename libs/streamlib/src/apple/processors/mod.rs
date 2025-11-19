// Sources
pub mod camera;
pub mod audio_capture;
pub mod webrtc_whep;  // WebRTC WHEP receive (egress)

// Sinks
pub mod display;
pub mod audio_output;
pub mod mp4_writer;
pub mod webrtc_whip;  // WebRTC WHIP streaming (ingress, webrtc-rs based)

// Source exports
pub use camera::AppleCameraProcessor;
pub use audio_capture::AppleAudioCaptureProcessor;
pub use webrtc_whep::{WebRtcWhepProcessor, WebRtcWhepConfig};

// Sink exports
pub use display::AppleDisplayProcessor;
pub use audio_output::AppleAudioOutputProcessor;
pub use mp4_writer::{AppleMp4WriterProcessor, AppleMp4WriterConfig};
pub use webrtc_whip::{
    WebRtcWhipProcessor, WebRtcWhipConfig,
};
// str0m version (experimental, not working):
// pub use webrtc_whip::{WebRtcWhipProcessor, WebRtcWhipConfig, WhipConfig, VideoEncoderConfig, AudioEncoderConfig, H264Profile};

