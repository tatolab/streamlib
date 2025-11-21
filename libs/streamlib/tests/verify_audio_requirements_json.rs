//! Verify AudioRequirements serialize to JSON correctly for MCP
//!
//! Run with: cargo test --test verify_audio_requirements_json

#[cfg(test)]
mod tests {
    use streamlib::core::{AudioRequirements, ProcessorDescriptor};

    #[test]
    fn test_audio_requirements_serialization() {
        // Create descriptor with audio requirements (like TestToneGenerator)
        let descriptor = ProcessorDescriptor::new("TestProcessor", "Test audio processor")
            .with_audio_requirements(AudioRequirements {
                preferred_buffer_size: Some(2048),
                required_buffer_size: None,
                supported_sample_rates: vec![44100, 48000],
                required_channels: Some(2),
            });

        // Serialize to JSON (this is what MCP does)
        let json = descriptor.to_json().expect("Should serialize");
        println!("Serialized JSON:\n{}", json);

        // Parse back to verify structure
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("Should be valid JSON");

        // Verify audio_requirements field exists
        assert!(
            parsed.get("audio_requirements").is_some(),
            "audio_requirements field should be present"
        );

        let audio_req = &parsed["audio_requirements"];
        assert_eq!(audio_req["preferred_buffer_size"], 2048);
        assert_eq!(audio_req["supported_sample_rates"][0], 44100);
        assert_eq!(audio_req["supported_sample_rates"][1], 48000);
        assert_eq!(audio_req["required_channels"], 2);

        // Verify required_buffer_size is absent (None should skip serialization)
        assert!(
            audio_req.get("required_buffer_size").is_none(),
            "required_buffer_size should not serialize when None"
        );

        println!("✅ AudioRequirements serialize correctly to JSON!");
    }

    #[test]
    fn test_audio_requirements_compatibility() {
        // Test compatibility checking
        let flexible = AudioRequirements::flexible();
        let strict = AudioRequirements::required(2048, 48000, 2);

        // Flexible should be compatible with anything
        assert!(
            flexible.compatible_with(&strict),
            "Flexible should be compatible with strict"
        );

        // But strict may not be compatible with different requirements
        let different = AudioRequirements::required(4096, 44100, 1);
        assert!(
            !strict.compatible_with(&different),
            "Strict requirements should detect incompatibility"
        );

        // Get error message
        let error_msg = strict.compatibility_error(&different);
        println!("Compatibility error: {}", error_msg);
        assert!(
            error_msg.to_lowercase().contains("buffer size") || error_msg.contains("Buffer"),
            "Error should mention buffer size"
        );

        println!("✅ AudioRequirements compatibility checking works!");
    }
}
