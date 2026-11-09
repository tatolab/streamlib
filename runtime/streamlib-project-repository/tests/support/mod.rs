// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A project remote for tests: stock `git http-backend` for the history and an in-memory Git LFS
//! endpoint at `<repo>/info/lfs` for the asset bytes, both behind a minimal HTTP/1.1 server that
//! refuses a request without the bearer token, standing in for an Artifacts repo and its store.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use streamlib_project_repository::ProjectHistoryRemote;

pub const REMOTE_BEARER_TOKEN: &str = "art_v1_0123456789abcdef0123456789abcdef01234567";

/// What the remote's LFS endpoint holds and what it was asked, in order.
#[derive(Default)]
pub struct LfsEndpointHeld {
    /// Each object's bytes, by SHA-256.
    pub objects: BTreeMap<String, Vec<u8>>,
    /// Each request the remote answered, as `<method> <what>`: `upload <oid>`, `download <oid>`,
    /// `verify <oid>`, `batch <operation>`, `git <service>`.
    pub requests: Vec<String>,
    /// Serve these bytes in place of every object downloaded.
    pub serve_other_bytes: Option<Vec<u8>>,
}

/// The remote, serving one bare repository `arm.git` and its LFS endpoint.
pub struct SmartHttpRemoteServedByGitHttpBackend {
    remote_root: tempfile::TempDir,
    pub port: u16,
    pub lfs_endpoint_held: Arc<Mutex<LfsEndpointHeld>>,
}

impl SmartHttpRemoteServedByGitHttpBackend {
    pub fn serving_an_empty_bare_repository() -> Self {
        let remote_root = tempfile::tempdir().unwrap();
        let status = Command::new("git")
            .args(["init", "--bare", "--initial-branch=main", "-q", "arm.git"])
            .current_dir(remote_root.path())
            .status()
            .unwrap();
        assert!(status.success());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let project_root = remote_root.path().to_path_buf();
        let lfs_endpoint_held = Arc::new(Mutex::new(LfsEndpointHeld::default()));
        let served_lfs_endpoint_held = Arc::clone(&lfs_endpoint_held);
        std::thread::spawn(move || {
            for connection in listener.incoming().flatten() {
                let project_root = project_root.clone();
                let lfs_endpoint_held = Arc::clone(&served_lfs_endpoint_held);
                std::thread::spawn(move || {
                    answer_one_request(connection, &project_root, port, &lfs_endpoint_held)
                });
            }
        });
        Self {
            remote_root,
            port,
            lfs_endpoint_held,
        }
    }

    pub fn remote(&self, bearer_token: Option<&str>) -> ProjectHistoryRemote {
        ProjectHistoryRemote {
            url: format!("http://127.0.0.1:{}/arm.git", self.port),
            bearer_token: bearer_token.map(str::to_owned),
            content_remote: None,
        }
    }

    pub fn bare_repository(&self) -> PathBuf {
        self.remote_root.path().join("arm.git")
    }

    pub fn requests(&self) -> Vec<String> {
        self.lfs_endpoint_held.lock().unwrap().requests.clone()
    }
}

struct ParsedRequest {
    method: String,
    path_info: String,
    query_string: String,
    content_type: String,
    authorization: String,
    body: Vec<u8>,
}

fn read_one_request(connection: &TcpStream) -> ParsedRequest {
    let mut request = BufReader::new(connection.try_clone().unwrap());
    let mut request_line = String::new();
    request.read_line(&mut request_line).unwrap();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default().to_owned();
    let (path_info, query_string) = target.split_once('?').unwrap_or((&target, ""));
    let mut content_type = String::new();
    let mut content_length = 0;
    let mut authorization = String::new();
    loop {
        let mut header_line = String::new();
        request.read_line(&mut header_line).unwrap();
        if header_line.trim().is_empty() {
            break;
        }
        let (name, value) = header_line.split_once(':').unwrap();
        match name.to_ascii_lowercase().as_str() {
            "content-type" => content_type = value.trim().to_owned(),
            "content-length" => content_length = value.trim().parse().unwrap(),
            "authorization" => authorization = value.trim().to_owned(),
            _ => {}
        }
    }
    let mut body = vec![0; content_length];
    request.read_exact(&mut body).unwrap();
    ParsedRequest {
        method,
        path_info: path_info.to_owned(),
        query_string: query_string.to_owned(),
        content_type,
        authorization,
        body,
    }
}

fn answer_one_request(
    connection: TcpStream,
    project_root: &Path,
    port: u16,
    lfs_endpoint_held: &Mutex<LfsEndpointHeld>,
) {
    let request = read_one_request(&connection);
    let mut response = connection;
    if request.authorization != format!("Bearer {REMOTE_BEARER_TOKEN}") {
        response
            .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 13\r\nConnection: close\r\n\r\nno such token")
            .unwrap();
        return;
    }
    if let Some(lfs_path) = request.path_info.strip_prefix("/arm.git/info/lfs/") {
        let (status, body) = answer_an_lfs_request(&request, lfs_path, port, lfs_endpoint_held);
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/vnd.git-lfs+json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        response.write_all(head.as_bytes()).unwrap();
        response.write_all(&body).unwrap();
        return;
    }
    let service = request
        .path_info
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_owned();
    if request.method == "POST" {
        lfs_endpoint_held
            .lock()
            .unwrap()
            .requests
            .push(format!("git {service}"));
    }
    answer_with_git_http_backend(&request, project_root, response);
}

