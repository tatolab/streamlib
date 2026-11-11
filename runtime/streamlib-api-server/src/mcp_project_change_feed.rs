// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Each open project's change feed: the resource `streamlib://project/<project>/transactions`
//! holds its newest transactions and the actions refused since the runtime started, and a
//! `subscriptions/listen` naming it is sent `notifications/resources/updated` for every
//! transaction applied or action refused after.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use parking_lot::Mutex;

use rmcp::ErrorData as McpError;
use rmcp::model::{
    Notification, ReadResourceResult, ResourceContents, ResourceTemplate,
    ResourceUpdatedNotificationParam, ServerNotification, SubscriptionFilter,
};
use rmcp::service::SubscriptionContext;
use serde_json::json;
use streamlib_project_repository::{
    AppliedProjectTransaction, ProjectActionIdempotencyKey, ProjectRepositoriesOpenInThisRuntime,
    ProjectTransactionAuthor, ProjectTransactionAuthorKind, ProjectTransactionUndoState,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    AssetMediaFactsReadOnTheWire, ProjectActionRefusalOnTheWire, ProjectChangeFeedEntryOnTheWire,
    ProjectTransactionOnTheWire, ProjectTransactionsResourceContents,
    RefusedProjectActionOnTheWire,
};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const PROJECT_TRANSACTIONS_RESOURCE_URI_PREFIX: &str = "streamlib://project/";
const PROJECT_TRANSACTIONS_RESOURCE_URI_SUFFIX: &str = "/transactions";

/// How many of a project's newest transactions its resource holds.
const PROJECT_TRANSACTIONS_RESOURCE_TRANSACTION_COUNT: usize = 64;

/// How many of a project's latest refused actions the runtime keeps.
const REFUSED_PROJECT_ACTIONS_KEPT_PER_PROJECT: usize = 100;

/// One refused action as the runtime keeps it, with the second it was refused at.
struct RefusedProjectActionKeptInMemory {
    refused_at_unix_seconds: i64,
    refused_action: RefusedProjectActionOnTheWire,
}

/// The latest actions the runtime refused on each project since it started, held in memory
/// only, since no history records a refusal.
#[derive(Default)]
pub(crate) struct ProjectActionsRefusedSinceTheRuntimeStarted {
    refused_actions_oldest_first_by_project:
        Mutex<HashMap<Uuid, VecDeque<RefusedProjectActionKeptInMemory>>>,
}

/// An action on a project the runtime refused, as its caller sent it.
pub(crate) struct ProjectActionTheRuntimeRefused<'action> {
    pub(crate) author: &'action ProjectTransactionAuthor,
    pub(crate) mode: &'static str,
    pub(crate) operations_as_requested: Vec<serde_json::Value>,
    pub(crate) idempotency_key: Option<&'action ProjectActionIdempotencyKey>,
    pub(crate) refusal: ProjectActionRefusalOnTheWire,
}

impl ProjectActionsRefusedSinceTheRuntimeStarted {
    /// Keep `refused` among `project`'s latest refusals and announce it on the change feed.
    pub(crate) fn keep_and_announce(
        &self,
        project_repositories: &ProjectRepositoriesOpenInThisRuntime,
        project: Uuid,
        refused: ProjectActionTheRuntimeRefused<'_>,
    ) {
        let refused_at_unix_seconds = the_wall_clock_in_unix_seconds();
        let refused_action = RefusedProjectActionOnTheWire {
            author_kind: author_kind_on_the_wire(&refused.author.kind).to_owned(),
            author_name: refused.author.name.clone(),
            mode: refused.mode.to_owned(),
            operations: refused.operations_as_requested,
            idempotency_key: refused
                .idempotency_key
                .map(|idempotency_key| idempotency_key.as_str().to_owned()),
            refusal: refused.refusal,
            refused_at: rfc_3339_utc_of_unix_seconds(refused_at_unix_seconds),
        };
        {
            let mut refused_actions_oldest_first_by_project =
                self.refused_actions_oldest_first_by_project.lock();
            let refused_actions_oldest_first = refused_actions_oldest_first_by_project
                .entry(project)
                .or_default();
            if refused_actions_oldest_first.len() == REFUSED_PROJECT_ACTIONS_KEPT_PER_PROJECT {
                refused_actions_oldest_first.pop_front();
            }
            refused_actions_oldest_first.push_back(RefusedProjectActionKeptInMemory {
                refused_at_unix_seconds,
                refused_action,
            });
        }
        project_repositories.announce_a_refused_action(project);
    }

