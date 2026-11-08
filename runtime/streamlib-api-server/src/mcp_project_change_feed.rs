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

fn project_transaction_on_the_wire(
    applied: AppliedProjectTransaction,
) -> ProjectTransactionOnTheWire {
    let AppliedProjectTransaction {
        transaction,
        commit,
    } = applied;
    let operation_json = |operation| serde_json::to_value(operation).unwrap_or_default();
    ProjectTransactionOnTheWire {
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
    }
}
