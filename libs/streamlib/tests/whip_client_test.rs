// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration test for WHIP client against Cloudflare Stream
//!
//! This test verifies that our WHIP signaling implementation works with a real
//! Cloudflare Stream endpoint by:
//! 1. Creating a WebRTC session with real SDP offer
//! 2. POSTing the offer to Cloudflare
//! 3. Receiving and validating the SDP answer
//! 4. Testing ICE candidate PATCH
//! 5. Testing session DELETE
//!
//! Run with: cargo test --test whip_client_test -- --nocapture --ignored
//!
//! Note: This test is marked #[ignore] by default because it requires:
//! - Network connectivity to Cloudflare
//! - The Cloudflare test endpoint to be active

#[cfg(target_os = "macos")]
#[cfg(test)]
mod whip_client_tests {
    use http_body_util::BodyExt;
    use streamlib::core::error::Result;

    /// Type alias for boxed body used by hyper client
    type BoxBody = http_body_util::combinators::BoxBody<
        bytes::Bytes,
        Box<dyn std::error::Error + Send + Sync>,
    >;

    /// Test configuration for Cloudflare Stream endpoint
    const CLOUDFLARE_WHIP_URL: &str = "https://customer-5xiy6nkciicmt85v.cloudflarestream.com/4e48912c1e10e84c9bab3777695145dbk0072e99f6ddb152545830a794d165fce/webRTC/publish";

