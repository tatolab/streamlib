// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::time::Duration;

use serde_json::json;

use super::{ProjectStore, ProjectStoreTableName};

const TABLE_UNDER_TEST: ProjectStoreTableName = ProjectStoreTableName::named("table_under_test");

#[test]
fn a_write_reads_back_at_once_and_after_the_store_opens_again() {
    let project_directory = tempfile::tempdir().expect("a temporary project");
    {
        let store = ProjectStore::open_in_the_project_at(project_directory.path()).expect("opens");
        store
            .write(
                TABLE_UNDER_TEST,
                "node/value",
                &json!({ "level": 0.75, "label": "warm" }),
            )
            .expect("writes");
        assert_eq!(
            store.read(TABLE_UNDER_TEST, "node/value").expect("reads"),
            Some(json!({ "level": 0.75, "label": "warm" }))
        );
    }
    let reopened = ProjectStore::open_in_the_project_at(project_directory.path()).expect("reopens");
    assert_eq!(
        reopened
            .read(TABLE_UNDER_TEST, "node/value")
            .expect("reads"),
        Some(json!({ "level": 0.75, "label": "warm" }))
    );
    assert_eq!(
        reopened.read(TABLE_UNDER_TEST, "absent").expect("reads"),
        None
    );
}

#[test]
fn a_thousand_writes_in_one_window_commit_once_holding_the_last() {
    let project_directory = tempfile::tempdir().expect("a temporary project");
    let store_file = project_directory.path().join("store.redb");
    let store =
        ProjectStore::open_at_the_file(&store_file, Duration::from_secs(60)).expect("opens");
    for step in 0..1000 {
        store
            .write(TABLE_UNDER_TEST, "slider", &json!(f64::from(step) / 1000.0))
            .expect("writes");
    }
    assert_eq!(store.write_transactions_committed(), 0);
    store.commit_every_waiting_write_now().expect("commits");
    assert_eq!(store.write_transactions_committed(), 1);
    drop(store);
    let reopened =
        ProjectStore::open_at_the_file(&store_file, Duration::from_secs(60)).expect("reopens");
    assert_eq!(
        reopened.read(TABLE_UNDER_TEST, "slider").expect("reads"),
        Some(json!(0.999))
    );
}

#[test]
fn the_writer_commits_a_waiting_write_once_its_window_passes() {
    let project_directory = tempfile::tempdir().expect("a temporary project");
    let store = ProjectStore::open_at_the_file(
        &project_directory.path().join("store.redb"),
        Duration::from_millis(20),
    )
    .expect("opens");
    store
        .write(TABLE_UNDER_TEST, "slider", &json!(0.5))
        .expect("writes");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while store.write_transactions_committed() == 0 && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(store.write_transactions_committed(), 1);
}

#[test]
fn a_removed_key_reads_as_absent_before_and_after_its_commit() {
    let project_directory = tempfile::tempdir().expect("a temporary project");
    let store = ProjectStore::open_in_the_project_at(project_directory.path()).expect("opens");
    store
        .write(TABLE_UNDER_TEST, "slider", &json!(1))
        .expect("writes");
    store.commit_every_waiting_write_now().expect("commits");
    store.remove(TABLE_UNDER_TEST, "slider");
    assert_eq!(store.read(TABLE_UNDER_TEST, "slider").expect("reads"), None);
    store.commit_every_waiting_write_now().expect("commits");
    assert_eq!(store.read(TABLE_UNDER_TEST, "slider").expect("reads"), None);
}

#[test]
fn writes_spread_over_a_second_commit_about_once_per_window_holding_the_last() {
    let project_directory = tempfile::tempdir().expect("a temporary project");
    let store = ProjectStore::open_in_the_project_at(project_directory.path()).expect("opens");
    let started = std::time::Instant::now();
    for step in 0..1000_u32 {
        let write_at = started + Duration::from_millis(u64::from(step));
        while std::time::Instant::now() < write_at {
            std::thread::yield_now();
        }
        store
            .write(TABLE_UNDER_TEST, "slider", &json!(f64::from(step) / 1000.0))
            .expect("writes");
    }
    store.commit_every_waiting_write_now().expect("commits");
    assert!(
        store.write_transactions_committed() <= 6,
        "{} write transactions for a second of writes at a {:?} window",
        store.write_transactions_committed(),
        super::PROJECT_STORE_WRITE_DEBOUNCE_WINDOW
    );
    assert_eq!(
        store.read(TABLE_UNDER_TEST, "slider").expect("reads"),
        Some(json!(0.999))
    );
}
