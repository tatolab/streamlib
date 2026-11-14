// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{Map, Value, json};
use streamlib_project_repository::{
    ProjectOperation, ProjectRepository, ProjectRepositoryRefusal, ProjectTransactionAuthor,
    ProjectTransactionAuthorKind, ProjectTransactionLineage, STUDIO_CONTRACT_VERSION,
    StreamGraphDocument, StreamGraphDocumentNote, StreamGraphNodeStudioFields,
    StreamGraphNoteFields, StreamGraphOperation, StreamGraphOperationOnAStream,
};
use uuid::Uuid;

const A_FREE_STUDIO_MAP_DOCUMENT: &str =
    include_str!("fixtures/studio-contract/a_graph_document_with_a_free_studio_map.graph.json");
const A_STUDIO_CONTRACT_1_DOCUMENT: &str =
    include_str!("fixtures/studio-contract/a_graph_document_under_studio_contract_1.graph.json");
const CAMERA_NODE_UUID: &str = "0b7e4c1a-2d3f-4a5b-8c6d-9e0f1a2b3c4d";
const DISPLAY_NODE_UUID: &str = "1c8f5d2b-3e4a-4b6c-9d7e-0f1a2b3c4d5e";
const FIXTURE_NOTE_UUID: &str = "7d3e9a1b-5c2f-4e8d-a6b0-1f2e3d4c5b6a";

fn a_document_of(fixture: &str) -> StreamGraphDocument {
    StreamGraphDocument::from_value(serde_json::from_str(fixture).unwrap()).unwrap()
}

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).unwrap()
}

fn a_callers_studio(studio: Value) -> Result<StreamGraphNodeStudioFields, String> {
    StreamGraphNodeStudioFields::from_a_callers_studio(studio.as_object().unwrap().clone())
}

fn a_note(text: &str, position: [i64; 2], size: [i64; 2]) -> StreamGraphNoteFields {
    StreamGraphNoteFields {
        text: text.to_owned(),
        position: position.map(Into::into),
        size: size.map(Into::into),
    }
}

#[test]
fn a_document_written_with_a_free_studio_map_still_loads_its_unread_keys_under_metadata() {
    let document = a_document_of(A_FREE_STUDIO_MAP_DOCUMENT);

    let camera = document.node(uuid(CAMERA_NODE_UUID)).unwrap();
    assert_eq!(camera.studio.position, Some([120.into(), 40.into()]));
    assert_eq!(camera.studio.colour.as_deref(), Some("#3b82f6"));
    assert_eq!(camera.studio.group, None, "`sources` is no group's UUID");
    assert_eq!(
        Value::Object(camera.studio.metadata.clone()),
        json!({ "group": "sources", "zoom": 1.5 })
    );
    let display = document.node(uuid(DISPLAY_NODE_UUID)).unwrap();
    assert_eq!(display.studio.position, None);
    assert_eq!(display.studio.collapsed, Some(true));
    assert_eq!(
        Value::Object(display.studio.metadata.clone()),
        json!({ "position": "left" })
    );
    assert!(document.notes().unwrap().is_empty());
}

#[test]
fn a_document_under_studio_contract_1_reads_every_fixed_key_and_writes_back_as_it_was() {
    let mut document = a_document_of(A_STUDIO_CONTRACT_1_DOCUMENT);

    let camera = document.node(uuid(CAMERA_NODE_UUID)).unwrap();
    assert_eq!(
        *camera.studio,
        StreamGraphNodeStudioFields {
            position: Some([120.into(), 40.into()]),
            group: Some(uuid("2c6a1f7e-93b4-4d0a-8e15-b6f0c3a9d421")),
            comment: Some("the rig's front camera".to_owned()),
            colour: Some("#3b82f6".to_owned()),
            collapsed: Some(false),
            metadata: json!({ "zoom": 1.5 }).as_object().unwrap().clone(),
        }
    );
    assert_eq!(
        document.notes().unwrap(),
        vec![StreamGraphDocumentNote {
            id: uuid(FIXTURE_NOTE_UUID),
            fields: a_note(
                "Swap the camera for the rig's second one before the demo.",
                [40, 300],
                [240, 120]
            ),
        }]
    );

    document
        .apply(&StreamGraphOperation::SetStudioFields {
            node: camera.id,
            from: camera.studio.clone(),
            to: camera.studio.clone(),
        })
        .unwrap();
    assert_eq!(
        document.to_value(),
        serde_json::from_str::<Value>(A_STUDIO_CONTRACT_1_DOCUMENT).unwrap(),
        "a node's studio written back under the contract is the document it was read from"
    );
    assert_eq!(STUDIO_CONTRACT_VERSION, 1);
}

