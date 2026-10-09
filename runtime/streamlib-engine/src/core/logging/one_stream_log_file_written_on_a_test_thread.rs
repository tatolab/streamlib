// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A logging pathway installed on a test's thread, routing that thread's
//! records into one stream's JSONL file.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use streamlib_runtime_client_contract::runtime_log_event::RuntimeLogEvent;

use crate::core::logging::init::init_for_tests_intercepting_nothing_yet;
use crate::core::logging::{
    LoadedStreamLogRoute, LoadedStreamLogRouteEnteredOnThisThread, StreamlibLoggingConfig,
    StreamlibLoggingGuard,
};

/// The stream name a test's stream log file is written under.
pub(crate) const STREAM_NAME_OF_A_TEST_STREAM_LOG: &str = "main";

/// A test pathway, and the stream route its thread carries until it
/// finishes.
pub(crate) struct OneStreamLogFileWrittenOnThisTestThread {
    entered: Option<LoadedStreamLogRouteEnteredOnThisThread>,
    route: Arc<LoadedStreamLogRoute>,
    guard: Option<StreamlibLoggingGuard>,
}

impl OneStreamLogFileWrittenOnThisTestThread {
    /// Install `config`'s pathway on this thread and route the thread's
    /// records into the stream log of `runtime_id` under `project_directory`;
    /// with `intercept_stdio`, the interception's readers carry that route.
    pub(crate) fn install(
        config: StreamlibLoggingConfig,
        runtime_id: &str,
        project_directory: &Path,
    ) -> Self {
        let mut guard =
            init_for_tests_intercepting_nothing_yet(&config).expect("the test pathway installs");
        let route = LoadedStreamLogRoute::open_in_project_directory(
            runtime_id,
            STREAM_NAME_OF_A_TEST_STREAM_LOG,
            project_directory,
        );
        let entered = route.enter_on_this_thread();
        #[cfg(unix)]
        if config.intercept_stdio {
            guard
                .intercept_the_standard_streams_carrying_this_threads_route()
                .expect("the standard streams are intercepted");
        }
        Self {
            entered: Some(entered),
            route,
            guard: Some(guard),
        }
    }

    /// The stream's active JSONL segment.
    pub(crate) fn jsonl_log_path(&self) -> PathBuf {
        self.route
            .jsonl_log_path()
            .expect("the test pathway opens the stream's log file")
            .to_path_buf()
    }

    /// The route the test thread carries.
    pub(crate) fn route(&self) -> &Arc<LoadedStreamLogRoute> {
        &self.route
    }

    /// The pathway's guard.
    pub(crate) fn guard(&self) -> &StreamlibLoggingGuard {
        self.guard.as_ref().expect("held until the test finishes")
    }

    /// Leave the route, then shut the pathway down: every queued record is
    /// written and the stream file synced.
    pub(crate) fn finish(mut self) {
        drop(self.entered.take());
        drop(self.guard.take());
    }
}

/// Every record of the JSONL file at `path`; none when it is missing.
pub(crate) fn read_every_record_of_a_jsonl_log(path: &Path) -> Vec<RuntimeLogEvent> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str::<RuntimeLogEvent>(line).expect("valid JSONL line"))
        .collect()
}