fn answer_an_lfs_request(
    request: &ParsedRequest,
    lfs_path: &str,
    port: u16,
    lfs_endpoint_held: &Mutex<LfsEndpointHeld>,
) -> (&'static str, Vec<u8>) {
    let mut held = lfs_endpoint_held.lock().unwrap();
    let object_href = |oid: &str| format!("http://127.0.0.1:{port}/arm.git/info/lfs/objects/{oid}");
    match (request.method.as_str(), lfs_path) {
        ("POST", "objects/batch") => {
            let batch: Value = serde_json::from_slice(&request.body).unwrap();
            let operation = batch["operation"].as_str().unwrap().to_owned();
            held.requests.push(format!("batch {operation}"));
            let objects: Vec<Value> = batch["objects"]
                .as_array()
                .unwrap()
                .iter()
                .map(|object| {
                    let oid = object["oid"].as_str().unwrap();
                    let size = object["size"].as_u64().unwrap();
                    let is_held = held.objects.contains_key(oid);
                    match (operation.as_str(), is_held) {
                        ("upload", true) => json!({"oid": oid, "size": size}),
                        ("upload", false) => json!({"oid": oid, "size": size, "actions": {
                            "upload": {"href": object_href(oid)},
                            "verify": {"href": format!("{}/verify", object_href(oid))},
                        }}),
                        (_, true) => json!({"oid": oid, "size": size, "actions": {
                            "download": {"href": object_href(oid)},
                        }}),
                        (_, false) => json!({"oid": oid, "size": size, "error": {
                            "code": 404, "message": "Object does not exist",
                        }}),
                    }
                })
                .collect();
            (
                "200 OK",
                serde_json::to_vec(&json!({"transfer": "basic", "objects": objects})).unwrap(),
            )
        }
        ("PUT", object_path) => {
            let oid = object_path.trim_start_matches("objects/").to_owned();
            held.requests.push(format!("upload {oid}"));
            held.objects.insert(oid, request.body.clone());
            ("200 OK", Vec::new())
        }
        ("POST", object_path) if object_path.ends_with("/verify") => {
            let oid = object_path
                .trim_start_matches("objects/")
                .trim_end_matches("/verify")
                .to_owned();
            held.requests.push(format!("verify {oid}"));
            if held.objects.contains_key(&oid) {
                ("200 OK", Vec::new())
            } else {
                ("404 Not Found", b"not held".to_vec())
            }
        }
        ("GET", object_path) => {
            let oid = object_path.trim_start_matches("objects/").to_owned();
            held.requests.push(format!("download {oid}"));
            if let Some(other_bytes) = held.serve_other_bytes.clone() {
                return ("200 OK", other_bytes);
            }
            match held.objects.get(&oid) {
                Some(bytes) => ("200 OK", bytes.clone()),
                None => ("404 Not Found", b"not held".to_vec()),
            }
        }
        _ => ("404 Not Found", Vec::new()),
    }
}

fn answer_with_git_http_backend(
    request: &ParsedRequest,
    project_root: &Path,
    mut response: TcpStream,
) {
    let mut backend = Command::new("git")
        .arg("http-backend")
        .env("GIT_PROJECT_ROOT", project_root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REMOTE_USER", "tatolab")
        .env("REQUEST_METHOD", &request.method)
        .env("PATH_INFO", &request.path_info)
        .env("QUERY_STRING", &request.query_string)
        .env("CONTENT_TYPE", &request.content_type)
        .env("CONTENT_LENGTH", request.body.len().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    backend
        .stdin
        .take()
        .unwrap()
        .write_all(&request.body)
        .unwrap();
    let cgi_output = backend.wait_with_output().unwrap().stdout;
    let header_end = cgi_output
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| (at, 4))
        .or_else(|| {
            cgi_output
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|at| (at, 2))
        })
        .unwrap();
    let cgi_headers = String::from_utf8_lossy(&cgi_output[..header_end.0]).to_string();
    let cgi_body = &cgi_output[header_end.0 + header_end.1..];
    let mut status = "200 OK".to_owned();
    let mut headers = String::new();
    for line in cgi_headers.lines() {
        if let Some(cgi_status) = line.strip_prefix("Status:") {
            status = cgi_status.trim().to_owned();
        } else if !line.trim().is_empty() {
            headers.push_str(line.trim());
            headers.push_str("\r\n");
        }
    }
    let head = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
        cgi_body.len()
    );
    response.write_all(head.as_bytes()).unwrap();
    response.write_all(cgi_body).unwrap();
}