    /// `project`'s kept refusals, newest first, each with the second it was refused at.
    fn the_refused_actions_newest_first(
        &self,
        project: Uuid,
    ) -> Vec<(i64, RefusedProjectActionOnTheWire)> {
        self.refused_actions_oldest_first_by_project
            .lock()
            .get(&project)
            .map(|refused_actions_oldest_first| {
                refused_actions_oldest_first
                    .iter()
                    .rev()
                    .map(|kept| (kept.refused_at_unix_seconds, kept.refused_action.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// `applied`, `media_facts_read` and `refused`, each newest first, merged newest first; in the
/// same second a refusal comes first, then a finished media facts read, then an applied
/// transaction.
fn the_change_feed_newest_first(
    applied: Vec<AppliedProjectTransaction>,
    undo_state_of_each_transaction: &HashMap<Uuid, ProjectTransactionUndoState>,
    media_facts_read: Vec<(i64, AssetMediaFactsReadOnTheWire)>,
    refused: Vec<(i64, RefusedProjectActionOnTheWire)>,
) -> Vec<ProjectChangeFeedEntryOnTheWire> {
    const APPLIED_IN_THE_SAME_SECOND_RANK: u8 = 0;
    const MEDIA_FACTS_READ_IN_THE_SAME_SECOND_RANK: u8 = 1;
    const REFUSED_IN_THE_SAME_SECOND_RANK: u8 = 2;
    let mut change_feed: Vec<(i64, u8, ProjectChangeFeedEntryOnTheWire)> = applied
        .into_iter()
        .map(|applied| {
            (
                applied.applied_at_unix_seconds,
                APPLIED_IN_THE_SAME_SECOND_RANK,
                ProjectChangeFeedEntryOnTheWire::Applied(project_transaction_on_the_wire(
                    applied,
                    undo_state_of_each_transaction,
                )),
            )
        })
        .chain(
            media_facts_read
                .into_iter()
                .map(|(read_at_unix_seconds, read)| {
                    (
                        read_at_unix_seconds,
                        MEDIA_FACTS_READ_IN_THE_SAME_SECOND_RANK,
                        ProjectChangeFeedEntryOnTheWire::MediaFactsRead(read),
                    )
                }),
        )
        .chain(
            refused
                .into_iter()
                .map(|(refused_at_unix_seconds, refused_action)| {
                    (
                        refused_at_unix_seconds,
                        REFUSED_IN_THE_SAME_SECOND_RANK,
                        ProjectChangeFeedEntryOnTheWire::Refused(refused_action),
                    )
                }),
        )
        .collect();
    change_feed.sort_by(|(at, rank, _), (other_at, other_rank, _)| {
        (other_at, other_rank).cmp(&(at, rank))
    });
    change_feed.into_iter().map(|(_, _, entry)| entry).collect()
}

pub(crate) fn the_wall_clock_in_unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since_the_epoch| {
            i64::try_from(since_the_epoch.as_secs()).unwrap_or(i64::MAX)
        })
}

fn author_kind_on_the_wire(author_kind: &ProjectTransactionAuthorKind) -> &'static str {
    match author_kind {
        ProjectTransactionAuthorKind::Person => "person",
        ProjectTransactionAuthorKind::Agent => "agent",
        ProjectTransactionAuthorKind::Runtime => "runtime",
    }
}

/// The change feed of the project whose UUID is `project`.
pub(crate) fn project_transactions_resource_uri(project: Uuid) -> String {
    format!(
        "{PROJECT_TRANSACTIONS_RESOURCE_URI_PREFIX}{project}{PROJECT_TRANSACTIONS_RESOURCE_URI_SUFFIX}"
    )
}

/// The project a transactions resource URI names, or `None` for any other URI.
pub(crate) fn the_project_a_transactions_resource_uri_names(uri: &str) -> Option<Uuid> {
    let project = uri
        .strip_prefix(PROJECT_TRANSACTIONS_RESOURCE_URI_PREFIX)?
        .strip_suffix(PROJECT_TRANSACTIONS_RESOURCE_URI_SUFFIX)?;
    Uuid::parse_str(project).ok()
}

/// The `resources/templates/list` entry every project's change feed is read through.
pub(crate) fn project_transactions_resource_template() -> ResourceTemplate {
    ResourceTemplate::new(
        format!("{PROJECT_TRANSACTIONS_RESOURCE_URI_PREFIX}{{project}}{PROJECT_TRANSACTIONS_RESOURCE_URI_SUFFIX}"),
        "project-transactions",
    )
    .with_title("Project transactions")
    .with_description("A project's change feed, by its UUID as `list_assets` answers it under `transactions_resource_uri`: its newest transactions, the actions refused and the assets whose media facts were read since the runtime started, newest first, each tagged by `status`. An `applied` one carries its `transaction` UUID, its `commit`, its `parent_commit`, its author's kind and name, its `operations` and `inverse_operations`, its `format_version`, its `idempotency_key` when it had one, and `applied_at`. A `refused` one carries its author's kind and name, its `mode`, the `operations` it asked for, its `idempotency_key` when it had one, the `refusal` in the runtime's own words, and `refused_at`; the runtime keeps the latest 100 of each project in memory only. A `media_facts_read` one carries the `asset`, the `content_sha256` of the bytes read, its `media_facts_state`, `read` or `unreadable`, and `read_at`, after which `describe_asset` answers what was read; the runtime keeps the latest 100 of each project in memory only. Name the URI in `subscriptions/listen`'s resource subscriptions to be sent `notifications/resources/updated` for it each time a transaction applies, an action is refused or an asset's media facts are read. Served for a project an asset tool has opened since the runtime started.")
    .with_mime_type("application/json")
}

/// The resource subscriptions in `requested` that name a project's change feed.
pub(crate) fn accepted_project_transactions_subscriptions(
    requested: &SubscriptionFilter,
) -> SubscriptionFilter {
    let accepted_resource_subscriptions: Vec<String> = requested
        .resource_subscriptions
        .iter()
        .flatten()
        .filter(|uri| the_project_a_transactions_resource_uri_names(uri).is_some())
        .cloned()
        .collect();
    if accepted_resource_subscriptions.is_empty() {
        return SubscriptionFilter::default();
    }
    SubscriptionFilter::builder()
        .resource_subscriptions(accepted_resource_subscriptions)
        .build()
}

/// Send `context` an update for each transaction on a project it subscribed to, until it is
/// cancelled or the local API stops.
pub(crate) async fn announce_project_transactions_until_the_listen_ends(
    project_repositories: &ProjectRepositoriesOpenInThisRuntime,
    context: &SubscriptionContext,
    local_api_stopping_token: &CancellationToken,
) {
    let subscribed_resource_uris: Vec<String> = context
        .accepted()
        .resource_subscriptions
        .iter()
        .flatten()
        .filter(|uri| the_project_a_transactions_resource_uri_names(uri).is_some())
        .cloned()
        .collect();
    let mut change_feed = project_repositories.subscribe_to_the_change_feed();
    loop {
        let updated_resource_uris: Vec<String> = tokio::select! {
            () = context.cancelled() => return,
            () = local_api_stopping_token.cancelled() => return,
            received = change_feed.recv() => match received {
            Ok(announcement) => vec![project_transactions_resource_uri(announcement.project)],
                Err(RecvError::Lagged(missed_announcement_count)) => {
                    tracing::debug!(missed_announcement_count, "a change-feed listener fell behind; every subscribed project is marked updated");
                    subscribed_resource_uris.clone()
                }
                Err(RecvError::Closed) => return,
            },
        };
        for updated_resource_uri in updated_resource_uris
            .into_iter()
            .filter(|uri| subscribed_resource_uris.contains(uri))
        {
            let update = ServerNotification::ResourceUpdatedNotification(Notification::new(
                ResourceUpdatedNotificationParam::new(updated_resource_uri),
            ));
            if let Err(not_sent) = context.sink().send(update).await {
                tracing::debug!(%not_sent, "a change-feed update was not sent; the listen ends");
                return;
            }
        }
    }
}

/// Read the change feed of `project`, which an asset tool must have opened.
pub(crate) async fn read_project_transactions_resource(
    project_repositories: Arc<ProjectRepositoriesOpenInThisRuntime>,
    project_actions_refused: Arc<ProjectActionsRefusedSinceTheRuntimeStarted>,
    asset_media_facts_reads: Arc<crate::asset_media_facts_reads::AssetMediaFactsReadsInThisRuntime>,
    uri: &str,
    project: Uuid,
) -> Result<ReadResourceResult, McpError> {
    let rendering = tokio::task::spawn_blocking(move || {
        let Some(project_repository) = project_repositories.the_open_project_repository_of(project)
        else {
            return Err(McpError::resource_not_found(
                format!("no project {project} is open in this runtime; an asset tool such as `list_assets` opens it by its directory"),
                Some(json!({ "project": project.to_string() })),
            ));
        };
        let project_repository = project_repository.lock();
        let applied_transactions = project_repository
            .applied_transactions_newest_first(PROJECT_TRANSACTIONS_RESOURCE_TRANSACTION_COUNT)
            .map_err(|refusal| {
                McpError::internal_error(format!("project {project}'s history could not be read: {refusal}"), None)
            })?;
        let undo_state_of_each_transaction = project_repository
            .the_undo_state_of_each_transaction()
            .map_err(|refusal| {
                McpError::internal_error(format!("project {project}'s history could not be read: {refusal}"), None)
            })?;
        let transactions = the_change_feed_newest_first(
            applied_transactions,
            &undo_state_of_each_transaction,
            asset_media_facts_reads.the_finished_reads_newest_first(project),
            project_actions_refused.the_refused_actions_newest_first(project),
        );
        serde_json::to_string_pretty(&ProjectTransactionsResourceContents {
            project: project.to_string(),
            project_directory: project_repository.project_directory().to_path_buf(),
            transactions,
        })
        .map_err(|e| McpError::internal_error(format!("project {project}'s transactions did not render: {e}"), None))
    })
    .await
    .map_err(|join_failure| McpError::internal_error(format!("the read did not finish: {join_failure}"), None))??;
    Ok(ReadResourceResult::new(vec![
        ResourceContents::text(rendering, uri).with_mime_type("application/json"),
    ]))
}

pub(crate) fn project_transaction_on_the_wire(
    applied: AppliedProjectTransaction,
    undo_state_of_each_transaction: &HashMap<Uuid, ProjectTransactionUndoState>,
) -> ProjectTransactionOnTheWire {
    let undo_state = undo_state_of_each_transaction
        .get(&applied.transaction.transaction)
        .map(|undo_state| undo_state_on_the_wire(*undo_state).to_owned());
    let AppliedProjectTransaction {
        transaction,
        commit,
        applied_at_unix_seconds,
    } = applied;
    ProjectTransactionOnTheWire {
        format_version: transaction.format_version,
        transaction: transaction.transaction.to_string(),
        commit,
        parent_commit: transaction.parent_commit,
        author_kind: author_kind_on_the_wire(&transaction.author.kind).to_owned(),
        author_name: transaction.author.name,
        operations: transaction
            .operations
            .iter()
            .map(serialized_on_the_wire)
            .collect(),
        inverse_operations: transaction
            .inverse_operations
            .iter()
            .map(serialized_on_the_wire)
            .collect(),
        undoes: transaction.undoes.map(|undone| undone.to_string()),
        redoes: transaction.redoes.map(|redone| redone.to_string()),
        idempotency_key: transaction
            .idempotency_key
            .map(|idempotency_key| idempotency_key.as_str().to_owned()),
        actions: transaction
            .actions
            .iter()
            .map(serialized_on_the_wire)
            .collect(),
        undo_state,
        applied_at: rfc_3339_utc_of_unix_seconds(applied_at_unix_seconds),
    }
}

fn undo_state_on_the_wire(undo_state: ProjectTransactionUndoState) -> &'static str {
    match undo_state {
        ProjectTransactionUndoState::Applied => "applied",
        ProjectTransactionUndoState::UndoneRedoable => "undone_redoable",
        ProjectTransactionUndoState::UndoneNotRedoable => "undone_not_redoable",
    }
}

/// `unix_seconds` as RFC 3339 in UTC to the second, `YYYY-MM-DDTHH:MM:SSZ`.
pub(crate) fn rfc_3339_utc_of_unix_seconds(unix_seconds: i64) -> String {
    let days_since_the_epoch = unix_seconds.div_euclid(86_400);
    let second_of_the_day = unix_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_date_of_days_since_the_unix_epoch(days_since_the_epoch);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_the_day / 3_600,
        second_of_the_day % 3_600 / 60,
        second_of_the_day % 60
    )
}

/// The proleptic Gregorian `(year, month, day)` `days` after 1970-01-01, by Howard Hinnant's
/// `civil_from_days`.
fn civil_date_of_days_since_the_unix_epoch(days: i64) -> (i64, i64, i64) {
    let days_since_0000_03_01 = days + 719_468;
    let era = days_since_0000_03_01.div_euclid(146_097);
    let day_of_era = days_since_0000_03_01.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn serialized_on_the_wire(recorded: &impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(recorded).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_refusal_of(runtime_words: &str) -> ProjectActionRefusalOnTheWire {
        ProjectActionRefusalOnTheWire {
            action: "remove_node".to_owned(),
            refused_operation_index: Some(0),
            refused_node: None,
            refused_link: None,
            refused_field: None,
            not_running_because: None,
            runtime_words: runtime_words.to_owned(),
        }
    }

    #[test]
    fn a_project_keeps_its_latest_refusals_newest_first_and_each_is_announced() {
        let project_repositories = ProjectRepositoriesOpenInThisRuntime::default();
        let mut change_feed = project_repositories.subscribe_to_the_change_feed();
        let project_actions_refused = ProjectActionsRefusedSinceTheRuntimeStarted::default();
        let (project, other_project) = (Uuid::new_v4(), Uuid::new_v4());
        let author = ProjectTransactionAuthor::the_runtime();
        for refusal_number in 0..=REFUSED_PROJECT_ACTIONS_KEPT_PER_PROJECT {
            project_actions_refused.keep_and_announce(
                &project_repositories,
                project,
                ProjectActionTheRuntimeRefused {
                    author: &author,
                    mode: "commit",
                    operations_as_requested: Vec::new(),
                    idempotency_key: None,
                    refusal: a_refusal_of(&format!("refusal {refusal_number}")),
                },
            );
            assert_eq!(change_feed.try_recv().unwrap().project, project);
        }
        let kept = project_actions_refused.the_refused_actions_newest_first(project);
        assert_eq!(kept.len(), REFUSED_PROJECT_ACTIONS_KEPT_PER_PROJECT);
        assert_eq!(
            kept[0].1.refusal.runtime_words,
            format!("refusal {REFUSED_PROJECT_ACTIONS_KEPT_PER_PROJECT}")
        );
        assert_eq!(
            kept.last().unwrap().1.refusal.runtime_words,
            "refusal 1",
            "the oldest refusal is dropped"
        );
        assert!(
            project_actions_refused
                .the_refused_actions_newest_first(other_project)
                .is_empty()
        );
    }

    #[test]
    fn unix_seconds_render_as_rfc_3339_utc() {
        assert_eq!(rfc_3339_utc_of_unix_seconds(0), "1970-01-01T00:00:00Z");
        assert_eq!(
            rfc_3339_utc_of_unix_seconds(951_782_400),
            "2000-02-29T00:00:00Z"
        );
        assert_eq!(
            rfc_3339_utc_of_unix_seconds(1_794_268_920),
            "2026-11-10T00:02:00Z"
        );
        assert_eq!(rfc_3339_utc_of_unix_seconds(-1), "1969-12-31T23:59:59Z");
    }
}
