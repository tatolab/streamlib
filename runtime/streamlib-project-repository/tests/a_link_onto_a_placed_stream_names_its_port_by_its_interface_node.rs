// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use streamlib_project_repository::{
    ProjectOperation, ProjectRepository, ProjectStreamDocumentOperation, ProjectTransactionAuthor,
    ProjectTransactionLineage, StreamGraphDocumentLink, StreamGraphDocumentLinkEnd,
    StreamGraphDocumentNode, StreamGraphOperation, StreamGraphOperationOnAStream,
    graph_document_as_its_project_reads_it,
};
use uuid::Uuid;

const MAIN: &str = "0b6a9c1e-2f47-4d83-9e15-6c7a8b9d0e21";
const TITLE: &str = "c82f4e17-6a3b-4d95-b1e0-7f2a9c4d3b58";
const VIDEO_IN: &str = "7a460338-4840-544c-af0a-7f50ab1d9367";
const VIDEO_OUT: &str = "910e4314-4d41-5cb3-a067-cafd2ef2c2ce";
const PLACING_NODE: &str = "16ae2834-a12e-5387-987f-36c73db1bb37";
const CAMERA: &str = "4d1f7a2b-8c3e-4b69-a5d0-e2f19c7b3a86";
const PREVIEW: &str = "e3b8c5d1-7a49-4f26-b0c3-5d9e1a7f2b64";
const IMPORTED_PROJECT: &str = "a7d3e9c1-5b2f-4a86-9c04-e1f6b8d2a359";

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).unwrap()
}

/// A project holding the stream `title`, whose `GraphInput` declares `video_in_port` and whose
/// `GraphOutput` declares `video_out`, and the stream `main`, which places it as the node
/// `title` between a camera and a preview, linked by `main_links`.
struct ProjectPlacingTitleInMain {
    _root: tempfile::TempDir,
    repository: ProjectRepository,
}

impl ProjectPlacingTitleInMain {
    fn new(video_in_port: &str, main_links: Value) -> Self {
        Self::placing(json!({ "asset": TITLE }), video_in_port, main_links)
    }

    /// The project placing `title` from its import `titles`, which holds it, rather than from
    /// its own streams.
    fn placing_it_from_an_import(main_links: Value) -> Self {
        let project = Self::placing(
            json!({ "asset": TITLE, "from": IMPORTED_PROJECT }),
            "video_in",
            main_links,
        );
        let project_directory = project.project_directory();
        std::fs::remove_file(project_directory.join("streams/title_card.graph.json")).unwrap();
        let import_folder = project_directory.join("imports/titles");
        std::fs::create_dir_all(import_folder.join("streams")).unwrap();
        std::fs::write(
            import_folder.join("tatolab-project.json"),
            json!({ "project": IMPORTED_PROJECT, "name": "titles", "dependencies": [] })
                .to_string(),
        )
        .unwrap();
        write_title(&import_folder, "video_in");
        let project_file = project_directory.join("tatolab-project.json");
        let mut held: Value =
            serde_json::from_slice(&std::fs::read(&project_file).unwrap()).unwrap();
        held["dependencies"] = json!([{
            "project": IMPORTED_PROJECT, "folder": "titles", "source": "titles",
            "version": "0".repeat(64), "copied_files": {}
        }]);
        std::fs::write(&project_file, held.to_string()).unwrap();
        Self {
            repository: ProjectRepository::open(&project_directory).unwrap(),
            _root: project._root,
        }
    }

    fn placing(held_stream: Value, video_in_port: &str, main_links: Value) -> Self {
        let root = tempfile::tempdir().unwrap();
        let project_directory = root.path().join("stage");
        ProjectRepository::create(
            &project_directory,
            "stage",
            &ProjectTransactionAuthor::the_runtime(),
        )
        .unwrap();
        std::fs::create_dir_all(project_directory.join("streams")).unwrap();
        write_title(&project_directory, video_in_port);
        std::fs::write(
            project_directory.join("streams/main.graph.json"),
            json!({
                "asset": MAIN,
                "nodes": [
                    { "id": CAMERA, "name": "camera", "type": "nodes.camera:Camera" },
                    {
                        "id": PLACING_NODE,
                        "name": "title",
                        "type": "tatolab.project:StreamFragment",
                        "input_values": { "stream": held_stream }
                    },
                    { "id": PREVIEW, "name": "preview", "type": "nodes.preview:Preview" }
                ],
                "links": main_links
            })
            .to_string(),
        )
        .unwrap();
        Self {
            repository: ProjectRepository::open(&project_directory).unwrap(),
            _root: root,
        }
    }

