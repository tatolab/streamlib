// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The HTTP half of WHIP (RFC 9725) and WHEP, which are the same three
//! requests: POST the offer, PATCH trickled candidates, DELETE the session.

use crate::error::{Result, WebRtcExtensionError};
use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::{Request, StatusCode, header};
use std::time::Duration;

type SignallingRequestBody = BoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;
type SignallingHttpClient = hyper_util::client::legacy::Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    SignallingRequestBody,
>;

/// How long a signalling request may take. Bounded well under the helper's
/// 60 s registration budget, which a connect on the first bag sits outside but
/// a stalled relay would otherwise hold a processor in indefinitely.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The DELETE that ends a session runs during teardown, which the engine bounds
/// at five seconds before it kills the child — so this has to finish first.
const TEARDOWN_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// A relay that answers every redirect with another one would otherwise spin
/// here for as long as it liked.
const HIGHEST_REDIRECTS_FOLLOWED: u8 = 5;

/// The session a successful offer opened.
#[derive(Debug)]
pub struct OpenedSession {
    pub sdp_answer: String,
    /// Absolute, resolved from the `Location` header the relay returned.
    pub session_url: String,
}

/// Speaks WHIP or WHEP signalling to one endpoint.
pub struct WhipWhepSignallingClient {
    http_client: SignallingHttpClient,
    endpoint_url: String,
    bearer_token: Option<String>,
    protocol: &'static str,
}

impl WhipWhepSignallingClient {
    /// `protocol` names this client in every refusal it raises.
    ///
    /// The transport stack must already be up: building the HTTPS connector
    /// reads rustls's default crypto provider, which panics inside rustls if
    /// nothing installed one. `extension.py:load` is what installs it, and it
    /// runs before any processor module is imported.
    pub fn new(
        endpoint_url: String,
        bearer_token: Option<String>,
        protocol: &'static str,
    ) -> Result<Self> {
        let https_connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .map_err(|failure| WebRtcExtensionError::Signalling {
                protocol,
                what: format!("no system CA roots to verify the relay against: {failure}"),
            })?
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();

        Ok(Self {
            http_client: hyper_util::client::legacy::Client::builder(
                hyper_util::rt::TokioExecutor::new(),
            )
            .pool_idle_timeout(Duration::from_secs(30))
            .build(https_connector),
            endpoint_url,
            bearer_token,
            protocol,
        })
    }

    /// POST the offer and read back the answer and the session's own URL.
    pub async fn post_offer(&self, sdp_offer: &str) -> Result<OpenedSession> {
        let mut endpoint_url = self.endpoint_url.clone();

        for _ in 0..=HIGHEST_REDIRECTS_FOLLOWED {
            let request = self
                .request_builder("POST", &endpoint_url)
                .header(header::CONTENT_TYPE, "application/sdp")
                .body(full_body(sdp_offer.to_owned()))
                .map_err(|failure| self.refusal(format!("could not build the offer: {failure}")))?;

            let response = self.send(request, REQUEST_TIMEOUT).await?;
            let status = response.status();
            let headers = response.headers().clone();
            let body = self.collect_body(response).await?;

            if status == StatusCode::TEMPORARY_REDIRECT || status == StatusCode::PERMANENT_REDIRECT
            {
                endpoint_url = location_header(&headers, &endpoint_url).ok_or_else(|| {
                    self.refusal(format!("a {status} redirect carried no Location header"))
                })?;
                continue;
            }

            if status != StatusCode::CREATED {
                return Err(self.refusal(format!(
                    "the relay answered {status}: {}",
                    String::from_utf8_lossy(&body)
                )));
            }

            let session_url = location_header(&headers, &endpoint_url).ok_or_else(|| {
                self.refusal("a 201 Created carried no Location header, so the session \
                              cannot be trickled to or deleted"
                    .to_owned())
            })?;
            let sdp_answer = String::from_utf8(body.to_vec()).map_err(|failure| {
                self.refusal(format!("the answer is not UTF-8: {failure}"))
            })?;
            return Ok(OpenedSession {
                sdp_answer,
                session_url,
            });
        }

        Err(self.refusal(format!(
            "the relay redirected more than {HIGHEST_REDIRECTS_FOLLOWED} times"
        )))
    }

