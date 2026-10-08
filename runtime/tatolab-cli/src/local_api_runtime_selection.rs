// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Which running runtime a verb drives: the node registry scanned, each entry liveness-checked,
//! the dead ones pruned, and `--node` resolved against what is live.
//!
//! Liveness has two independent signals: whether the entry's local API answers MCP, and whether
//! its host process still exists. An entry is deleted only when both say dead, so a runtime that
//! is briefly slow to answer is never pruned out from under its own process.

use std::path::{Path, PathBuf};

use streamlib_runtime_client_contract::node_registry::{
    NodeRegistryEntry, NodeRegistryError, remove_scanned_entry_file, scan_entries,
};
use streamlib_runtime_client_contract::streamlib_runtime_directory::{
    StreamlibRuntimeDirectory, StreamlibRuntimeDirectoryRefusal,
};

use crate::TatolabCommandFailure;
use crate::local_api_mcp_tool_client::local_api_answers_mcp;

/// A registry entry paired with whether its runtime's local API answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LivenessCheckedNodeRegistryEntry {
    /// The entry as its runtime wrote it.
    pub(crate) node_registry_entry: NodeRegistryEntry,
    /// The local API answered an MCP round trip — what a verb needs, since a runtime whose
    /// process is alive but whose local API is silent cannot be driven.
    pub(crate) local_api_answers: bool,
}

/// Why no runtime could be picked for a verb to drive.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LocalApiRuntimeSelectionFailure {
    /// The runtime directory could not be trusted, in the engine's own words.
    #[error(transparent)]
    RuntimeDirectoryRefused(#[from] StreamlibRuntimeDirectoryRefusal),
    /// The registry directory itself could not be read.
    #[error(transparent)]
    NodeRegistryUnreadable(#[from] NodeRegistryError),
    /// No runtime on this machine answers, `--node` or not.
    #[error("no running runtime found on this machine.\nStart one with `tatolab run`.")]
    NoRunningRuntime,
    /// `--node` named no live runtime by name or by runtime_id.
    #[error(
        "no live runtime named `{requested_runtime_name_or_id}`, and none with that \
         runtime_id.{}",
        live_runtimes_listing(.live_runtimes)
    )]
    RequestedRuntimeMatchesNoLiveRuntime {
        requested_runtime_name_or_id: String,
        live_runtimes: Vec<NodeRegistryEntry>,
    },
    /// `--node` named more than one live runtime, which nothing makes unique.
    #[error(
        "{} live runtimes answer to `{requested_runtime_name_or_id}` — pick one by runtime_id \
         with `--node <runtime_id>`.{}",
        .matching_live_runtimes.len(),
        live_runtimes_listing(.matching_live_runtimes)
    )]
    RequestedRuntimeMatchesSeveralLiveRuntimes {
        requested_runtime_name_or_id: String,
        matching_live_runtimes: Vec<NodeRegistryEntry>,
    },
    /// More than one runtime is live and `--node` picked none.
    #[error(
        "{} live runtimes — pick one with `--node <runtime name or id>`.{}",
        .live_runtimes.len(),
        live_runtimes_listing(.live_runtimes)
    )]
    SeveralLiveRuntimesAndNoneRequested {
        live_runtimes: Vec<NodeRegistryEntry>,
    },
}

impl From<LocalApiRuntimeSelectionFailure> for TatolabCommandFailure {
    fn from(runtime_selection_failure: LocalApiRuntimeSelectionFailure) -> Self {
        TatolabCommandFailure::refused(runtime_selection_failure.to_string())
    }
}

/// A trailing ` Live runtimes: name (id) -> socket, ...` for a refusal that lists what it found.
fn live_runtimes_listing(live_runtimes: &[NodeRegistryEntry]) -> String {
    if live_runtimes.is_empty() {
        return String::new();
    }
    let listed_runtimes: Vec<String> = live_runtimes
        .iter()
        .map(|live_runtime| {
            format!(
                "{} ({}) -> {}",
                live_runtime.runtime_name,
                live_runtime.runtime_id,
                live_runtime.local_api_socket_path.display()
            )
        })
        .collect();
    format!(" Live runtimes: {}", listed_runtimes.join(", "))
}

/// This user's node registry directory, resolved as a reader resolves the runtime directory:
/// creating nothing.
pub(crate) fn this_users_node_registry_directory()
-> Result<PathBuf, LocalApiRuntimeSelectionFailure> {
    Ok(
        StreamlibRuntimeDirectory::resolve_for_a_reader_without_creating()?
            .node_registry_directory(),
    )
}

