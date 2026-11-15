// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn an_origin_named(client_name: Option<&str>) -> ControlRequestOrigin {
    ControlRequestOrigin {
        client_name: client_name.map(str::to_owned),
        subscriptions: None,
        viewers_held_open: None,
    }
}

#[test]
fn a_change_with_no_author_is_by_the_agent_its_client_named() {
    let author = the_author_of_a_change(None, &an_origin_named(Some("tato-studio")));
    assert_eq!(author.kind, ProjectTransactionAuthorKind::Agent);
    assert_eq!(author.name, "tato-studio");
}

#[test]
fn a_change_from_an_unnamed_or_blank_client_is_by_an_unnamed_client() {
    for client_name in [None, Some("  ")] {
        let author = the_author_of_a_change(None, &an_origin_named(client_name));
        assert_eq!(author.name, AUTHOR_NAME_OF_AN_UNNAMED_CLIENT);
    }
}

#[test]
fn a_named_author_is_recorded_as_named_whatever_the_client() {
    let author = the_author_of_a_change(
        Some(ProjectTransactionAuthorArgument {
            kind: ProjectTransactionAuthorKindArgument::Person,
            name: "Dana".to_owned(),
        }),
        &an_origin_named(Some("tato-studio")),
    );
    assert_eq!(author.kind, ProjectTransactionAuthorKind::Person);
    assert_eq!(author.name, "Dana");
}