    /// Trickle gathered candidates onto an open session.
    pub async fn patch_ice_candidates(
        &self,
        session_url: &str,
        sdp_fragment: String,
    ) -> Result<()> {
        let request = self
            .request_builder("PATCH", session_url)
            .header(header::CONTENT_TYPE, "application/trickle-ice-sdpfrag")
            .body(full_body(sdp_fragment))
            .map_err(|failure| self.refusal(format!("could not build the PATCH: {failure}")))?;

        let response = self.send(request, REQUEST_TIMEOUT).await?;
        let status = response.status();
        if status == StatusCode::NO_CONTENT || status.is_success() {
            return Ok(());
        }
        let body = self.collect_body(response).await?;
        Err(self.refusal(format!(
            "trickling candidates was answered {status}: {}",
            String::from_utf8_lossy(&body)
        )))
    }

    /// End the session. A relay that refuses is logged rather than raised:
    /// this runs during teardown, where the local side is going away either
    /// way and a raise would only replace one report with a worse one.
    pub async fn delete_session(&self, session_url: &str) {
        let request = match self
            .request_builder("DELETE", session_url)
            .body(empty_body())
        {
            Ok(request) => request,
            Err(failure) => {
                tracing::warn!(protocol = self.protocol, %failure, "could not build the DELETE");
                return;
            }
        };

        match self.send(request, TEARDOWN_REQUEST_TIMEOUT).await {
            Ok(response) if response.status().is_success() => {
                tracing::info!(protocol = self.protocol, session_url, "session deleted");
            }
            Ok(response) => {
                tracing::warn!(
                    protocol = self.protocol,
                    status = %response.status(),
                    "the relay refused the session delete; it may linger server-side"
                );
            }
            Err(failure) => {
                tracing::warn!(protocol = self.protocol, %failure, "the session delete did not complete");
            }
        }
    }

    fn request_builder(&self, method: &str, url: &str) -> hyper::http::request::Builder {
        let builder = Request::builder().method(method).uri(url);
        match &self.bearer_token {
            Some(token) => builder.header(header::AUTHORIZATION, format!("Bearer {token}")),
            None => builder,
        }
    }

    async fn send(
        &self,
        request: Request<SignallingRequestBody>,
        timeout: Duration,
    ) -> Result<hyper::Response<hyper::body::Incoming>> {
        tokio::time::timeout(timeout, self.http_client.request(request))
            .await
            .map_err(|_| self.refusal(format!("the relay did not answer within {timeout:?}")))?
            .map_err(|failure| self.refusal(format!("the request did not complete: {failure}")))
    }

    async fn collect_body(&self, response: hyper::Response<hyper::body::Incoming>) -> Result<Bytes> {
        Ok(BodyExt::collect(response.into_body())
            .await
            .map_err(|failure| self.refusal(format!("could not read the response: {failure}")))?
            .to_bytes())
    }

    fn refusal(&self, what: String) -> WebRtcExtensionError {
        WebRtcExtensionError::Signalling {
            protocol: self.protocol,
            what,
        }
    }
}

/// Resolve a `Location` against the URL it was returned from, since a relay may
/// answer with an absolute URL or a site-root-relative path.
fn location_header(headers: &hyper::HeaderMap, requested_url: &str) -> Option<String> {
    let location = headers.get(header::LOCATION)?.to_str().ok()?;
    if !location.starts_with('/') {
        return Some(location.to_owned());
    }
    let origin: String = requested_url.split('/').take(3).collect::<Vec<_>>().join("/");
    Some(format!("{origin}{location}"))
}

fn full_body(body: String) -> SignallingRequestBody {
    Full::new(Bytes::from(body))
        .map_err(|never| match never {})
        .boxed()
}

fn empty_body() -> SignallingRequestBody {
    Empty::<Bytes>::new().map_err(|never| match never {}).boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_with_location(location: &str) -> hyper::HeaderMap {
        let mut headers = hyper::HeaderMap::new();
        headers.insert(header::LOCATION, location.parse().unwrap());
        headers
    }

    #[test]
    fn an_absolute_location_is_taken_as_it_stands() {
        let resolved = location_header(
            &headers_with_location("https://relay.example/sessions/7"),
            "https://ingest.example/live/abc",
        );

        assert_eq!(
            resolved.as_deref(),
            Some("https://relay.example/sessions/7")
        );
    }

    #[test]
    fn a_root_relative_location_resolves_against_the_endpoints_origin() {
        let resolved = location_header(
            &headers_with_location("/sessions/7"),
            "https://ingest.example/live/abc",
        );

        assert_eq!(
            resolved.as_deref(),
            Some("https://ingest.example/sessions/7")
        );
    }

    #[test]
    fn a_response_without_a_location_resolves_to_nothing() {
        assert_eq!(
            location_header(&hyper::HeaderMap::new(), "https://ingest.example/live"),
            None
        );
    }
}

