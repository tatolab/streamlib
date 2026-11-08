// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A project's history pushed to a Git smart-HTTP remote and fetched back unchanged, over the
//! crate's own transport. The remote is stock `git http-backend` behind a minimal HTTP server
//! that requires the bearer token, standing in for an Artifacts repo.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use streamlib_project_repository::{
    ProjectAssetChangeRequest, ProjectAssetImportSource, ProjectHistoryRemote,
    ProjectHistoryRepository, ProjectRepository, ProjectRepositoryRefusal,
    ProjectTransactionAuthor, ProjectTransactionAuthorKind,
};

const REMOTE_BEARER_TOKEN: &str = "art_v1_0123456789abcdef0123456789abcdef01234567";

/// `git http-backend` behind one HTTP/1.1 request per connection, refusing a request without
/// the bearer token.
struct SmartHttpRemoteServedByGitHttpBackend {
    remote_root: tempfile::TempDir,
    port: u16,
}

impl SmartHttpRemoteServedByGitHttpBackend {
    fn serving_an_empty_bare_repository() -> Self {
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
        std::thread::spawn(move || {
            for connection in listener.incoming().flatten() {
                let project_root = project_root.clone();
                std::thread::spawn(move || answer_one_request(connection, &project_root));
            }
        });
        Self { remote_root, port }
    }

    fn remote(&self, bearer_token: Option<&str>) -> ProjectHistoryRemote {
        ProjectHistoryRemote {
            url: format!("http://127.0.0.1:{}/arm.git", self.port),
            bearer_token: bearer_token.map(str::to_owned),
        }
    }

    fn bare_repository(&self) -> PathBuf {
        self.remote_root.path().join("arm.git")
    }
}

fn answer_one_request(connection: TcpStream, project_root: &Path) {
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
    let mut request_body = vec![0; content_length];
    request.read_exact(&mut request_body).unwrap();
    let mut response = connection;
    if authorization != format!("Bearer {REMOTE_BEARER_TOKEN}") {
        response
            .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 13\r\nConnection: close\r\n\r\nno such token")
            .unwrap();
        return;
    }
    let mut backend = Command::new("git")
        .arg("http-backend")
        .env("GIT_PROJECT_ROOT", project_root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REMOTE_USER", "tatolab")
        .env("REQUEST_METHOD", &method)
        .env("PATH_INFO", path_info)
        .env("QUERY_STRING", query_string)
        .env("CONTENT_TYPE", &content_type)
        .env("CONTENT_LENGTH", content_length.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    backend
        .stdin
        .take()
        .unwrap()
        .write_all(&request_body)
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

fn a_project_with_one_imported_asset(parent: &Path) -> ProjectRepository {
    let project_directory = parent.join("arm-control");
    let mut project = ProjectRepository::create(
        &project_directory,
        "arm-control",
        &ProjectTransactionAuthor::the_runtime(),
    )
    .unwrap();
    std::fs::write(parent.join("intro.mp4"), b"video bytes").unwrap();
    project
        .apply_asset_changes(
            &ProjectTransactionAuthor {
                kind: ProjectTransactionAuthorKind::Person,
                name: "Jonathan".to_owned(),
            },
            vec![ProjectAssetChangeRequest::ImportAsset {
                source: ProjectAssetImportSource::FileOnThisMachine(parent.join("intro.mp4")),
                asset_path: "media/intro.mp4".to_owned(),
                kind: None,
                import_settings: serde_json::Map::new(),
            }],
        )
        .unwrap();
    project
}

#[test]
fn a_pushed_history_is_fetched_back_unchanged_into_a_fresh_history() {
    let remote_server = SmartHttpRemoteServedByGitHttpBackend::serving_an_empty_bare_repository();
    let parent = tempfile::tempdir().unwrap();
    let project = a_project_with_one_imported_asset(parent.path());

    project
        .history()
        .push_main_to(&remote_server.remote(Some(REMOTE_BEARER_TOKEN)))
        .unwrap();
    let pushed_head = project.history().head_commit().unwrap().unwrap();
    let remote_head = Command::new("git")
        .args(["rev-parse", "refs/heads/main"])
        .current_dir(remote_server.bare_repository())
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&remote_head.stdout).trim(),
        pushed_head
    );

    let fetched_into = tempfile::tempdir().unwrap();
    let fresh_history = ProjectHistoryRepository::create_for_project(fetched_into.path()).unwrap();
    let fetched_head = fresh_history
        .fetch_main_from(&remote_server.remote(Some(REMOTE_BEARER_TOKEN)))
        .unwrap();
    assert_eq!(fetched_head.as_deref(), Some(pushed_head.as_str()));
    assert_eq!(
        fresh_history.commits_newest_first().unwrap(),
        project.history().commits_newest_first().unwrap()
    );
    assert_eq!(
        fresh_history.files_at_head().unwrap(),
        project.history().files_at_head().unwrap()
    );
}

#[test]
fn a_push_without_the_token_is_refused_in_the_remotes_words() {
    let remote_server = SmartHttpRemoteServedByGitHttpBackend::serving_an_empty_bare_repository();
    let parent = tempfile::tempdir().unwrap();
    let project = a_project_with_one_imported_asset(parent.path());
    let refused = project
        .history()
        .push_main_to(&remote_server.remote(Some("art_v1_wrong")))
        .err()
        .unwrap();
    assert!(
        matches!(refused, ProjectRepositoryRefusal::History(_)),
        "{refused:?}"
    );
    assert!(
        refused.to_string().contains("refused the token"),
        "{refused}"
    );
    assert!(!refused.to_string().contains("art_v1_wrong"), "{refused}");
}

#[test]
fn a_push_that_is_not_a_fast_forward_is_refused() {
    let remote_server = SmartHttpRemoteServedByGitHttpBackend::serving_an_empty_bare_repository();
    let first_parent = tempfile::tempdir().unwrap();
    let first = a_project_with_one_imported_asset(first_parent.path());
    first
        .history()
        .push_main_to(&remote_server.remote(Some(REMOTE_BEARER_TOKEN)))
        .unwrap();
    let second_parent = tempfile::tempdir().unwrap();
    let unrelated = a_project_with_one_imported_asset(second_parent.path());
    let refused = unrelated
        .history()
        .push_main_to(&remote_server.remote(Some(REMOTE_BEARER_TOKEN)))
        .err()
        .unwrap();
    assert!(
        matches!(
            refused,
            ProjectRepositoryRefusal::HistoryRemoteRefused { .. }
                | ProjectRepositoryRefusal::History(_)
        ),
        "{refused:?}"
    );
}
