// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::{Map, Value, json};
use streamlib_project_repository::{
    ProjectOperation, ProjectRepository, ProjectRepositoryRefusal, ProjectTransactionAuthor,
    ProjectTransactionAuthorKind, ProjectTransactionLineage, StreamGraphDocumentNode,
    StreamGraphOperation, StreamGraphOperationOnAStream,
};
use uuid::Uuid;

fn a_person() -> ProjectTransactionAuthor {
    ProjectTransactionAuthor {
        kind: ProjectTransactionAuthorKind::Person,
        name: "jo".to_owned(),
    }
}

struct ProjectWithOneStream {
    repository: ProjectRepository,
    stream: Uuid,
}

impl ProjectWithOneStream {
    fn created_in(root: &std::path::Path, name: &str) -> Self {
        let project_directory = root.join(name);
        ProjectRepository::create(
            &project_directory,
            name,
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
            stream,
        }
    }

    fn add_a_node_named(&mut self, name: &str) -> String {
        self.repository
            .apply_project_operations(
                &a_person(),
                vec![ProjectOperation::StreamGraph(
                    StreamGraphOperationOnAStream {
                        stream: self.stream,
                        graph_operation: StreamGraphOperation::AddNode {
                            node: StreamGraphDocumentNode {
                                id: Uuid::new_v4(),
                                name: name.to_owned(),
                                node_type: "nodes.arm:Joint".to_owned(),
                                config: json!({}),
                                pins: Map::new(),
                                exposed_config_fields: Vec::new(),
                                studio: Map::new(),
                                fragment: None,
                            },
                            links: Vec::new(),
                        },
                    },
                )],
                ProjectTransactionLineage::NewEdit,
            )
            .unwrap()
            .commit
    }

    fn source(&self) -> String {
        self.repository.project_directory().display().to_string()
    }
}

fn node_names_of_the_main_stream_at_the_pin(
    repository: &ProjectRepository,
    project: Uuid,
) -> Vec<String> {
    let files = repository.dependency_files_at_its_pin(project).unwrap();
    let document: Value = serde_json::from_slice(&files["streams/main.graph.json"]).unwrap();
    document["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_dependency_is_read_at_its_pin_until_an_update_moves_it_and_undo_moves_it_back() {
    let root = tempfile::tempdir().unwrap();
    let mut arm = ProjectWithOneStream::created_in(root.path(), "arm");
    let first_revision = arm.add_a_node_named("shoulder");
    let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");
    let arm_project = arm.repository.project_file().project;

    cell.repository
        .add_dependency(&a_person(), &arm.source(), &first_revision[..10])
        .unwrap();
    let second_revision = arm.add_a_node_named("elbow");

    let pinned = cell.repository.dependency(arm_project).unwrap();
    assert_eq!(pinned.revision, first_revision);
    assert_eq!(pinned.source, arm.source());
    assert_eq!(
        node_names_of_the_main_stream_at_the_pin(&cell.repository, arm_project),
        ["shoulder"]
    );
    assert_eq!(
        ProjectRepository::open(cell.repository.project_directory())
            .unwrap()
            .project_file()
            .dependencies,
        vec![pinned]
    );

    cell.repository
        .update_dependency(&a_person(), arm_project, &second_revision)
        .unwrap();
    assert_eq!(
        node_names_of_the_main_stream_at_the_pin(&cell.repository, arm_project),
        ["shoulder", "elbow"]
    );

    let reversal = cell.repository.undo_by(&a_person()).unwrap();
    cell.repository
        .apply_project_operations(&a_person(), reversal.operations, reversal.lineage)
        .unwrap();
    assert_eq!(
        cell.repository.dependency(arm_project).unwrap().revision,
        first_revision
    );
}

#[test]
fn a_revision_the_source_does_not_hold_is_refused_and_nothing_is_pinned() {
    let root = tempfile::tempdir().unwrap();
    let mut arm = ProjectWithOneStream::created_in(root.path(), "arm");
    arm.add_a_node_named("shoulder");
    let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");

    let refusal = cell
        .repository
        .add_dependency(&a_person(), &arm.source(), "v9")
        .unwrap_err();

    assert!(
        matches!(refusal, ProjectRepositoryRefusal::NoSuchRevision { ref revision, .. } if revision == "v9"),
        "{refusal}"
    );
    assert!(cell.repository.project_file().dependencies.is_empty());
    assert_eq!(
        std::fs::read_dir(
            cell.repository
                .project_directory()
                .join(".tatolab/dependencies")
        )
        .unwrap()
        .count(),
        0
    );
}

#[test]
fn a_project_is_refused_as_its_own_dependency_and_a_second_add_of_one_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let mut arm = ProjectWithOneStream::created_in(root.path(), "arm");
    let revision = arm.add_a_node_named("shoulder");
    let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");
    let source = arm.source();

    assert!(matches!(
        arm.repository
            .add_dependency(&a_person(), &source, &revision),
        Err(ProjectRepositoryRefusal::ProjectDependsOnItself(_))
    ));
    assert!(
        !arm.repository
            .project_directory()
            .join(".tatolab/dependencies")
            .join(arm.repository.project_file().project.to_string())
            .exists()
    );
    cell.repository
        .add_dependency(&a_person(), &source, &revision)
        .unwrap();
    assert!(matches!(
        cell.repository
            .add_dependency(&a_person(), &source, &revision),
        Err(ProjectRepositoryRefusal::DependencyAlreadyHeld(_))
    ));
}