#[cfg(test)]
mod signalling_round_trips {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// One canned HTTP response, and what the stub recorded of the request
    /// that drew it.
    struct RecordedRequest {
        request_line: String,
        headers: String,
        body: String,
    }

    /// A one-connection-at-a-time HTTP responder, so a signalling round trip
    /// can be checked with no network and no relay.
    struct StubRelay {
        origin: String,
        requests: Arc<Mutex<Vec<RecordedRequest>>>,
        listening: tokio::task::JoinHandle<()>,
    }

    impl StubRelay {
        /// Serves `responses` in order; a connection past the end gets a 500.
        async fn answering(responses: Vec<String>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let recorded = Arc::clone(&requests);

            let listening = tokio::spawn(async move {
                let mut responses = responses.into_iter();
                while let Ok((mut connection, _)) = listener.accept().await {
                    let mut received = Vec::new();
                    let mut buffer = [0u8; 4096];
                    // Read until the headers are complete, then take exactly
                    // the body the request declared.
                    loop {
                        let read = connection.read(&mut buffer).await.unwrap_or(0);
                        if read == 0 {
                            break;
                        }
                        received.extend_from_slice(&buffer[..read]);
                        let text = String::from_utf8_lossy(&received).to_string();
                        if let Some(headers_end) = text.find("\r\n\r\n") {
                            let declared_body_length = text
                                .lines()
                                .find_map(|line| {
                                    line.strip_prefix("content-length: ")
                                        .or_else(|| line.strip_prefix("Content-Length: "))
                                })
                                .and_then(|value| value.trim().parse::<usize>().ok())
                                .unwrap_or(0);
                            if received.len() >= headers_end + 4 + declared_body_length {
                                let (head, body) = text.split_at(headers_end + 4);
                                let mut lines = head.lines();
                                recorded.lock().unwrap().push(RecordedRequest {
                                    request_line: lines.next().unwrap_or_default().to_owned(),
                                    headers: lines.collect::<Vec<_>>().join("\n"),
                                    body: body.to_owned(),
                                });
                                break;
                            }
                        }
                    }

                    let response = responses.next().unwrap_or_else(|| {
                        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\
                         Connection: close\r\n\r\n"
                            .to_owned()
                    });
                    let _ = connection.write_all(response.as_bytes()).await;
                    let _ = connection.flush().await;
                }
            });

            Self {
                origin,
                requests,
                listening,
            }
        }