    fn project_directory(&self) -> PathBuf {
        self.repository.project_directory().to_path_buf()
    }

    fn main_on_disk(&self) -> Value {
        serde_json::from_slice(
            &std::fs::read(self.project_directory().join("streams/main.graph.json")).unwrap(),
        )
        .unwrap()
    }

    fn main_as_the_repository_reads_it(&self) -> Value {
        ProjectRepository::open(&self.project_directory())
            .unwrap()
            .stream_graph_document(uuid(MAIN))
            .unwrap()
            .to_value()
    }

    fn main_as_it_runs(&self) -> Result<Value, String> {
        graph_document_as_its_project_reads_it(&self.project_directory(), &self.main_on_disk())
    }

    fn apply(&mut self, operations: Vec<ProjectOperation>) {
        self.apply_by(&ProjectTransactionAuthor::the_runtime(), operations);
    }

    fn apply_by(&mut self, author: &ProjectTransactionAuthor, operations: Vec<ProjectOperation>) {
        self.repository
            .apply_project_operations(
                author,
                None,
                Vec::new(),
                operations,
                ProjectTransactionLineage::NewEdit,
            )
            .unwrap();
    }

    fn undo(&mut self) {
        self.undo_by(&ProjectTransactionAuthor::the_runtime());
    }