    /// Generate a minimal SDP offer for testing WHIP signaling
    ///
    /// This creates a valid WebRTC SDP offer without actually initializing
    /// the full encoder stack. We only need valid SDP to test the HTTP signaling.
    fn create_test_sdp_offer() -> String {
        // Minimal SDP offer with H.264 Baseline Profile Level 3.1 (required by Cloudflare)
        // This is what our WebRtcSession would generate
        format!(
            "v=0\r\n\
             o=- {} 2 IN IP4 127.0.0.1\r\n\
             s=-\r\n\
             t=0 0\r\n\
             a=group:BUNDLE 0 1\r\n\
             a=msid-semantic: WMS streamlib\r\n\
             m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
             c=IN IP4 0.0.0.0\r\n\
             a=rtcp:9 IN IP4 0.0.0.0\r\n\
             a=ice-ufrag:test\r\n\
             a=ice-pwd:testpasswordtestpasswordtest\r\n\
             a=fingerprint:sha-256 00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00\r\n\
             a=setup:actpass\r\n\
             a=mid:0\r\n\
             a=sendonly\r\n\
             a=rtcp-mux\r\n\
             a=rtpmap:96 H264/90000\r\n\
             a=fmtp:96 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n\
             a=ssrc:1000 cname:streamlib-video\r\n\
             a=ssrc:1000 msid:streamlib streamlib-video\r\n\
             m=audio 9 UDP/TLS/RTP/SAVPF 97\r\n\
             c=IN IP4 0.0.0.0\r\n\
             a=rtcp:9 IN IP4 0.0.0.0\r\n\
             a=ice-ufrag:test\r\n\
             a=ice-pwd:testpasswordtestpasswordtest\r\n\
             a=fingerprint:sha-256 00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00\r\n\
             a=setup:actpass\r\n\
             a=mid:1\r\n\
             a=sendonly\r\n\
             a=rtcp-mux\r\n\
             a=rtpmap:97 opus/48000/2\r\n\
             a=ssrc:2000 cname:streamlib-audio\r\n\
             a=ssrc:2000 msid:streamlib streamlib-audio\r\n",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        )
    }

    /// Test POST offer to Cloudflare WHIP endpoint
    ///
    /// This verifies:
    /// - HTTP connection to Cloudflare succeeds
    /// - SDP offer is accepted (201 Created)
    /// - SDP answer is received and valid
    /// - Location header is provided for session URL
    #[test]
    #[ignore] // Run with: cargo test -- --ignored
    fn test_whip_post_offer() -> Result<()> {
        use http_body_util::{BodyExt, Full};
        use hyper::{header, Request, StatusCode};
        use hyper_rustls::HttpsConnectorBuilder;
        use hyper_util::client::legacy::Client;

        println!("=== Testing WHIP POST to Cloudflare ===\n");

        // Install default crypto provider for rustls (ring)
        let _ = rustls::crypto::ring::default_provider().install_default();

        // Create HTTPS client
        let https = HttpsConnectorBuilder::new()
            .with_native_roots()? // Use system root certificates
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();

        let client: Client<_, BoxBody> = Client::builder(hyper_util::rt::TokioExecutor::new())
            .pool_idle_timeout(std::time::Duration::from_secs(30))
            .build(https);

        // Generate test SDP offer
        let sdp_offer = create_test_sdp_offer();
        println!("Generated SDP offer ({} bytes)", sdp_offer.len());
        println!("Codec: H.264 Baseline Profile Level 3.1 (42e01f)\n");

        // Build POST request
        let body = Full::new(bytes::Bytes::from(sdp_offer.clone()));
        let boxed_body = body.map_err(|never| match never {}).boxed();

        let req = Request::builder()
            .method("POST")
            .uri(CLOUDFLARE_WHIP_URL)
            .header(header::CONTENT_TYPE, "application/sdp")
            .body(boxed_body)
            .expect("Failed to build request");

        println!("Sending POST to: {}", CLOUDFLARE_WHIP_URL);

        // Send request (use tokio runtime)
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let response = runtime
            .block_on(async { client.request(req).await })
            .expect("HTTP request failed");

        let status = response.status();
        println!("Response status: {}", status);

        // Extract and clone headers before consuming response
        let location = response
            .headers()
            .get(header::LOCATION)
            .map(|v| v.to_str().unwrap_or("").to_string());
        let etag = response
            .headers()
            .get(header::ETAG)
            .map(|v| v.to_str().unwrap_or("").to_string());

        if let Some(ref loc) = location {
            println!("Location: {}", loc);
        }
        if let Some(ref tag) = etag {
            println!("ETag: {}", tag);
        }

        // Read response body
        let body_bytes = runtime
            .block_on(async { response.into_body().collect().await })
            .expect("Failed to read body");

        let sdp_answer = String::from_utf8_lossy(&body_bytes.to_bytes()).to_string();

        println!("\n=== Response ===");
        println!("Status: {}", status);
        println!("SDP Answer ({} bytes):\n{}", sdp_answer.len(), sdp_answer);

        // Validate response
        assert_eq!(status, StatusCode::CREATED, "Expected 201 Created");
        assert!(location.is_some(), "Expected Location header");
        assert!(!sdp_answer.is_empty(), "Expected SDP answer in body");
        assert!(
            sdp_answer.contains("v=0"),
            "SDP answer should start with version"
        );
        assert!(
            sdp_answer.contains("m=video") || sdp_answer.contains("m=audio"),
            "SDP answer should contain media sections"
        );

        println!("\n✅ WHIP POST test passed!");
        println!("   - Cloudflare accepted our SDP offer");
        println!("   - Received valid SDP answer");
        println!("   - Session URL available for PATCH/DELETE");

        Ok(())
    }

    /// Test the full WHIP flow: POST -> PATCH -> DELETE
    ///
    /// This is a more comprehensive test that verifies:
    /// 1. POST creates session
    /// 2. PATCH sends ICE candidates
    /// 3. DELETE terminates session
    #[test]
    #[ignore] // Run with: cargo test -- --ignored
    fn test_whip_full_flow() -> Result<()> {
        use http_body_util::{BodyExt, Empty, Full};
        use hyper::{header, Request, StatusCode};
        use hyper_rustls::HttpsConnectorBuilder;
        use hyper_util::client::legacy::Client;

        println!("=== Testing Full WHIP Flow ===\n");

        // Install default crypto provider for rustls (ring)
        let _ = rustls::crypto::ring::default_provider().install_default();

        // Create HTTPS client
        let https = HttpsConnectorBuilder::new()
            .with_native_roots()? // Use system root certificates
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();

        let client: Client<_, BoxBody> = Client::builder(hyper_util::rt::TokioExecutor::new())
            .pool_idle_timeout(std::time::Duration::from_secs(30))
            .build(https);

        let runtime = tokio::runtime::Runtime::new().unwrap();

        // Step 1: POST offer
        println!("Step 1: POST SDP offer");
        let sdp_offer = create_test_sdp_offer();
        let body = Full::new(bytes::Bytes::from(sdp_offer));
        let boxed_body = body.map_err(|never| match never {}).boxed();

        let req = Request::builder()
            .method("POST")
            .uri(CLOUDFLARE_WHIP_URL)
            .header(header::CONTENT_TYPE, "application/sdp")
            .body(boxed_body)
            .expect("Failed to build POST request");

        let response = runtime
            .block_on(async { client.request(req).await })
            .expect("POST failed");

        assert_eq!(response.status(), StatusCode::CREATED);

        let session_url = response
            .headers()
            .get(header::LOCATION)
            .expect("No Location header")
            .to_str()
            .expect("Invalid Location header")
            .to_string();

        println!("   ✓ Session created: {}\n", session_url);

        // Consume the POST response body
        runtime
            .block_on(async { response.into_body().collect().await })
            .expect("Failed to read POST body");

        // Step 2: PATCH ICE candidates
        println!("Step 2: PATCH ICE candidates");

        // Simulate ICE candidates in SDP fragment format
        let ice_candidates = vec![
            "a=candidate:1 1 UDP 2130706431 192.168.1.100 54321 typ host",
            "a=candidate:2 1 UDP 1694498815 203.0.113.1 54322 typ srflx raddr 192.168.1.100 rport 54321",
        ];
        let sdp_fragment = ice_candidates.join("\r\n");

        let body = Full::new(bytes::Bytes::from(sdp_fragment));
        let boxed_body = body.map_err(|never| match never {}).boxed();

        let req = Request::builder()
            .method("PATCH")
            .uri(&session_url)
            .header(header::CONTENT_TYPE, "application/trickle-ice-sdpfrag")
            .body(boxed_body)
            .expect("Failed to build PATCH request");

        let response = runtime
            .block_on(async { client.request(req).await })
            .expect("PATCH failed");

        let patch_status = response.status();
        println!("   Response: {}", patch_status);

        // Consume PATCH response body
        runtime
            .block_on(async { response.into_body().collect().await })
            .expect("Failed to read PATCH body");

        // PATCH should return 204 No Content or 200 OK
        assert!(
            patch_status == StatusCode::NO_CONTENT || patch_status == StatusCode::OK,
            "PATCH should return 204 or 200, got: {}",
            patch_status
        );
        println!("   ✓ ICE candidates sent\n");

        // Step 3: DELETE session
        println!("Step 3: DELETE session");

        let body = Empty::<bytes::Bytes>::new();
        let boxed_body = body.map_err(|never| match never {}).boxed();

        let req = Request::builder()
            .method("DELETE")
            .uri(&session_url)
            .body(boxed_body)
            .expect("Failed to build DELETE request");

        let response = runtime
            .block_on(async { client.request(req).await })
            .expect("DELETE failed");

        let delete_status = response.status();
        println!("   Response: {}", delete_status);

        // Consume DELETE response body
        runtime
            .block_on(async { response.into_body().collect().await })
            .expect("Failed to read DELETE body");

        // DELETE typically returns 200 or 204
        assert!(
            delete_status == StatusCode::OK || delete_status == StatusCode::NO_CONTENT,
            "DELETE failed"
        );
        println!("   ✓ Session terminated\n");

        println!("✅ Full WHIP flow test passed!");
        println!("   POST -> PATCH -> DELETE all succeeded");

        Ok(())
    }

    /// Validate that our SDP offer contains correct H.264 profile
    #[test]
    fn test_sdp_offer_contains_correct_h264_profile() {
        let sdp = create_test_sdp_offer();

        // Must contain H.264 codec
        assert!(sdp.contains("H264/90000"), "SDP must specify H.264 codec");

        // Must contain Cloudflare-required profile-level-id
        assert!(
            sdp.contains("profile-level-id=42e01f"),
            "SDP must specify Constrained Baseline Profile Level 3.1 (42e01f)"
        );

        // Must contain packetization-mode=1 (non-interleaved)
        assert!(
            sdp.contains("packetization-mode=1"),
            "SDP must specify packetization-mode=1"
        );

        // Must contain Opus audio
        assert!(sdp.contains("opus/48000/2"), "SDP must specify Opus codec");

        println!("✅ SDP offer validation passed");
        println!("   - H.264 Baseline Profile Level 3.1 (42e01f)");
        println!("   - Packetization mode 1");
        println!("   - Opus 48kHz stereo");
    }
}
