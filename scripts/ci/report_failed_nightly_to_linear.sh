#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Reports a failed scheduled run to Linear: comments on the open Tatolab issue
# with this title, or opens one labelled Bug.
#
# Usage: report_failed_nightly_to_linear.sh <issue title> <body>
# Needs LINEAR_API_KEY, a Linear personal API key sent as-is in Authorization.
set -euo pipefail

failed_nightly_issue_title="$1"
failed_nightly_report_body="$2"
linear_team_key="TAT"

if [ -z "${LINEAR_API_KEY:-}" ]; then
  echo "::error::LINEAR_API_KEY does not reach this repository — add a Linear personal API key to Infisical's prod environment, which syncs it to the organization's secrets"
  exit 1
fi

linear_graphql() {
  local response
  response="$(curl --silent --show-error --fail-with-body https://api.linear.app/graphql \
    --header "Authorization: $LINEAR_API_KEY" \
    --header 'Content-Type: application/json' \
    --data "$(jq --null-input --arg query "$1" --argjson variables "$2" \
      '{query: $query, variables: $variables}')")"
  if [ "$(jq '.errors // [] | length' <<<"$response")" != "0" ]; then
    echo "::error::Linear refused the request: $(jq --compact-output '.errors' <<<"$response")"
    exit 1
  fi
  printf '%s' "$response"
}

open_issue_id="$(linear_graphql \
  'query($teamKey: String!, $title: String!) {
     issues(first: 1, filter: {
       team: { key: { eq: $teamKey } },
       title: { eq: $title },
       state: { type: { nin: ["completed", "canceled", "duplicate"] } }
     }) { nodes { id } }
   }' \
  "$(jq --null-input --arg teamKey "$linear_team_key" --arg title "$failed_nightly_issue_title" \
    '{teamKey: $teamKey, title: $title}')" \
  | jq --raw-output '.data.issues.nodes[0].id // empty')"

if [ -n "$open_issue_id" ]; then
  linear_graphql \
    'mutation($issueId: String!, $body: String!) {
       commentCreate(input: { issueId: $issueId, body: $body }) { success }
     }' \
    "$(jq --null-input --arg issueId "$open_issue_id" --arg body "$failed_nightly_report_body" \
      '{issueId: $issueId, body: $body}')" >/dev/null
  exit 0
fi

team_and_bug_label="$(linear_graphql \
  'query($teamKey: String!) {
     teams(filter: { key: { eq: $teamKey } }) {
       nodes { id labels(filter: { name: { eq: "Bug" } }) { nodes { id } } }
     }
   }' \
  "$(jq --null-input --arg teamKey "$linear_team_key" '{teamKey: $teamKey}')")"

linear_graphql \
  'mutation($teamId: String!, $title: String!, $description: String!, $labelIds: [String!]) {
     issueCreate(input: { teamId: $teamId, title: $title, description: $description, labelIds: $labelIds }) { success }
   }' \
  "$(jq --null-input \
    --arg teamId "$(jq --raw-output '.data.teams.nodes[0].id' <<<"$team_and_bug_label")" \
    --arg title "$failed_nightly_issue_title" \
    --arg description "$failed_nightly_report_body" \
    --argjson labelIds "$(jq '[.data.teams.nodes[0].labels.nodes[0].id // empty]' <<<"$team_and_bug_label")" \
    '{teamId: $teamId, title: $title, description: $description, labelIds: $labelIds}')" >/dev/null