    fn undo_by(&mut self, author: &ProjectTransactionAuthor) {
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
}

fn write_title(project_directory: &Path, video_in_port: &str) {
    std::fs::write(
        project_directory.join("streams/title_card.graph.json"),
        json!({
            "asset": TITLE,
            "nodes": [
                {
                    "id": VIDEO_IN,
                    "name": "video_in",
                    "type": "tatolab.stream:GraphInput",
                    "input_values": { "family": "image", "port": video_in_port }
                },
                {
                    "id": VIDEO_OUT,
                    "name": "video_out",
                    "type": "tatolab.stream:GraphOutput",
                    "input_values": { "family": "image", "port": "video_out" }
                }
            ],
            "links": [
                {
                    "source": { "node": VIDEO_IN, "port": "value" },
                    "target": { "node": VIDEO_OUT, "port": "value" }
                }
            ]
        })
        .to_string(),
    )
    .unwrap();
}

fn a_link(source: &str, source_port: &str, target: &str, target_port: &str) -> Value {
    json!({
        "source": { "node": source, "port": source_port },
        "target": { "node": target, "port": target_port }
    })
}

fn a_document_link(
    source: &str,
    source_port: &str,
    target: &str,
    target_port: &str,
) -> StreamGraphDocumentLink {
    StreamGraphDocumentLink {
        source: StreamGraphDocumentLinkEnd {
            node: uuid(source),
            port: source_port.to_owned(),
            member: None,
        },
        target: StreamGraphDocumentLinkEnd {
            node: uuid(target),
            port: target_port.to_owned(),
            member: None,
        },
    }
}

fn on(stream: &str, graph_operation: StreamGraphOperation) -> ProjectOperation {
    ProjectOperation::StreamGraph(StreamGraphOperationOnAStream {
        stream: uuid(stream),
        graph_operation,
    })
}

fn the_links_of(document: &Value) -> Vec<Value> {
    document["links"].as_array().cloned().unwrap_or_default()
}

#[test]
fn a_link_made_onto_a_placed_streams_ports_is_written_by_each_interface_nodes_uuid() {
    let mut project = ProjectPlacingTitleInMain::new("video_in", json!([]));
    project.apply(vec![
        on(
            MAIN,
            StreamGraphOperation::Connect {
                link: a_document_link(CAMERA, "video", PLACING_NODE, "video_in"),
            },
        ),
        on(
            MAIN,
            StreamGraphOperation::Connect {
                link: a_document_link(PLACING_NODE, "video_out", PREVIEW, "video"),
            },
        ),
    ]);

    assert_eq!(
        the_links_of(&project.main_on_disk()),
        vec![
            a_link(CAMERA, "video", PLACING_NODE, VIDEO_IN),
            a_link(PLACING_NODE, VIDEO_OUT, PREVIEW, "video"),
        ]
    );
    assert_eq!(
        the_links_of(&project.main_as_the_repository_reads_it()),
        vec![
            a_link(CAMERA, "video", PLACING_NODE, "video_in"),
            a_link(PLACING_NODE, "video_out", PREVIEW, "video"),
        ]
    );
}

#[test]
fn a_port_renamed_in_the_placed_streams_file_keeps_each_link_onto_it_under_the_new_name() {
    let project = ProjectPlacingTitleInMain::new(
        "video_in",
        json!([a_link(CAMERA, "video", PLACING_NODE, VIDEO_IN)]),
    );
    write_title(&project.project_directory(), "clip_in");

    let renamed = vec![a_link(CAMERA, "video", PLACING_NODE, "clip_in")];
    assert_eq!(
        the_links_of(&project.main_as_the_repository_reads_it()),
        renamed
    );
    assert_eq!(the_links_of(&project.main_as_it_runs().unwrap()), renamed);
}

#[test]
fn a_port_renamed_by_a_transaction_leaves_the_placing_streams_file_as_it_was_and_undoes() {
    let mut project = ProjectPlacingTitleInMain::new(
        "video_in",
        json!([a_link(CAMERA, "video", PLACING_NODE, VIDEO_IN)]),
    );
    let main_before = project.main_on_disk();
    let title_before = project
        .repository
        .stream_graph_document(uuid(TITLE))
        .unwrap();
    let rename = StreamGraphOperation::SetNodeInputValue {
        node: uuid(VIDEO_IN),
        input: "port".to_owned(),
        member: None,
        from: Some(json!("video_in")),
        to: Some(json!("clip_in")),
    };
    let mut title_after = title_before.clone();
    title_after.apply(&rename).unwrap();
    let carry = project
        .repository
        .stream_graph_document(uuid(MAIN))
        .unwrap()
        .the_links_carried_onto_the_ports_of_a_placed_stream(
            uuid(TITLE),
            &title_before,
            &title_after,
        )
        .unwrap()
        .expect("the rename carries the link onto the renamed port");

    project.apply(vec![on(TITLE, rename), on(MAIN, carry)]);

    assert_eq!(project.main_on_disk(), main_before);
    assert_eq!(
        the_links_of(&project.main_as_the_repository_reads_it()),
        vec![a_link(CAMERA, "video", PLACING_NODE, "clip_in")]
    );

    project.undo();

    assert_eq!(project.main_on_disk(), main_before);
    assert_eq!(
        the_links_of(&project.main_as_the_repository_reads_it()),
        vec![a_link(CAMERA, "video", PLACING_NODE, "video_in")]
    );
}

#[test]
fn a_document_linking_a_placed_stream_by_its_port_names_is_read_so_and_saved_by_uuid() {
    let by_names = json!([
        a_link(CAMERA, "video", PLACING_NODE, "video_in"),
        a_link(PLACING_NODE, "video_out", PREVIEW, "video"),
    ]);
    let mut project = ProjectPlacingTitleInMain::new("video_in", by_names.clone());

    assert_eq!(
        json!(the_links_of(&project.main_as_the_repository_reads_it())),
        by_names
    );
    assert_eq!(
        json!(the_links_of(&project.main_as_it_runs().unwrap())),
        by_names
    );

    project.apply(vec![on(
        MAIN,
        StreamGraphOperation::RenameNode {
            node: uuid(PREVIEW),
            from: "preview".to_owned(),
            to: "monitor".to_owned(),
        },
    )]);

    assert_eq!(
        the_links_of(&project.main_on_disk()),
        vec![
            a_link(CAMERA, "video", PLACING_NODE, VIDEO_IN),
            a_link(PLACING_NODE, VIDEO_OUT, PREVIEW, "video"),
        ]
    );
}

#[test]
fn a_link_naming_an_interface_node_the_placed_stream_does_not_hold_is_refused_by_name() {
    let gone = "5f3c9a7e-1b2d-4e8f-a6c0-9d7b3e1f4a25";
    let project = ProjectPlacingTitleInMain::new(
        "video_in",
        json!([a_link(CAMERA, "video", PLACING_NODE, gone)]),
    );

    let refusal = project.main_as_it_runs().unwrap_err();

    assert_eq!(
        refusal,
        format!(
            "a link onto node `title` names its port by the `GraphInput` node \
             {gone}, which the stream it places does not hold"
        )
    );
}

fn the_rename_of_title_video_in_to(to: &str) -> ProjectOperation {
    on(
        TITLE,
        StreamGraphOperation::SetNodeInputValue {
            node: uuid(VIDEO_IN),
            input: "port".to_owned(),
            member: None,
            from: Some(json!("video_in")),
            to: Some(json!(to)),
        },
    )
}

#[test]
fn a_port_renamed_in_its_own_stream_beside_an_edit_of_the_placing_stream_keeps_its_link() {
    let mut project = ProjectPlacingTitleInMain::new(
        "video_in",
        json!([a_link(CAMERA, "video", PLACING_NODE, VIDEO_IN)]),
    );

    project.apply(vec![
        the_rename_of_title_video_in_to("clip_in"),
        on(
            MAIN,
            StreamGraphOperation::RenameNode {
                node: uuid(PREVIEW),
                from: "preview".to_owned(),
                to: "monitor".to_owned(),
            },
        ),
    ]);

    assert_eq!(
        the_links_of(&project.main_on_disk()),
        vec![a_link(CAMERA, "video", PLACING_NODE, VIDEO_IN)]
    );
    assert_eq!(
        the_links_of(&project.main_as_the_repository_reads_it()),
        vec![a_link(CAMERA, "video", PLACING_NODE, "clip_in")]
    );
}

#[test]
fn a_link_made_before_its_port_is_renamed_by_another_author_still_undoes() {
    let mut project = ProjectPlacingTitleInMain::new("video_in", json!([]));
    let person = ProjectTransactionAuthor {
        kind: streamlib_project_repository::ProjectTransactionAuthorKind::Person,
        name: "jo".to_owned(),
    };
    project.apply_by(
        &person,
        vec![on(
            MAIN,
            StreamGraphOperation::Connect {
                link: a_document_link(CAMERA, "video", PLACING_NODE, "video_in"),
            },
        )],
    );
    project.apply(vec![the_rename_of_title_video_in_to("clip_in")]);

    project.undo_by(&person);

    assert_eq!(the_links_of(&project.main_on_disk()), Vec::<Value>::new());
}

#[test]
fn a_stream_created_and_placed_in_one_transaction_is_linked_by_its_interface_nodes_uuid() {
    let mut project = ProjectPlacingTitleInMain::new("video_in", json!([]));
    let lower_third = "2c8f5a1d-6e3b-4d97-b0a4-f1e7c9d3b528";
    let lower_third_in = "9b4e2d7a-1c5f-4a38-8e60-d3b7f1a9c245";
    let placing_lower_third = "6e1a9d3c-7b2f-4c85-a9d0-b4f8e2c1d736";
    let mut placing_node = StreamGraphDocumentNode {
        id: uuid(placing_lower_third),
        name: "lower third".to_owned(),
        node_type: "tatolab.project:StreamFragment".to_owned(),
        input_values: Default::default(),
        exposed_config_fields: Vec::new(),
        studio: Default::default(),
        fragment: None,
    };
    placing_node
        .input_values
        .insert("stream".to_owned(), json!({ "asset": lower_third }));

    project.apply(vec![
        ProjectOperation::StreamDocument(ProjectStreamDocumentOperation::CreateStreamDocument {
            stream: uuid(lower_third),
            path: streamlib_project_repository::ProjectRelativeAssetPath::parse(
                "streams/lower_third.graph.json",
            )
            .unwrap(),
            document: json!({
                "asset": lower_third,
                "nodes": [{
                    "id": lower_third_in, "name": "picture",
                    "type": "tatolab.stream:GraphInput",
                    "input_values": { "family": "image", "port": "picture" }
                }],
                "links": []
            }),
        }),
        on(
            MAIN,
            StreamGraphOperation::AddNode {
                node: placing_node,
                links: vec![a_document_link(
                    CAMERA,
                    "video",
                    placing_lower_third,
                    "picture",
                )],
            },
        ),
    ]);

    assert_eq!(
        the_links_of(&project.main_on_disk()),
        vec![a_link(CAMERA, "video", placing_lower_third, lower_third_in)]
    );
}

#[test]
fn a_link_onto_a_stream_placed_from_an_import_names_its_port_by_the_imports_interface_node() {
    let mut project = ProjectPlacingTitleInMain::placing_it_from_an_import(json!([]));

    project.apply(vec![on(
        MAIN,
        StreamGraphOperation::Connect {
            link: a_document_link(CAMERA, "video", PLACING_NODE, "video_in"),
        },
    )]);

    assert_eq!(
        the_links_of(&project.main_on_disk()),
        vec![a_link(CAMERA, "video", PLACING_NODE, VIDEO_IN)]
    );
    let by_name = vec![a_link(CAMERA, "video", PLACING_NODE, "video_in")];
    assert_eq!(
        the_links_of(&project.main_as_the_repository_reads_it()),
        by_name
    );
    assert_eq!(the_links_of(&project.main_as_it_runs().unwrap()), by_name);
}

#[test]
fn a_link_inside_a_placed_stream_onto_a_stream_it_places_runs_by_its_port_name() {
    let project = ProjectPlacingTitleInMain::new("video_in", json!([]));
    let outer = "3f7b1e9a-4c2d-4e85-b6a0-d9c3f8e1b247";
    let outer_in = "8c2e6a4f-1d9b-4f37-a5e0-c7b3d1f9e862";
    let title_inside_outer = "b5d9e3a1-7f4c-4a26-9e08-f2c6a1d8b394";
    std::fs::write(
        project.project_directory().join("streams/outer.graph.json"),
        json!({
            "asset": outer,
            "nodes": [
                {
                    "id": outer_in, "name": "outer_in", "type": "tatolab.stream:GraphInput",
                    "input_values": { "family": "image", "port": "outer_in" }
                },
                {
                    "id": title_inside_outer, "name": "title",
                    "type": "tatolab.project:StreamFragment",
                    "input_values": { "stream": { "asset": TITLE } }
                }
            ],
            "links": [a_link(outer_in, "value", title_inside_outer, VIDEO_IN)]
        })
        .to_string(),
    )
    .unwrap();
    let main = json!({
        "asset": MAIN,
        "nodes": [
            { "id": CAMERA, "name": "camera", "type": "nodes.camera:Camera" },
            {
                "id": PLACING_NODE, "name": "outer", "type": "tatolab.project:StreamFragment",
                "input_values": { "stream": { "asset": outer } }
            }
        ],
        "links": [a_link(CAMERA, "video", PLACING_NODE, outer_in)]
    });

    let read = graph_document_as_its_project_reads_it(&project.project_directory(), &main).unwrap();

    assert_eq!(
        the_links_of(&read),
        vec![a_link(CAMERA, "video", PLACING_NODE, "outer_in")]
    );
    let placed = |own: &str| Uuid::new_v5(&uuid(PLACING_NODE), uuid(own).as_bytes()).to_string();
    assert_eq!(
        the_links_of(&read["nodes"][1]["fragment"]),
        vec![a_link(
            &placed(outer_in),
            "value",
            &placed(title_inside_outer),
            "video_in"
        )]
    );
}
