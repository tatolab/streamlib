// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Who a local relay child admits, answered by `tatolabd` itself over the
//! unix socket the child's `auth.url` names, in the moq-auth request shape:
//! a `POST /` carrying a [`moq_auth::Request`], answered by a
//! [`moq_auth::Grant`] or, for a session's `end`, no content.
//!
//! The session presenting the runtime's own token may publish and subscribe;
//! every other session may subscribe alone, so only the runtime publishes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use moq_auth::{Event, Grant, Pattern, Patterns, Request, Token};
use streamlib::sdk::unix_socket_path_cleared_for_bind::{
    UnixSocketPathRefusedForBind, clear_unix_socket_path_for_bind,
};
use streamlib_relay_adapter_sdk::RELAY_DIAL_TOKEN_QUERY_PARAMETER;
use streamlib_relay_adapter_sdk::secrecy::{ExposeSecret, SecretString};
use streamlib_relay_adapter_sdk::url::form_urlencoded;

/// What one admission request is answered with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalRelayAdmissionAnswer {
    /// The session is admitted with this grant.
    Admitted(Grant),
    /// The session ended; nothing is granted.
    SessionEnded,
}

/// The one rule every local relay child admits by.
pub struct LocalRelayAdmission {
    runtime_publish_token: SecretString,
}

impl LocalRelayAdmission {
    /// Admission that lets the session presenting `runtime_publish_token` publish.
    pub fn granting_publish_to_the_token(runtime_publish_token: SecretString) -> Self {
        Self {
            runtime_publish_token,
        }
    }

    /// The answer to `request`.
    pub fn answer(&self, request: &Request) -> LocalRelayAdmissionAnswer {
        match request.event {
            Event::End { .. } => LocalRelayAdmissionAnswer::SessionEnded,
            Event::Connect | Event::Revalidate => {
                LocalRelayAdmissionAnswer::Admitted(self.grant_for(request))
            }
        }
    }

    fn grant_for(&self, request: &Request) -> Grant {
        let everything = Patterns::from_iter([Pattern::all()]);
        if self.the_request_presents_the_runtimes_token(request) {
            Grant::new(everything.clone(), everything)
        } else {
            Grant::new(Patterns::new(), everything)
        }
    }

    fn the_request_presents_the_runtimes_token(&self, request: &Request) -> bool {
        let runtime_publish_token = self.runtime_publish_token.expose_secret().as_bytes();
        tokens_the_request_presents(request).any(|presented| {
            aws_lc_rs::constant_time::verify_slices_are_equal(&presented, runtime_publish_token)
                .is_ok()
        })
    }
}

/// Every token `request` carries: its query's token parameter and a SETUP
/// token of type 0.
fn tokens_the_request_presents(request: &Request) -> impl Iterator<Item = Vec<u8>> + '_ {
    let from_the_query = request.query.iter().flat_map(|query| {
        form_urlencoded::parse(query.as_bytes())
            .filter(|(name, _)| name == RELAY_DIAL_TOKEN_QUERY_PARAMETER)
            .map(|(_, value)| value.into_owned().into_bytes())
    });
    let from_the_setup = request
        .token
        .iter()
        .filter(|token| token.kind == Token::OUT_OF_BAND)
        .map(|token| token.value.clone());
    from_the_query.chain(from_the_setup)
}

/// The admission socket of one child, answered until dropped.
pub struct LocalRelayAdmissionResponder {
    socket_path: PathBuf,
    stop_serving: Option<tokio::sync::oneshot::Sender<()>>,
}