#[test]
fn a_fixed_studio_key_of_the_wrong_shape_is_refused_by_name() {
    for (studio, refusal_names) in [
        (json!({ "position": [1] }), "`studio.position`"),
        (json!({ "position": ["a", "b"] }), "`studio.position`"),
        (json!({ "group": "sources" }), "`studio.group`"),
        (json!({ "comment": 3 }), "`studio.comment`"),
        (json!({ "colour": false }), "`studio.colour`"),
        (json!({ "collapsed": "yes" }), "`studio.collapsed`"),
        (json!({ "metadata": [1, 2] }), "`studio.metadata`"),
    ] {
        let refusal = a_callers_studio(studio.clone()).unwrap_err();
        assert!(refusal.contains(refusal_names), "{studio}: {refusal}");
    }
}

#[test]
fn a_key_outside_the_studio_contract_is_refused_by_name_and_kept_under_metadata() {
    let refusal = a_callers_studio(json!({ "position": [0, 0], "zoom": 2 })).unwrap_err();
    assert!(refusal.contains("`studio.zoom`"), "{refusal}");
    assert!(refusal.contains("`metadata`"), "{refusal}");

    let kept = a_callers_studio(json!({ "position": [0, 0], "metadata": { "zoom": 2 } })).unwrap();
    assert_eq!(Value::Object(kept.metadata.clone()), json!({ "zoom": 2 }));
    assert_eq!(
        Value::Object(kept.to_map()),
        json!({ "contract": 1, "position": [0, 0], "metadata": { "zoom": 2 } })
    );
}

#[test]
fn a_studio_contract_newer_than_this_runtime_reads_is_refused_naming_the_versions_it_reads() {
    let refusal = a_callers_studio(json!({ "contract": 2, "position": [0, 0] })).unwrap_err();
    assert!(refusal.contains("`studio.contract` is 2"), "{refusal}");
    assert!(refusal.contains("1 to 1"), "{refusal}");

    let mut document: Value = serde_json::from_str(A_STUDIO_CONTRACT_1_DOCUMENT).unwrap();
    document["studio"]["contract"] = json!(2);
    let refusal = StreamGraphDocument::from_value(document)
        .unwrap()
        .notes()
        .unwrap_err();
    assert!(refusal.contains("`studio.contract` is 2"), "{refusal}");
}

#[test]
fn a_graphs_studio_holding_a_key_other_than_notes_is_refused_by_name() {
    let mut document: Value = serde_json::from_str(A_STUDIO_CONTRACT_1_DOCUMENT).unwrap();
    document["studio"]["viewport"] = json!([0, 0]);
    let refusal = StreamGraphDocument::from_value(document)
        .unwrap()
        .notes()
        .unwrap_err();
    assert!(refusal.contains("`studio.viewport`"), "{refusal}");
}

#[test]
fn a_note_carrying_a_key_a_note_does_not_take_is_refused_by_name() {
    let mut document: Value = serde_json::from_str(A_STUDIO_CONTRACT_1_DOCUMENT).unwrap();
    document["studio"]["notes"][0]["colour"] = json!("amber");
    let refusal = StreamGraphDocument::from_value(document)
        .unwrap()
        .notes()
        .unwrap_err();
    assert!(refusal.contains("`colour`"), "{refusal}");
}

struct ProjectHoldingOneStream {
    _root: tempfile::TempDir,
    repository: ProjectRepository,
    stream: Uuid,
}

