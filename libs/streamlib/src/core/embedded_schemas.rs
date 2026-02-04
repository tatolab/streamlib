// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/// Get the embedded JTD YAML definition for a built-in schema.
pub fn get_embedded_schema_definition(name: &str) -> Option<&'static str> {
    match name {
        // Data schemas
        "com.tatolab.videoframe" => Some(include_str!("../../schemas/com.tatolab.videoframe.yaml")),
        "com.tatolab.audioframe" => Some(include_str!("../../schemas/com.tatolab.audioframe.yaml")),
        "com.tatolab.encodedvideoframe" => Some(include_str!(
            "../../schemas/com.tatolab.encodedvideoframe.yaml"
        )),
        // Config schemas
        "com.tatolab.camera.config@1.0.0" => Some(include_str!(
            "../../schemas/com.tatolab.camera.config@1.0.0.yaml"
        )),
        "com.tatolab.display.config@1.0.0" => Some(include_str!(
            "../../schemas/com.tatolab.display.config@1.0.0.yaml"
        )),
        "com.tatolab.audio_capture.config@1.0.0" => Some(include_str!(
            "../../schemas/com.tatolab.audio_capture.config@1.0.0.yaml"
        )),
        "com.tatolab.audio_output.config@1.0.0" => Some(include_str!(
            "../../schemas/com.tatolab.audio_output.config@1.0.0.yaml"
        )),
        "com.tatolab.audio_mixer.config@1.0.0" => Some(include_str!(
            "../../schemas/com.tatolab.audio_mixer.config@1.0.0.yaml"
        )),
        "com.tatolab.audio_resampler.config@1.0.0" => Some(include_str!(
            "../../schemas/com.tatolab.audio_resampler.config@1.0.0.yaml"
        )),
        "com.tatolab.audio_channel_converter.config@1.0.0" => Some(include_str!(
            "../../schemas/com.tatolab.audio_channel_converter.config@1.0.0.yaml"
        )),
        "com.tatolab.buffer_rechunker.config@1.0.0" => Some(include_str!(
            "../../schemas/com.tatolab.buffer_rechunker.config@1.0.0.yaml"
        )),
        "com.tatolab.chord_generator.config@1.0.0" => Some(include_str!(
            "../../schemas/com.tatolab.chord_generator.config@1.0.0.yaml"
        )),
        "com.tatolab.mp4_writer.config@1.0.0" => Some(include_str!(
            "../../schemas/com.tatolab.mp4_writer.config@1.0.0.yaml"
        )),
        "com.tatolab.screen_capture.config@1.0.0" => Some(include_str!(
            "../../schemas/com.tatolab.screen_capture.config@1.0.0.yaml"
        )),
        "com.tatolab.simple_passthrough.config@1.0.0" => Some(include_str!(
            "../../schemas/com.tatolab.simple_passthrough.config@1.0.0.yaml"
        )),
        "com.streamlib.api_server.config@1.0.0" => Some(include_str!(
            "../../schemas/com.streamlib.api_server.config@1.0.0.yaml"
        )),
        "com.streamlib.clap.effect.config@1.0.0" => Some(include_str!(
            "../../schemas/com.streamlib.clap.effect.config@1.0.0.yaml"
        )),
        "com.streamlib.webrtc_whip.config@1.0.0" => Some(include_str!(
            "../../schemas/com.streamlib.webrtc_whip.config@1.0.0.yaml"
        )),
        "com.streamlib.webrtc_whep.config@1.0.0" => Some(include_str!(
            "../../schemas/com.streamlib.webrtc_whep.config@1.0.0.yaml"
        )),
        _ => None,
    }
}

/// List all embedded schema names.
pub fn list_embedded_schema_names() -> Vec<&'static str> {
    vec![
        // Data schemas
        "com.tatolab.audioframe",
        "com.tatolab.encodedvideoframe",
        "com.tatolab.videoframe",
        // Config schemas
        "com.streamlib.api_server.config@1.0.0",
        "com.streamlib.clap.effect.config@1.0.0",
        "com.streamlib.webrtc_whep.config@1.0.0",
        "com.streamlib.webrtc_whip.config@1.0.0",
        "com.tatolab.audio_capture.config@1.0.0",
        "com.tatolab.audio_channel_converter.config@1.0.0",
        "com.tatolab.audio_mixer.config@1.0.0",
        "com.tatolab.audio_output.config@1.0.0",
        "com.tatolab.audio_resampler.config@1.0.0",
        "com.tatolab.buffer_rechunker.config@1.0.0",
        "com.tatolab.camera.config@1.0.0",
        "com.tatolab.chord_generator.config@1.0.0",
        "com.tatolab.display.config@1.0.0",
        "com.tatolab.mp4_writer.config@1.0.0",
        "com.tatolab.screen_capture.config@1.0.0",
        "com.tatolab.simple_passthrough.config@1.0.0",
    ]
}