/// Why an admission socket could not be served.
#[derive(Debug, thiserror::Error)]
pub enum LocalRelayAdmissionResponderRefusal {
    #[error("the local relay's admission socket cannot be bound: {0}")]
    PathNotClear(#[from] UnixSocketPathRefusedForBind),
    #[error("the local relay's admission socket {} could not be bound: {source}", path.display())]
    NotBound {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl LocalRelayAdmissionResponder {
    /// Serve `admission` at `socket_path` on `async_runtime`, replacing a
    /// socket file a previous run left there and refusing one a live process
    /// answers on.
    pub fn serve(
        admission: Arc<LocalRelayAdmission>,
        socket_path: &Path,
        async_runtime: &tokio::runtime::Handle,
    ) -> Result<Self, LocalRelayAdmissionResponderRefusal> {
        let not_bound = |source| LocalRelayAdmissionResponderRefusal::NotBound {
            path: socket_path.to_path_buf(),
            source,
        };
        clear_unix_socket_path_for_bind(socket_path)?;
        let standard_listener =
            std::os::unix::net::UnixListener::bind(socket_path).map_err(not_bound)?;
        standard_listener.set_nonblocking(true).map_err(not_bound)?;
        let listener = {
            let _entered = async_runtime.enter();
            tokio::net::UnixListener::from_std(standard_listener).map_err(not_bound)?
        };

        let router = axum::Router::new()
            .route("/", axum::routing::post(answer_one_admission_request))
            .with_state(admission);
        let (stop_serving, stop_requested) = tokio::sync::oneshot::channel::<()>();
        let served_socket_path = socket_path.to_path_buf();
        async_runtime.spawn(async move {
            let served = axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = stop_requested.await;
                })
                .await;
            if let Err(failure) = served {
                tracing::error!(
                    "the local relay's admission socket {} stopped answering: {failure}",
                    served_socket_path.display()
                );
            }
        });
        Ok(Self {
            socket_path: socket_path.to_path_buf(),
            stop_serving: Some(stop_serving),
        })
    }
}

impl Drop for LocalRelayAdmissionResponder {
    fn drop(&mut self) {
        if let Some(stop_serving) = self.stop_serving.take() {
            let _ = stop_serving.send(());
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

async fn answer_one_admission_request(
    State(admission): State<Arc<LocalRelayAdmission>>,
    Json(request): Json<Request>,
) -> Response {
    match admission.answer(&request) {
        LocalRelayAdmissionAnswer::Admitted(grant) => Json(grant).into_response(),
        LocalRelayAdmissionAnswer::SessionEnded => StatusCode::NO_CONTENT.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moq_auth::Transport;

    const THE_RUNTIMES_TOKEN: &str = "c0ffee-runtime-token";

    fn admission() -> LocalRelayAdmission {
        LocalRelayAdmission::granting_publish_to_the_token(SecretString::from(
            THE_RUNTIMES_TOKEN.to_string(),
        ))
    }

    fn a_session_at(path: &str) -> Request {
        Request::new("local-loopback", Transport::Quic, path)
    }

    fn granted(answer: LocalRelayAdmissionAnswer) -> Grant {
        match answer {
            LocalRelayAdmissionAnswer::Admitted(grant) => grant,
            LocalRelayAdmissionAnswer::SessionEnded => panic!("the session is admitted"),
        }
    }

    fn may_publish(grant: &Grant) -> bool {
        grant.publish.matches("desk/main.hang")
    }

    fn may_subscribe(grant: &Grant) -> bool {
        grant.subscribe.matches("desk/main.hang")
    }

    #[test]
    fn the_runtimes_token_in_the_query_may_publish_and_subscribe_everything() {
        let mut request = a_session_at("/");
        request.query = Some(format!("jwt={THE_RUNTIMES_TOKEN}"));
        let grant = granted(admission().answer(&request));
        assert!(may_publish(&grant) && may_subscribe(&grant), "{grant:?}");
        assert_eq!(grant.root, None, "the grant applies under the dialed path");
        assert!(grant.validate().is_ok());
    }

    #[test]
    fn the_runtimes_token_as_a_setup_token_may_publish() {
        let mut request = a_session_at("/");
        request.token = Some(Token {
            kind: Token::OUT_OF_BAND,
            value: THE_RUNTIMES_TOKEN.as_bytes().to_vec(),
        });
        assert!(may_publish(&granted(admission().answer(&request))));
    }

    #[test]
    fn an_anonymous_session_may_subscribe_and_never_publish() {
        for transport in [Transport::Quic, Transport::WebSocket, Transport::Http] {
            let request = Request::new("local-loopback", transport, "/");
            let grant = granted(admission().answer(&request));
            assert!(may_subscribe(&grant), "{transport}");
            assert!(!may_publish(&grant), "{transport}");
            assert!(grant.validate().is_ok());
        }
    }

    #[test]
    fn a_wrong_token_or_a_setup_token_of_another_type_may_subscribe_alone() {
        let mut wrong_in_the_query = a_session_at("/");
        wrong_in_the_query.query = Some(format!("jwt={THE_RUNTIMES_TOKEN}x"));
        let mut prefix_in_the_query = a_session_at("/");
        prefix_in_the_query.query = Some("jwt=c0ffee".to_string());
        let mut another_type = a_session_at("/");
        another_type.token = Some(Token {
            kind: Token::CAT,
            value: THE_RUNTIMES_TOKEN.as_bytes().to_vec(),
        });
        let mut another_parameter = a_session_at("/");
        another_parameter.query = Some(format!("token={THE_RUNTIMES_TOKEN}"));
        for request in [
            wrong_in_the_query,
            prefix_in_the_query,
            another_type,
            another_parameter,
        ] {
            let grant = granted(admission().answer(&request));
            assert!(!may_publish(&grant), "{request:?}");
            assert!(may_subscribe(&grant), "{request:?}");
        }
    }

    #[test]
    fn a_session_ending_is_granted_nothing() {
        let mut request = a_session_at("/");
        request.event = Event::End {
            reason: moq_auth::lease::Reason::Session("closed".to_string()),
            duration: std::time::Duration::from_secs(1),
            bytes: moq_auth::Bytes::default(),
        };
        assert_eq!(
            admission().answer(&request),
            LocalRelayAdmissionAnswer::SessionEnded
        );
    }

    #[test]
    fn a_socket_a_live_process_answers_on_is_refused_rather_than_displaced() {
        let async_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let socket_path = directory.path().join("admission.sock");
        let _live = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
        let refusal = LocalRelayAdmissionResponder::serve(
            Arc::new(admission()),
            &socket_path,
            async_runtime.handle(),
        )
        .err()
        .unwrap();
        assert!(
            matches!(
                refusal,
                LocalRelayAdmissionResponderRefusal::PathNotClear(_)
            ),
            "{refusal}"
        );
    }

    #[test]
    fn the_socket_answers_a_post_with_the_grant_in_json() {
        let async_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let socket_path = directory.path().join("admission.sock");
        std::fs::write(&socket_path, b"left by a previous run").unwrap();
        let responder = LocalRelayAdmissionResponder::serve(
            Arc::new(admission()),
            &socket_path,
            async_runtime.handle(),
        )
        .unwrap();

        let body = serde_json::to_string(&a_session_at("/")).unwrap();
        let answer = post_over_the_unix_socket(&socket_path, &body);
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        let grant: Grant = serde_json::from_str(answer.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert!(may_subscribe(&grant) && !may_publish(&grant), "{grant:?}");

        drop(responder);
        assert!(
            !socket_path.exists(),
            "the socket file goes with the responder"
        );
    }

    fn post_over_the_unix_socket(socket_path: &Path, body: &str) -> String {
        use std::io::{Read, Write};
        let mut stream = std::os::unix::net::UnixStream::connect(socket_path).unwrap();
        write!(
            stream,
            "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        answer
    }
}