/// Whether a process with `pid` exists. `kill(pid, 0)` delivers no signal; `EPERM` is a process
/// that exists but is not ours to signal. A pid outside `pid_t` cannot name a process.
pub(crate) fn host_process_exists(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: `kill` with signal 0 sends nothing and reads no memory.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Every entry in `node_registry_directory`, liveness-checked, with the dead ones deleted. An
/// entry whose local API is silent but whose process is alive is kept, not answering.
pub(crate) fn scan_liveness_check_and_prune_node_registry(
    node_registry_directory: &Path,
) -> Result<Vec<LivenessCheckedNodeRegistryEntry>, LocalApiRuntimeSelectionFailure> {
    let scanned_entries = scan_entries(node_registry_directory)?;
    let mut liveness_checked_entries = Vec::with_capacity(scanned_entries.len());
    for scanned_entry in scanned_entries {
        let local_api_answers =
            local_api_answers_mcp(&scanned_entry.node_registry_entry.local_api_socket_path);
        if !local_api_answers && !host_process_exists(scanned_entry.node_registry_entry.pid) {
            // Both signals say gone; an entry that resists removal is still not listed, and the
            // next scan reaches the same verdict.
            let _removed_or_left_for_the_next_scan =
                remove_scanned_entry_file(&scanned_entry.entry_file_path);
            continue;
        }
        liveness_checked_entries.push(LivenessCheckedNodeRegistryEntry {
            node_registry_entry: scanned_entry.node_registry_entry,
            local_api_answers,
        });
    }
    Ok(liveness_checked_entries)
}

/// The runtimes in `node_registry_directory` whose local API answers, with the dead ones pruned.
pub(crate) fn live_runtimes_in_node_registry(
    node_registry_directory: &Path,
) -> Result<Vec<NodeRegistryEntry>, LocalApiRuntimeSelectionFailure> {
    Ok(
        scan_liveness_check_and_prune_node_registry(node_registry_directory)?
            .into_iter()
            .filter(|liveness_checked_entry| liveness_checked_entry.local_api_answers)
            .map(|liveness_checked_entry| liveness_checked_entry.node_registry_entry)
            .collect(),
    )
}

/// The live runtime in `node_registry_directory` a verb drives.
///
/// `requested_runtime_name_or_id` (`--node`) matches a runtime name first and a runtime_id
/// second, since the name is what an app chooses and keeps across runs; without it, the sole live
/// runtime. Zero live runtimes is [`LocalApiRuntimeSelectionFailure::NoRunningRuntime`], `--node`
/// or not.
pub(crate) fn select_live_runtime_in_node_registry(
    node_registry_directory: &Path,
    requested_runtime_name_or_id: Option<&str>,
) -> Result<NodeRegistryEntry, LocalApiRuntimeSelectionFailure> {
    let mut live_runtimes = live_runtimes_in_node_registry(node_registry_directory)?;
    if live_runtimes.is_empty() {
        return Err(LocalApiRuntimeSelectionFailure::NoRunningRuntime);
    }
    if let Some(requested_runtime_name_or_id) =
        requested_runtime_name_or_id.filter(|requested| !requested.is_empty())
    {
        return sole_live_runtime_answering_to(live_runtimes, requested_runtime_name_or_id);
    }
    if live_runtimes.len() == 1 {
        return Ok(live_runtimes.remove(0));
    }
    Err(LocalApiRuntimeSelectionFailure::SeveralLiveRuntimesAndNoneRequested { live_runtimes })
}

/// The one live runtime `--node` names. Nothing makes a name unique, so a tie names every match
/// and refuses rather than picking one.
fn sole_live_runtime_answering_to(
    live_runtimes: Vec<NodeRegistryEntry>,
    requested_runtime_name_or_id: &str,
) -> Result<NodeRegistryEntry, LocalApiRuntimeSelectionFailure> {
    let live_runtimes_whose = |identifying_field: fn(&NodeRegistryEntry) -> &str| {
        live_runtimes
            .iter()
            .filter(|live_runtime| identifying_field(live_runtime) == requested_runtime_name_or_id)
            .cloned()
            .collect::<Vec<NodeRegistryEntry>>()
    };
    let matches_by_name_then_by_runtime_id = [
        live_runtimes_whose(|live_runtime| live_runtime.runtime_name.as_str()),
        live_runtimes_whose(|live_runtime| live_runtime.runtime_id.as_str()),
    ];
    for mut matching_live_runtimes in matches_by_name_then_by_runtime_id {
        match matching_live_runtimes.len() {
            0 => {}
            1 => return Ok(matching_live_runtimes.remove(0)),
            _ => {
                return Err(
                    LocalApiRuntimeSelectionFailure::RequestedRuntimeMatchesSeveralLiveRuntimes {
                        requested_runtime_name_or_id: requested_runtime_name_or_id.to_owned(),
                        matching_live_runtimes,
                    },
                );
            }
        }
    }
    Err(
        LocalApiRuntimeSelectionFailure::RequestedRuntimeMatchesNoLiveRuntime {
            requested_runtime_name_or_id: requested_runtime_name_or_id.to_owned(),
            live_runtimes,
        },
    )
}

/// The live runtime on this machine a verb drives, through this user's node registry.
pub(crate) fn select_live_runtime_on_this_machine(
    requested_runtime_name_or_id: Option<&str>,
) -> Result<NodeRegistryEntry, LocalApiRuntimeSelectionFailure> {
    select_live_runtime_in_node_registry(
        &this_users_node_registry_directory()?,
        requested_runtime_name_or_id,
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use streamlib_runtime_client_contract::node_registry::NODE_REGISTRY_SCHEMA_VERSION;

    use super::*;
    use crate::isolated_node_registry::{
        IsolatedNodeRegistry, NOTHING_LISTENS_LOCAL_API_SOCKET_PATH, PID_NO_PROCESS_HAS,
        PID_OUTSIDE_PID_T, a_registry_entry, a_registry_entry_hosted_by, a_registry_entry_named,
    };
    use crate::stub_local_api_server::StubLocalApiServer;

    fn nothing_listens() -> &'static Path {
        Path::new(NOTHING_LISTENS_LOCAL_API_SOCKET_PATH)
    }

    /// Each scanned entry's runtime_id and whether its local API answered.
    fn scanned_runtime_ids_and_liveness(
        isolated_node_registry: &IsolatedNodeRegistry,
    ) -> Vec<(String, bool)> {
        scan_liveness_check_and_prune_node_registry(
            &isolated_node_registry.node_registry_directory(),
        )
        .unwrap()
        .into_iter()
        .map(|liveness_checked_entry| {
            (
                liveness_checked_entry.node_registry_entry.runtime_id,
                liveness_checked_entry.local_api_answers,
            )
        })
        .collect()
    }

    fn selected_runtime(
        isolated_node_registry: &IsolatedNodeRegistry,
        requested_runtime_name_or_id: Option<&str>,
    ) -> Result<NodeRegistryEntry, LocalApiRuntimeSelectionFailure> {
        select_live_runtime_in_node_registry(
            &isolated_node_registry.node_registry_directory(),
            requested_runtime_name_or_id,
        )
    }

    fn owned(runtime_ids_and_liveness: &[(&str, bool)]) -> Vec<(String, bool)> {
        runtime_ids_and_liveness
            .iter()
            .map(|(runtime_id, local_api_answers)| (runtime_id.to_string(), *local_api_answers))
            .collect()
    }

    #[test]
    fn a_reachable_entry_is_listed_as_alive() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry(
            "Ralive",
            &stub_local_api_server.local_api_socket_path,
        ));

        assert_eq!(
            scanned_runtime_ids_and_liveness(&isolated_node_registry),
            owned(&[("Ralive", true)])
        );
    }

    #[test]
    fn an_entry_that_is_unreachable_and_dead_is_pruned() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let entry_file_path = isolated_node_registry.write_registry_entry(
            &a_registry_entry_hosted_by("Rdead", nothing_listens(), PID_NO_PROCESS_HAS),
        );

        assert_eq!(
            scanned_runtime_ids_and_liveness(&isolated_node_registry),
            []
        );
        assert!(
            !entry_file_path.exists(),
            "both liveness signals said dead — prune the entry"
        );
    }

    /// The runtime's process is this test's, which is unambiguously alive, while its local API
    /// answers nothing. Pruning here would delete a live runtime's entry because it was briefly
    /// slow.
    #[test]
    fn an_unreachable_entry_with_a_live_process_is_kept_but_not_alive() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let entry_file_path = isolated_node_registry
            .write_registry_entry(&a_registry_entry("Rbusy", nothing_listens()));

        assert_eq!(
            scanned_runtime_ids_and_liveness(&isolated_node_registry),
            owned(&[("Rbusy", false)])
        );
        assert!(
            entry_file_path.exists(),
            "one dead signal is not enough to prune"
        );
    }

    #[test]
    fn a_pid_outside_pid_t_does_not_crash_the_scan() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry_hosted_by(
            "Rcorrupt",
            nothing_listens(),
            PID_OUTSIDE_PID_T,
        ));
        isolated_node_registry.write_registry_entry(&a_registry_entry(
            "Rgood",
            &stub_local_api_server.local_api_socket_path,
        ));

        assert_eq!(
            scanned_runtime_ids_and_liveness(&isolated_node_registry),
            owned(&[("Rgood", true)])
        );
    }

    #[test]
    fn a_malformed_entry_does_not_hide_the_others() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry_file("Rgarbage", b"{ not json");
        isolated_node_registry.write_registry_entry(&a_registry_entry(
            "Rgood",
            &stub_local_api_server.local_api_socket_path,
        ));

        assert_eq!(
            scanned_runtime_ids_and_liveness(&isolated_node_registry),
            owned(&[("Rgood", true)])
        );
    }

    /// Coercing would list a `null` name as a string and let `--node` resolve it; the reader
    /// skips what it cannot parse, which is also what keeps it out of the prune path.
    #[test]
    fn an_entry_whose_fields_are_the_wrong_shape_is_neither_listed_nor_deleted() {
        for (wrong_shape, field_name, wrong_value) in [
            ("null-name", "runtime_name", json!(null)),
            ("array-name", "runtime_name", json!(["desk", "rig"])),
            ("object-id", "runtime_id", json!({"nested": "object"})),
            ("fractional-version", "schema_version", json!(2.9)),
            ("boolean-pid", "pid", json!(true)),
            ("null-socket-path", "local_api_socket_path", json!(null)),
        ] {
            let isolated_node_registry = IsolatedNodeRegistry::new();
            let mut entry_json = serde_json::to_value(a_registry_entry_hosted_by(
                "Rmalformed",
                nothing_listens(),
                PID_NO_PROCESS_HAS,
            ))
            .unwrap();
            entry_json[field_name] = wrong_value;
            let entry_file_path = isolated_node_registry
                .write_registry_entry_file("Rmalformed", &serde_json::to_vec(&entry_json).unwrap());

            assert_eq!(
                scanned_runtime_ids_and_liveness(&isolated_node_registry),
                [],
                "{wrong_shape}"
            );
            assert!(
                entry_file_path.exists(),
                "{wrong_shape}: a reader must not delete a record it cannot parse"
            );
        }
    }

    /// The version field exists so a reader rejects what it cannot parse. Parsing it far enough
    /// to prune it would delete a record written by a newer engine.
    #[test]
    fn an_entry_whose_schema_version_is_unknown_is_neither_listed_nor_deleted() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let entry_file_path = isolated_node_registry.write_registry_entry(&NodeRegistryEntry {
            schema_version: NODE_REGISTRY_SCHEMA_VERSION + 1,
            ..a_registry_entry_hosted_by("Rfuture", nothing_listens(), PID_NO_PROCESS_HAS)
        });

        assert_eq!(
            scanned_runtime_ids_and_liveness(&isolated_node_registry),
            []
        );
        assert!(
            entry_file_path.exists(),
            "a reader must not delete a record it cannot parse"
        );
    }

    /// Entries are per run, so a schema-2 entry, which names no socket, has nothing to migrate.
    /// It is skipped, and a dead pid does not get it pruned.
    #[test]
    fn a_schema_two_entry_is_refused_by_its_version_and_never_pruned() {
        assert_eq!(NODE_REGISTRY_SCHEMA_VERSION, 3);
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let entry_file_path = isolated_node_registry.write_registry_entry_file(
            "Rschema-two",
            &serde_json::to_vec(&json!({
                "schema_version": 2,
                "runtime_id": "Rschema-two",
                "runtime_name": "rig-app-schema-two",
                "pid": PID_NO_PROCESS_HAS,
                "hint": "written by an engine before the local API socket",
            }))
            .unwrap(),
        );

        assert_eq!(
            scanned_runtime_ids_and_liveness(&isolated_node_registry),
            []
        );
        assert!(
            entry_file_path.exists(),
            "a reader must not delete a record it cannot parse"
        );
    }

    /// A released engine from before the TCP listener went writes schema 3 with its URL key beside
    /// the socket path; the reader ignores every key it does not read, so an app pinned to that
    /// engine still runs.
    #[test]
    fn a_schema_three_entry_carrying_a_key_this_reader_does_not_read_is_listed() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_default();
        let mut entry_json = serde_json::to_value(a_registry_entry(
            "Rextra-key",
            &stub_local_api_server.local_api_socket_path,
        ))
        .unwrap();
        entry_json["a_key_this_reader_does_not_read"] = json!("http://127.0.0.1:9000");
        isolated_node_registry
            .write_registry_entry_file("Rextra-key", &serde_json::to_vec(&entry_json).unwrap());

        assert_eq!(
            scanned_runtime_ids_and_liveness(&isolated_node_registry),
            owned(&[("Rextra-key", true)])
        );
    }

    #[test]
    fn a_registry_that_cannot_be_read_is_refused_naming_it() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let node_registry_directory = isolated_node_registry.node_registry_directory();
        std::fs::create_dir_all(node_registry_directory.parent().unwrap()).unwrap();
        std::fs::write(
            &node_registry_directory,
            b"a file where the registry should be",
        )
        .unwrap();

        let refusal =
            scan_liveness_check_and_prune_node_registry(&node_registry_directory).unwrap_err();

        assert!(
            matches!(
                refusal,
                LocalApiRuntimeSelectionFailure::NodeRegistryUnreadable(_)
            ),
            "{refusal:?}"
        );
        assert!(
            refusal
                .to_string()
                .contains(&node_registry_directory.display().to_string()),
            "{refusal}"
        );
    }

    #[test]
    fn a_runtime_directory_refusal_passes_through_in_the_engines_own_words() {
        let runtime_directory_refusal = || StreamlibRuntimeDirectoryRefusal::CannotBeTrusted {
            path: PathBuf::from("/tmp/streamlib-1000"),
            what_is_wrong: "it is a symlink, not a directory".to_owned(),
        };

        assert_eq!(
            LocalApiRuntimeSelectionFailure::RuntimeDirectoryRefused(runtime_directory_refusal())
                .to_string(),
            runtime_directory_refusal().to_string()
        );
    }

    #[test]
    fn the_sole_live_runtime_is_the_default_target() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry(
            "Ronly",
            &stub_local_api_server.local_api_socket_path,
        ));

        for no_runtime_requested in [None, Some("")] {
            assert_eq!(
                selected_runtime(&isolated_node_registry, no_runtime_requested)
                    .unwrap()
                    .local_api_socket_path,
                stub_local_api_server.local_api_socket_path,
                "{no_runtime_requested:?}"
            );
        }
    }

    #[test]
    fn a_runtime_that_is_not_answering_is_never_the_default_target() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry("Rbusy", nothing_listens()));
        isolated_node_registry.write_registry_entry(&a_registry_entry(
            "Ronly",
            &stub_local_api_server.local_api_socket_path,
        ));

        assert_eq!(
            selected_runtime(&isolated_node_registry, None)
                .unwrap()
                .runtime_id,
            "Ronly"
        );
    }

    #[test]
    fn a_named_runtime_resolves_to_its_local_api_socket() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let first_stub_local_api_server = StubLocalApiServer::serve_default();
        let second_stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry(
            "Rfirst",
            &first_stub_local_api_server.local_api_socket_path,
        ));
        isolated_node_registry.write_registry_entry(&a_registry_entry(
            "Rsecond",
            &second_stub_local_api_server.local_api_socket_path,
        ));

        assert_eq!(
            selected_runtime(&isolated_node_registry, Some("Rsecond"))
                .unwrap()
                .local_api_socket_path,
            second_stub_local_api_server.local_api_socket_path
        );
    }

    #[test]
    fn two_live_runtimes_and_no_flag_is_a_refusal_that_lists_them() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let first_stub_local_api_server = StubLocalApiServer::serve_default();
        let second_stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry(
            "Rfirst",
            &first_stub_local_api_server.local_api_socket_path,
        ));
        isolated_node_registry.write_registry_entry(&a_registry_entry(
            "Rsecond",
            &second_stub_local_api_server.local_api_socket_path,
        ));

        let refusal = selected_runtime(&isolated_node_registry, None).unwrap_err();

        assert_eq!(
            refusal.to_string(),
            format!(
                "2 live runtimes — pick one with `--node <runtime name or id>`. Live runtimes: \
                 rig-app-Rfirst (Rfirst) -> {}, rig-app-Rsecond (Rsecond) -> {}",
                first_stub_local_api_server.local_api_socket_path.display(),
                second_stub_local_api_server.local_api_socket_path.display()
            )
        );
    }

    #[test]
    fn no_live_runtime_is_the_distinct_refusal_that_names_the_command_that_starts_one() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        isolated_node_registry.write_registry_entry(&a_registry_entry("Rbusy", nothing_listens()));

        for requested_runtime_name_or_id in [None, Some("rig-app-Rbusy")] {
            let refusal = selected_runtime(&isolated_node_registry, requested_runtime_name_or_id)
                .unwrap_err();

            assert!(
                matches!(refusal, LocalApiRuntimeSelectionFailure::NoRunningRuntime),
                "{refusal:?}"
            );
            assert_eq!(
                refusal.to_string(),
                "no running runtime found on this machine.\nStart one with `tatolab run`."
            );
        }
    }

    #[test]
    fn a_runtime_is_targeted_by_its_runtime_name_and_still_by_its_runtime_id() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let named_stub_local_api_server = StubLocalApiServer::serve_default();
        let other_stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rnamed",
            "rig-desk-a1b2",
            &named_stub_local_api_server.local_api_socket_path,
        ));
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rother",
            "rig-lab-c3d4",
            &other_stub_local_api_server.local_api_socket_path,
        ));

        for requested_runtime_name_or_id in ["rig-desk-a1b2", "Rnamed"] {
            assert_eq!(
                selected_runtime(&isolated_node_registry, Some(requested_runtime_name_or_id))
                    .unwrap()
                    .local_api_socket_path,
                named_stub_local_api_server.local_api_socket_path,
                "{requested_runtime_name_or_id}"
            );
        }
    }

    #[test]
    fn a_runtime_name_is_matched_before_a_runtime_id() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let named_stub_local_api_server = StubLocalApiServer::serve_default();
        let identified_stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rnamed",
            "Rshared",
            &named_stub_local_api_server.local_api_socket_path,
        ));
        isolated_node_registry.write_registry_entry(&a_registry_entry(
            "Rshared",
            &identified_stub_local_api_server.local_api_socket_path,
        ));

        assert_eq!(
            selected_runtime(&isolated_node_registry, Some("Rshared"))
                .unwrap()
                .runtime_id,
            "Rnamed"
        );
    }

    #[test]
    fn a_node_flag_naming_nothing_says_so_and_lists_what_is_live() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rnamed",
            "rig-desk-a1b2",
            &stub_local_api_server.local_api_socket_path,
        ));

        let refusal =
            selected_runtime(&isolated_node_registry, Some("rig-nowhere-0000")).unwrap_err();

        assert_eq!(
            refusal.to_string(),
            format!(
                "no live runtime named `rig-nowhere-0000`, and none with that runtime_id. Live \
                 runtimes: rig-desk-a1b2 (Rnamed) -> {}",
                stub_local_api_server.local_api_socket_path.display()
            )
        );
    }

    #[test]
    fn two_runtimes_answering_to_one_name_are_named_rather_than_picked_between() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let first_stub_local_api_server = StubLocalApiServer::serve_default();
        let second_stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rfirst",
            "rig-desk-a1b2",
            &first_stub_local_api_server.local_api_socket_path,
        ));
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rsecond",
            "rig-desk-a1b2",
            &second_stub_local_api_server.local_api_socket_path,
        ));

        let refusal = selected_runtime(&isolated_node_registry, Some("rig-desk-a1b2")).unwrap_err();

        assert_eq!(
            refusal.to_string(),
            format!(
                "2 live runtimes answer to `rig-desk-a1b2` — pick one by runtime_id with `--node \
                 <runtime_id>`. Live runtimes: rig-desk-a1b2 (Rfirst) -> {}, rig-desk-a1b2 \
                 (Rsecond) -> {}",
                first_stub_local_api_server.local_api_socket_path.display(),
                second_stub_local_api_server.local_api_socket_path.display()
            )
        );
    }

    #[test]
    fn a_host_process_exists_while_it_runs_and_a_pid_naming_none_does_not() {
        assert!(host_process_exists(std::process::id()));
        assert!(
            host_process_exists(1),
            "pid 1 always runs, and EPERM still means it exists"
        );
        assert!(!host_process_exists(PID_NO_PROCESS_HAS));
        assert!(!host_process_exists(PID_OUTSIDE_PID_T));
    }
}
