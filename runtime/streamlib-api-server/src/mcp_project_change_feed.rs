// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Each open project's change feed: the resource `streamlib://project/<project>/transactions`
//! holds its newest transactions, and a `subscriptions/listen` naming it is sent
//! `notifications/resources/updated` for every transaction applied after.

use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::model::{
    Notification, ReadResourceResult, ResourceContents, ResourceTemplate,
    ResourceUpdatedNotificationParam, ServerNotification, SubscriptionFilter,
};
use rmcp::service::SubscriptionContext;
use serde_json::json;
use streamlib_project_repository::{
    AppliedProjectTransaction, ProjectRepositoriesOpenInThisRuntime, ProjectTransactionAuthorKind,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ProjectTransactionOnTheWire, ProjectTransactionsResourceContents,
};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const PROJECT_TRANSACTIONS_RESOURCE_URI_PREFIX: &str = "streamlib://project/";
const PROJECT_TRANSACTIONS_RESOURCE_URI_SUFFIX: &str = "/transactions";

/// How many of a project's newest transactions its resource holds.
const PROJECT_TRANSACTIONS_RESOURCE_TRANSACTION_COUNT: usize = 64;

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
    .with_description("A project's change feed, by its UUID as `list_assets` answers it under `transactions_resource_uri`: its newest transactions, newest first, each with its `transaction` UUID, its `commit`, its `parent_commit`, its author's kind and name, and its `operations` and `inverse_operations`. Name the URI in `subscriptions/listen`'s resource subscriptions to be sent `notifications/resources/updated` for it each time a transaction applies. Served for a project an asset tool has opened since the runtime started.")
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
        .clone()
        .unwrap_or_default();
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
        let transactions = project_repository
            .applied_transactions_newest_first(PROJECT_TRANSACTIONS_RESOURCE_TRANSACTION_COUNT)
            .map_err(|refusal| {
                McpError::internal_error(format!("project {project}'s history could not be read: {refusal}"), None)
            })?
            .into_iter()
            .map(project_transaction_on_the_wire)
            .collect();
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
) -> ProjectTransactionOnTheWire {
    let AppliedProjectTransaction {
        transaction,
        commit,
        applied_at_unix_seconds,
    } = applied;
    let operation_json = |operation| serde_json::to_value(operation).unwrap_or_default();
    ProjectTransactionOnTheWire {
        format_version: transaction.format_version,
        transaction: transaction.transaction.to_string(),
        commit,
        parent_commit: transaction.parent_commit,
        author_kind: match transaction.author.kind {
            ProjectTransactionAuthorKind::Person => "person",
            ProjectTransactionAuthorKind::Agent => "agent",
            ProjectTransactionAuthorKind::Runtime => "runtime",
        }
        .to_owned(),
        author_name: transaction.author.name,
        operations: transaction.operations.iter().map(operation_json).collect(),
        inverse_operations: transaction
            .inverse_operations
            .iter()
            .map(operation_json)
            .collect(),
        undoes: transaction.undoes.map(|undone| undone.to_string()),
        redoes: transaction.redoes.map(|redone| redone.to_string()),
        idempotency_key: transaction
            .idempotency_key
            .map(|idempotency_key| idempotency_key.as_str().to_owned()),
        applied_at: rfc_3339_utc_of_unix_seconds(applied_at_unix_seconds),
    }
}

/// `unix_seconds` as RFC 3339 in UTC to the second, `YYYY-MM-DDTHH:MM:SSZ`.
fn rfc_3339_utc_of_unix_seconds(unix_seconds: i64) -> String {
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

#[cfg(test)]
mod tests {
    use super::rfc_3339_utc_of_unix_seconds;

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
