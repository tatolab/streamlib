// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Git's smart HTTP protocol for libgit2, carried over the runtime's own rustls so libgit2 is built
//! without OpenSSL: each libgit2 action is one HTTP/1.1 request, a GET of `info/refs` for a ref
//! advertisement or a POST of the buffered pack negotiation to `git-upload-pack` or
//! `git-receive-pack`.

use std::cell::RefCell;
use std::io::{Cursor, Read, Write};
use std::sync::Once;

use git2::transport::{Service, SmartSubtransport, SmartSubtransportStream, Transport};
use git2::{Error as GitError, Remote};

use crate::project_remote_http_exchange::{
    HttpRequestToARemote, send_one_http_request_to_a_remote,
};

thread_local! {
    /// The bearer token of the push or fetch running on this thread, which libgit2 drives synchronously.
    static BEARER_TOKEN_OF_THE_TRANSFER_ON_THIS_THREAD: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Run `transfer` with `bearer_token` sent on every request it makes on this thread.
pub(crate) fn with_the_bearer_token_on_this_thread<TransferOutcome>(
    bearer_token: Option<&str>,
    transfer: impl FnOnce() -> TransferOutcome,
) -> TransferOutcome {
    register_the_smart_http_transport_once();
    BEARER_TOKEN_OF_THE_TRANSFER_ON_THIS_THREAD
        .with(|token| *token.borrow_mut() = bearer_token.map(str::to_owned));
    let outcome = transfer();
    BEARER_TOKEN_OF_THE_TRANSFER_ON_THIS_THREAD.with(|token| *token.borrow_mut() = None);
    outcome
}

fn register_the_smart_http_transport_once() {
    static REGISTERED: Once = Once::new();
    REGISTERED.call_once(|| {
        for scheme in ["https", "http"] {
            // SAFETY: libgit2's transport registry is process-global and not thread-safe to
            // mutate; `Once` makes this the only registration, before any transfer runs.
            let registered = unsafe {
                git2::transport::register(scheme, |remote: &Remote<'_>| {
                    Transport::smart(remote, true, ProjectHistorySmartHttpSubtransport)
                })
            };
            if let Err(not_registered) = registered {
                tracing::error!(scheme, %not_registered, "the smart HTTP transport was not registered");
            }
        }
    });
}

struct ProjectHistorySmartHttpSubtransport;

impl SmartSubtransport for ProjectHistorySmartHttpSubtransport {
    fn action(
        &self,
        url: &str,
        action: Service,
    ) -> Result<Box<dyn SmartSubtransportStream>, GitError> {
        let base_url = url.trim_end_matches('/');
        let request = match action {
            Service::UploadPackLs => {
                SmartHttpRequestToTheRemote::advertisement(base_url, "git-upload-pack")
            }
            Service::ReceivePackLs => {
                SmartHttpRequestToTheRemote::advertisement(base_url, "git-receive-pack")
            }
            Service::UploadPack => {
                SmartHttpRequestToTheRemote::negotiation(base_url, "git-upload-pack")
            }
            Service::ReceivePack => {
                SmartHttpRequestToTheRemote::negotiation(base_url, "git-receive-pack")
            }
        };
        Ok(Box::new(SmartHttpExchangeWithTheRemote {
            request,
            request_body: Vec::new(),
            response_body: None,
            bearer_token: BEARER_TOKEN_OF_THE_TRANSFER_ON_THIS_THREAD
                .with(|token| token.borrow().clone()),
        }))
    }

    fn close(&self) -> Result<(), GitError> {
        Ok(())
    }
}

struct SmartHttpRequestToTheRemote {
    method: &'static str,
    url: String,
    content_type: Option<String>,
    accept: String,
}

impl SmartHttpRequestToTheRemote {
    fn advertisement(base_url: &str, service: &str) -> Self {
        Self {
            method: "GET",
            url: format!("{base_url}/info/refs?service={service}"),
            content_type: None,
            accept: format!("application/x-{service}-advertisement"),
        }
    }

    fn negotiation(base_url: &str, service: &str) -> Self {
        Self {
            method: "POST",
            url: format!("{base_url}/{service}"),
            content_type: Some(format!("application/x-{service}-request")),
            accept: format!("application/x-{service}-result"),
        }
    }
}

/// One request's buffered body going out, and its response's body once it is read.
struct SmartHttpExchangeWithTheRemote {
    request: SmartHttpRequestToTheRemote,
    request_body: Vec<u8>,
    response_body: Option<Cursor<Vec<u8>>>,
    bearer_token: Option<String>,
}

impl Write for SmartHttpExchangeWithTheRemote {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.request_body.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Read for SmartHttpExchangeWithTheRemote {
    fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
        if self.response_body.is_none() {
            let mut headers = vec![("Accept".to_owned(), self.request.accept.clone())];
            if let Some(content_type) = &self.request.content_type {
                headers.push(("Content-Type".to_owned(), content_type.clone()));
            }
            if let Some(bearer_token) = &self.bearer_token {
                headers.push(("Authorization".to_owned(), format!("Bearer {bearer_token}")));
            }
            let mut body = Vec::new();
            send_one_http_request_to_a_remote(
                &HttpRequestToARemote {
                    method: self.request.method,
                    url: &self.request.url,
                    headers,
                    body_byte_len: self
                        .request
                        .content_type
                        .is_some()
                        .then_some(self.request_body.len() as u64),
                },
                &mut self.request_body.as_slice(),
                &mut body,
            )?;
            self.response_body = Some(Cursor::new(body));
        }
        self.response_body
            .as_mut()
            .map_or(Ok(0), |body| body.read(into))
    }
}