        fn recorded(&self) -> std::sync::MutexGuard<'_, Vec<RecordedRequest>> {
            self.requests.lock().unwrap()
        }
    }

    impl Drop for StubRelay {
        fn drop(&mut self) {
            self.listening.abort();
        }
    }

    const AN_ANSWER: &str = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\n";

    fn created_at(location: &str) -> String {
        format!(
            "HTTP/1.1 201 Created\r\nLocation: {location}\r\nContent-Type: application/sdp\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{AN_ANSWER}",
            AN_ANSWER.len()
        )
    }

    fn redirected_to(location: &str) -> String {
        format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n"
        )
    }

    fn client_for(endpoint_url: String, bearer_token: Option<String>) -> WhipWhepSignallingClient {
        // What `extension.py:load` does in a real process, and what the HTTPS
        // connector below reads.
        crate::transport_stack::bring_up().unwrap();
        WhipWhepSignallingClient::new(endpoint_url, bearer_token, "WHIP").unwrap()
    }

    #[tokio::test]
    async fn an_offer_is_posted_as_sdp_with_the_bearer_token_and_the_answer_read_back() {
        let relay = StubRelay::answering(vec![created_at("/sessions/7")]).await;
        let client = client_for(format!("{}/live", relay.origin), Some("a-token".to_owned()));

        let opened = client.post_offer("v=0\r\nthe-offer\r\n").await.unwrap();

        assert_eq!(opened.sdp_answer, AN_ANSWER);
        assert_eq!(opened.session_url, format!("{}/sessions/7", relay.origin));

        let recorded = relay.recorded();
        assert!(recorded[0].request_line.starts_with("POST /live "));
        assert!(recorded[0].headers.contains("content-type: application/sdp"));
        assert!(recorded[0].headers.contains("authorization: Bearer a-token"));
        assert_eq!(recorded[0].body, "v=0\r\nthe-offer\r\n");
    }

    #[tokio::test]
    async fn no_token_configured_sends_no_authorization_header() {
        let relay = StubRelay::answering(vec![created_at("/sessions/7")]).await;
        let client = client_for(format!("{}/live", relay.origin), None);

        client.post_offer("v=0\r\n").await.unwrap();

        assert!(!relay.recorded()[0].headers.contains("authorization"));
    }

    #[tokio::test]
    async fn a_redirect_is_followed_and_the_offer_posted_again() {
        // The Location is root-relative, which is the form that has to be
        // resolved against the origin before it can be requested at all.
        let relay = StubRelay::answering(vec![
            redirected_to("/elsewhere"),
            created_at("/sessions/9"),
        ])
        .await;
        let client = client_for(format!("{}/live", relay.origin), None);

        let opened = client.post_offer("v=0\r\nthe-offer\r\n").await.unwrap();

        assert_eq!(opened.session_url, format!("{}/sessions/9", relay.origin));
        let recorded = relay.recorded();
        assert!(recorded[0].request_line.starts_with("POST /live "));
        assert!(recorded[1].request_line.starts_with("POST /elsewhere "));
        assert_eq!(recorded[1].body, "v=0\r\nthe-offer\r\n");
    }

    #[tokio::test]
    async fn a_relay_that_redirects_forever_is_refused_rather_than_followed_forever() {
        let always_redirecting: Vec<String> = (0..HIGHEST_REDIRECTS_FOLLOWED + 3)
            .map(|_| redirected_to("/again"))
            .collect();
        let relay = StubRelay::answering(always_redirecting).await;
        let client = client_for(format!("{}/live", relay.origin), None);

        let refusal = client.post_offer("v=0\r\n").await.unwrap_err();

        assert!(refusal.to_string().contains("redirected more than"));
    }

    #[tokio::test]
    async fn a_created_without_a_location_is_refused_because_the_session_is_unreachable() {
        let relay = StubRelay::answering(vec![
            "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
        ])
        .await;
        let client = client_for(format!("{}/live", relay.origin), None);

        let refusal = client.post_offer("v=0\r\n").await.unwrap_err();

        assert!(refusal.to_string().contains("Location"));
    }

    #[tokio::test]
    async fn a_refusal_from_the_relay_names_its_status_and_carries_its_body() {
        let relay = StubRelay::answering(vec![
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: 12\r\nConnection: close\r\n\r\n\
             bad-token   "
                .to_owned(),
        ])
        .await;
        let client = client_for(format!("{}/live", relay.origin), None);

        let refusal = client.post_offer("v=0\r\n").await.unwrap_err().to_string();

        assert!(refusal.contains("401"), "{refusal}");
        assert!(refusal.contains("bad-token"), "{refusal}");
    }

    #[tokio::test]
    async fn trickled_candidates_are_patched_to_the_session_url_as_an_sdp_fragment() {
        let relay = StubRelay::answering(vec![
            "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_owned(),
        ])
        .await;
        let client = client_for(format!("{}/live", relay.origin), None);
        let session_url = format!("{}/sessions/7", relay.origin);

        client
            .patch_ice_candidates(&session_url, "a=candidate:1 1 udp".to_owned())
            .await
            .unwrap();

        let recorded = relay.recorded();
        assert!(recorded[0].request_line.starts_with("PATCH /sessions/7 "));
        assert!(
            recorded[0]
                .headers
                .contains("content-type: application/trickle-ice-sdpfrag")
        );
        assert_eq!(recorded[0].body, "a=candidate:1 1 udp");
    }

    #[tokio::test]
    async fn deleting_a_session_a_relay_refuses_is_reported_rather_than_raised() {
        let relay = StubRelay::answering(vec![
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
        ])
        .await;
        let client = client_for(format!("{}/live", relay.origin), None);

        // Teardown is going away either way; a raise here would replace one
        // report with a worse one.
        client
            .delete_session(&format!("{}/sessions/7", relay.origin))
            .await;

        assert!(relay.recorded()[0].request_line.starts_with("DELETE /sessions/7 "));
    }
}
