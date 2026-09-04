// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A one-connection-at-a-time HTTP responder for tests, so a signalling round
//! trip can be driven with no network and no relay.

#![cfg(test)]

use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// What the responder recorded of one request it answered.
pub(crate) struct HttpRequestUnderTest {
    pub request_line: String,
    pub headers: String,
    pub body: String,
}

impl HttpRequestUnderTest {
    pub(crate) fn has_header(&self, lowercased: &str) -> bool {
        self.headers.to_ascii_lowercase().contains(lowercased)
    }
}

/// Serves answers a handler computes, and records what drew each one.
pub(crate) struct HttpResponderUnderTest {
    pub origin: String,
    requests: Arc<Mutex<Vec<HttpRequestUnderTest>>>,
    listening: tokio::task::JoinHandle<()>,
}

impl HttpResponderUnderTest {
    /// The handler is called once per request and returns the whole raw
    /// response, so a test can answer with anything a relay could.
    pub(crate) async fn answering_with<Handler, Answer>(handler: Handler) -> Self
    where
        Handler: Fn(&HttpRequestUnderTest) -> Answer + Send + Sync + 'static,
        Answer: Future<Output = String> + Send,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);

        let listening = tokio::spawn(async move {
            while let Ok((mut connection, _)) = listener.accept().await {
                let Some(request) = read_one_request(&mut connection).await else {
                    continue;
                };
                let response = handler(&request).await;
                recorded.lock().unwrap().push(request);
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

    /// Serves `responses` in order; a request past the end gets a 500.
    pub(crate) async fn answering(responses: Vec<String>) -> Self {
        let remaining = Arc::new(Mutex::new(responses.into_iter()));
        Self::answering_with(move |_request| {
            let next = remaining.lock().unwrap().next();
            async move {
                next.unwrap_or_else(|| {
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\
                     Connection: close\r\n\r\n"
                        .to_owned()
                })
            }
        })
        .await
    }

    pub(crate) fn recorded(&self) -> MutexGuard<'_, Vec<HttpRequestUnderTest>> {
        self.requests.lock().unwrap()
    }
}

impl Drop for HttpResponderUnderTest {
    fn drop(&mut self) {
        self.listening.abort();
    }
}

/// Read one HTTP/1.1 request: headers to the blank line, then exactly the body
/// `Content-Length` declared.
async fn read_one_request(connection: &mut TcpStream) -> Option<HttpRequestUnderTest> {
    let mut received = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let read = connection.read(&mut buffer).await.unwrap_or(0);
        if read == 0 {
            return None;
        }
        received.extend_from_slice(&buffer[..read]);
        let text = String::from_utf8_lossy(&received).to_string();
        let Some(headers_end) = text.find("\r\n\r\n") else {
            continue;
        };
        let declared_body_length = text
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim())
            })
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        if received.len() < headers_end + 4 + declared_body_length {
            continue;
        }
        let (head, body) = text.split_at(headers_end + 4);
        let mut lines = head.lines();
        return Some(HttpRequestUnderTest {
            request_line: lines.next().unwrap_or_default().to_owned(),
            headers: lines.collect::<Vec<_>>().join("\n"),
            body: body.to_owned(),
        });
    }
}