impl ProjectHoldingOneStream {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let project_directory = root.path().join("arm");
        ProjectRepository::create(
            &project_directory,
            "arm",
            &ProjectTransactionAuthor::the_runtime(),
        )
        .unwrap();
        let stream = Uuid::new_v4();
        std::fs::create_dir_all(project_directory.join("streams")).unwrap();
        std::fs::write(
            project_directory.join("streams/main.graph.json"),
            json!({ "asset": stream, "nodes": [], "links": [] }).to_string(),
        )
        .unwrap();
        Self {
            repository: ProjectRepository::open(&project_directory).unwrap(),
            _root: root,
            stream,
        }
    }

    fn apply(
        &mut self,
        author: &ProjectTransactionAuthor,
        graph_operation: StreamGraphOperation,
    ) -> Result<(), ProjectRepositoryRefusal> {
        self.repository
            .apply_project_operations(
                author,
                None,
                Vec::new(),
                vec![ProjectOperation::StreamGraph(
                    StreamGraphOperationOnAStream {
                        stream: self.stream,
                        graph_operation,
                    },
                )],
                ProjectTransactionLineage::NewEdit,
            )
            .map(|_| ())
    }

    fn undo(&mut self, author: &ProjectTransactionAuthor) {
        let reversal = self.repository.undo_by(author).unwrap();
        self.repository
            .apply_project_operations(
                author,
                None,
                Vec::new(),
                reversal.operations,
                reversal.lineage,
            )
            .unwrap();
    }

    fn the_notes_on_disk(&self) -> Vec<StreamGraphDocumentNote> {
        ProjectRepository::open(self.repository.project_directory())
            .unwrap()
            .stream_graph_document(self.stream)
            .unwrap()
            .notes()
            .unwrap()
    }

    fn the_documents_own_studio_on_disk(&self) -> Option<Value> {
        let bytes = std::fs::read(
            self.repository
                .project_directory()
                .join("streams/main.graph.json"),
        )
        .unwrap();
        serde_json::from_slice::<Map<String, Value>>(&bytes)
            .unwrap()
            .remove("studio")
    }
}

#[test]
fn a_canvas_note_is_added_edited_removed_and_each_undone() {
    let mut project = ProjectHoldingOneStream::new();
    let person = ProjectTransactionAuthor {
        kind: ProjectTransactionAuthorKind::Person,
        name: "jo".to_owned(),
    };
    let note = StreamGraphDocumentNote {
        id: Uuid::new_v4(),
        fields: a_note("check the exposure", [40, 300], [240, 120]),
    };

    project
        .apply(
            &person,
            StreamGraphOperation::AddNote { note: note.clone() },
        )
        .unwrap();
    assert_eq!(project.the_notes_on_disk(), vec![note.clone()]);
    assert_eq!(
        project.the_documents_own_studio_on_disk(),
        Some(json!({
            "contract": 1,
            "notes": [{ "id": note.id, "text": "check the exposure",
                        "position": [40, 300], "size": [240, 120] }],
        }))
    );

    let edited = a_note(
        "check the exposure and the white balance",
        [60, 320],
        [320, 160],
    );
    project
        .apply(
            &person,
            StreamGraphOperation::SetNoteFields {
                note: note.id,
                from: note.fields.clone(),
                to: edited.clone(),
            },
        )
        .unwrap();
    assert_eq!(project.the_notes_on_disk()[0].fields, edited);

    project
        .apply(
            &person,
            StreamGraphOperation::RemoveNote {
                note: StreamGraphDocumentNote {
                    id: note.id,
                    fields: edited.clone(),
                },
            },
        )
        .unwrap();
    assert!(project.the_notes_on_disk().is_empty());
    assert_eq!(
        project.the_documents_own_studio_on_disk(),
        None,
        "a graph with no note writes no `studio`"
    );

    project.undo(&person);
    assert_eq!(project.the_notes_on_disk()[0].fields, edited);
    project.undo(&person);
    assert_eq!(project.the_notes_on_disk(), vec![note]);
    project.undo(&person);
    assert!(project.the_notes_on_disk().is_empty());
}

#[test]
fn an_edit_of_a_note_the_graph_does_not_hold_is_refused_naming_it() {
    let mut project = ProjectHoldingOneStream::new();
    let missing = Uuid::new_v4();
    let refusal = project
        .apply(
            &ProjectTransactionAuthor::the_runtime(),
            StreamGraphOperation::SetNoteFields {
                note: missing,
                from: a_note("a", [0, 0], [1, 1]),
                to: a_note("b", [0, 0], [1, 1]),
            },
        )
        .unwrap_err();
    assert!(
        refusal.to_string().contains(&missing.to_string()),
        "{refusal}"
    );
}
