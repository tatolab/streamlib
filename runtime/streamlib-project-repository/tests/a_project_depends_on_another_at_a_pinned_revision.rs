// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::{Map, Value, json};
use streamlib_project_repository::{
    ProjectAssetChangeRequest, ProjectAssetForkOrigin, ProjectAssetImportSource, ProjectOperation,
    ProjectRelativeAssetPath, ProjectRepository, ProjectRepositoryRefusal,
    ProjectStreamDocumentOperation, ProjectTransactionAuthor, ProjectTransactionAuthorKind,
    ProjectTransactionLineage, StreamGraphDocumentNode, StreamGraphOperation,
    StreamGraphOperationOnAStream, the_content_path_of_a_dependency_asset,
    the_content_path_of_an_asset,
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
        self.add_a_node(name, json!({}), None).1
    }

    /// Add a node named `name` holding `config` and, for a fragment, `fragment`, answering its
    /// UUID and the commit.
    fn add_a_node(&mut self, name: &str, config: Value, fragment: Option<Value>) -> (Uuid, String) {
        let node = Uuid::new_v4();
        let commit = self
            .repository
            .apply_project_operations(
                &a_person(),
                vec![ProjectOperation::StreamGraph(
                    StreamGraphOperationOnAStream {
                        stream: self.stream,
                        graph_operation: StreamGraphOperation::AddNode {
                            node: StreamGraphDocumentNode {
                                id: node,
                                name: name.to_owned(),
                                node_type: "nodes.arm:Joint".to_owned(),
                                config,
                                pins: Map::new(),
                                exposed_config_fields: Vec::new(),
                                studio: Map::new(),
                                fragment: fragment.map(Box::new),
                            },
                            links: Vec::new(),
                        },
                    },
                )],
                ProjectTransactionLineage::NewEdit,
            )
            .unwrap()
            .commit;
        (node, commit)
    }

    fn undo(&mut self) -> Result<(), ProjectRepositoryRefusal> {
        let reversal = self.repository.undo_by(&a_person())?;
        self.repository
            .apply_project_operations(&a_person(), reversal.operations, reversal.lineage)
            .map(|_| ())
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
        .add_dependency(&a_person(), &arm.source(), &first_revision[..10], None)
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
        .update_dependency(&a_person(), arm_project, &second_revision, None)
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
        .add_dependency(&a_person(), &arm.source(), "v9", None)
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
            .add_dependency(&a_person(), &source, &revision, None),
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
        .add_dependency(&a_person(), &source, &revision, None)
        .unwrap();
    assert!(matches!(
        cell.repository
            .add_dependency(&a_person(), &source, &revision, None),
        Err(ProjectRepositoryRefusal::DependencyAlreadyHeld(_))
    ));
}

#[test]
fn a_forked_stream_is_an_editable_copy_the_dependencys_next_revision_no_longer_reaches() {
    let root = tempfile::tempdir().unwrap();
    let mut arm = ProjectWithOneStream::created_in(root.path(), "arm");
    let first_revision = arm.add_a_node_named("shoulder");
    let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");
    let arm_project = arm.repository.project_file().project;
    cell.repository
        .add_dependency(&a_person(), &arm.source(), &first_revision, None)
        .unwrap();
    let arm_stream_reference = json!({ "stream": { "asset": arm.stream, "from": arm_project } });
    let user_of_the_arm = Uuid::new_v4();
    cell.repository
        .apply_project_operations(
            &a_person(),
            vec![ProjectOperation::StreamGraph(
                StreamGraphOperationOnAStream {
                    stream: cell.stream,
                    graph_operation: StreamGraphOperation::AddNode {
                        node: StreamGraphDocumentNode {
                            id: user_of_the_arm,
                            name: "arm".to_owned(),
                            node_type: "nodes.arm:ArmUser".to_owned(),
                            config: arm_stream_reference.clone(),
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
        .unwrap();

    let (fork, _) = cell
        .repository
        .fork_dependency_asset(&a_person(), arm_project, arm.stream, None)
        .unwrap();
    let second_revision = arm.add_a_node_named("elbow");
    cell.repository
        .update_dependency(&a_person(), arm_project, &second_revision, None)
        .unwrap();

    let copy_on_disk = cell
        .repository
        .project_directory()
        .join("streams/main-fork.graph.json");
    let copy: Value = serde_json::from_slice(&std::fs::read(&copy_on_disk).unwrap()).unwrap();
    assert_eq!(copy["asset"], json!(fork.copy));
    assert_eq!(
        copy["forked_from"],
        json!({ "asset": arm.stream, "from": arm_project, "revision": first_revision })
    );
    let copy_node_names: Vec<&str> = copy["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["name"].as_str().unwrap())
        .collect();
    assert_eq!(copy_node_names, ["shoulder"]);
    let user_after_the_fork = cell
        .repository
        .stream_graph_document(cell.stream)
        .unwrap()
        .node(user_of_the_arm)
        .unwrap();
    assert_eq!(
        user_after_the_fork.config,
        json!({ "stream": { "asset": fork.copy } })
    );

    let mut forked = ProjectWithOneStream {
        repository: ProjectRepository::open(cell.repository.project_directory()).unwrap(),
        stream: fork.copy,
    };
    forked.add_a_node_named("gripper");
    let edited_copy = forked.repository.stream_graph_document(fork.copy).unwrap();
    assert!(edited_copy.node_named("gripper").is_some());
    assert!(edited_copy.node_named("elbow").is_none());
}

#[test]
fn undo_of_a_fork_takes_the_copy_out_and_points_each_reference_back_at_the_dependency() {
    let root = tempfile::tempdir().unwrap();
    let mut arm = ProjectWithOneStream::created_in(root.path(), "arm");
    let revision = arm.add_a_node_named("shoulder");
    let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");
    let arm_project = arm.repository.project_file().project;
    cell.repository
        .add_dependency(&a_person(), &arm.source(), &revision, None)
        .unwrap();
    let arm_stream_reference = json!({ "stream": { "asset": arm.stream, "from": arm_project } });
    let (user_of_the_arm, _) = cell.add_a_node("arm", arm_stream_reference.clone(), None);

    let (fork, _) = cell
        .repository
        .fork_dependency_asset(
            &a_person(),
            arm_project,
            arm.stream,
            Some("streams/arm.graph.json"),
        )
        .unwrap();
    assert!(cell.repository.asset(fork.copy).is_ok());
    cell.undo().unwrap();

    assert_eq!(
        cell.repository
            .stream_graph_document(cell.stream)
            .unwrap()
            .node(user_of_the_arm)
            .unwrap()
            .config,
        arm_stream_reference
    );

    assert!(
        !cell
            .repository
            .project_directory()
            .join("streams/arm.graph.json")
            .exists()
    );
    assert!(matches!(
        cell.repository.asset(fork.copy),
        Err(ProjectRepositoryRefusal::NoSuchAsset(_))
    ));
    assert!(matches!(
        cell.repository
            .fork_dependency_asset(&a_person(), arm_project, Uuid::new_v4(), None),
        Err(ProjectRepositoryRefusal::DependencyHoldsNoAsset { .. })
    ));
}

#[test]
fn a_forked_stream_references_the_dependencys_own_assets_into_the_dependency() {
    let root = tempfile::tempdir().unwrap();
    let arm = ProjectWithOneStream::created_in(root.path(), "arm");
    let lut = Uuid::new_v4();
    std::fs::write(
        arm.repository
            .project_directory()
            .join("streams/main.graph.json"),
        json!({ "asset": arm.stream, "nodes": [{
            "id": Uuid::new_v4(), "name": "lens", "type": "nodes.arm:Lens",
            "config": { "lut": { "asset": lut } } }], "links": [] })
        .to_string(),
    )
    .unwrap();
    let mut arm = ProjectWithOneStream {
        repository: ProjectRepository::open(arm.repository.project_directory()).unwrap(),
        stream: arm.stream,
    };
    let revision = arm.add_a_node_named("shoulder");
    let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");
    let arm_project = arm.repository.project_file().project;
    cell.repository
        .add_dependency(&a_person(), &arm.source(), &revision, None)
        .unwrap();

    let (fork, _) = cell
        .repository
        .fork_dependency_asset(&a_person(), arm_project, arm.stream, None)
        .unwrap();

    let copy = cell.repository.stream_graph_document(fork.copy).unwrap();
    assert_eq!(
        copy.node_named("lens").unwrap().config,
        json!({ "lut": { "asset": lut, "from": arm_project } })
    );
}

fn another_person() -> ProjectTransactionAuthor {
    ProjectTransactionAuthor {
        kind: ProjectTransactionAuthorKind::Person,
        name: "sam".to_owned(),
    }
}

#[test]
fn undo_of_an_add_is_refused_once_another_author_has_moved_the_pin() {
    let root = tempfile::tempdir().unwrap();
    let mut arm = ProjectWithOneStream::created_in(root.path(), "arm");
    let first_revision = arm.add_a_node_named("shoulder");
    let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");
    let arm_project = arm.repository.project_file().project;
    cell.repository
        .add_dependency(&a_person(), &arm.source(), &first_revision, None)
        .unwrap();
    let second_revision = arm.add_a_node_named("elbow");
    cell.repository
        .update_dependency(&another_person(), arm_project, &second_revision, None)
        .unwrap();

    let reversal = cell.repository.undo_by(&a_person()).unwrap();
    let refusal = cell
        .repository
        .apply_project_operations(&a_person(), reversal.operations, reversal.lineage)
        .unwrap_err();

    assert!(
        matches!(refusal, ProjectRepositoryRefusal::DependencyPinnedElsewhere { ref pinned, .. } if *pinned == second_revision),
        "{refusal}"
    );
    assert_eq!(
        cell.repository.dependency(arm_project).unwrap().revision,
        second_revision
    );
}

#[test]
fn a_project_directory_named_by_a_relative_path_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let mut arm = ProjectWithOneStream::created_in(root.path(), "arm");
    let revision = arm.add_a_node_named("shoulder");
    let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");
    let relative_source = pathdiff_from_the_working_directory(arm.repository.project_directory());

    let refusal = cell
        .repository
        .add_dependency(&a_person(), &relative_source, &revision, None)
        .unwrap_err();

    assert!(
        matches!(
            refusal,
            ProjectRepositoryRefusal::DependencySourceNotAnAbsolutePath { .. }
        ),
        "{refusal}"
    );
    assert!(cell.repository.project_file().dependencies.is_empty());
}

/// `directory` spelled relative to this process's working directory, through `..` as needed.
fn pathdiff_from_the_working_directory(directory: &std::path::Path) -> String {
    let working_directory = std::env::current_dir().unwrap();
    let ups = working_directory.components().count() - 1;
    let mut relative = std::path::PathBuf::new();
    for _ in 0..ups {
        relative.push("..");
    }
    relative.push(directory.strip_prefix("/").unwrap());
    relative.display().to_string()
}

/// Project `cell` depending on project `arm` at its first revision, `arm`'s one stream holding
/// a node named `shoulder`.
struct CellDependingOnArm {
    _root: tempfile::TempDir,
    arm: ProjectWithOneStream,
    arm_project: Uuid,
    pinned_revision: String,
    cell: ProjectWithOneStream,
}

impl CellDependingOnArm {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut arm = ProjectWithOneStream::created_in(root.path(), "arm");
        let pinned_revision = arm.add_a_node_named("shoulder");
        let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");
        let arm_project = arm.repository.project_file().project;
        cell.repository
            .add_dependency(&a_person(), &arm.source(), &pinned_revision, None)
            .unwrap();
        Self {
            _root: root,
            arm,
            arm_project,
            pinned_revision,
            cell,
        }
    }

    fn arm_stream_reference(&self) -> Value {
        json!({ "asset": self.arm.stream, "from": self.arm_project })
    }
}

#[test]
fn a_clone_without_the_fetched_history_fetches_it_again_from_the_source_and_keeps_the_pin() {
    let mut depending = CellDependingOnArm::new();
    depending.arm.add_a_node_named("elbow");
    let cell_directory = depending.cell.repository.project_directory().to_path_buf();
    std::fs::remove_dir_all(cell_directory.join(".tatolab/dependencies")).unwrap();
    let mut clone = ProjectWithOneStream {
        repository: ProjectRepository::open(&cell_directory).unwrap(),
        stream: depending.cell.stream,
    };

    assert_eq!(
        node_names_of_the_main_stream_at_the_pin(&clone.repository, depending.arm_project),
        ["shoulder"]
    );
    assert_eq!(
        clone
            .repository
            .dependency(depending.arm_project)
            .unwrap()
            .revision,
        depending.pinned_revision
    );
    assert!(
        cell_directory
            .join(".tatolab/dependencies")
            .join(depending.arm_project.to_string())
            .is_dir()
    );
    let stray_fetches: Vec<_> = std::fs::read_dir(cell_directory.join(".tatolab/dependencies"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".fetching-")
        })
        .collect();
    assert!(stray_fetches.is_empty());

    std::fs::remove_dir_all(cell_directory.join(".tatolab/dependencies")).unwrap();
    let (fork, _) = clone
        .repository
        .fork_dependency_asset(
            &a_person(),
            depending.arm_project,
            depending.arm.stream,
            None,
        )
        .unwrap();
    assert!(clone.repository.asset(fork.copy).is_ok());
}

#[test]
fn a_clone_whose_source_no_longer_holds_the_pinned_commit_is_refused_and_keeps_the_pin() {
    let depending = CellDependingOnArm::new();
    let cell_directory = depending.cell.repository.project_directory().to_path_buf();
    std::fs::remove_dir_all(cell_directory.join(".tatolab/dependencies")).unwrap();
    std::fs::remove_dir_all(
        depending
            .arm
            .repository
            .project_directory()
            .join(".tatolab/history.git"),
    )
    .unwrap();
    streamlib_project_repository::ProjectHistoryRepository::create_for_project(
        depending.arm.repository.project_directory(),
    )
    .unwrap();
    let clone = ProjectRepository::open(&cell_directory).unwrap();

    let refusal = clone
        .dependency_files_at_its_pin(depending.arm_project)
        .unwrap_err();

    assert!(
        matches!(refusal, ProjectRepositoryRefusal::NoSuchRevision { .. }),
        "{refusal}"
    );
    assert_eq!(
        clone.dependency(depending.arm_project).unwrap().revision,
        depending.pinned_revision
    );
    assert!(
        !cell_directory
            .join(".tatolab/dependencies")
            .join(depending.arm_project.to_string())
            .exists()
    );
}

#[test]
fn undo_of_a_fork_is_refused_once_its_copy_has_been_edited() {
    let mut depending = CellDependingOnArm::new();
    let (fork, _) = depending
        .cell
        .repository
        .fork_dependency_asset(
            &a_person(),
            depending.arm_project,
            depending.arm.stream,
            None,
        )
        .unwrap();
    let gripper = Uuid::new_v4();
    depending
        .cell
        .repository
        .apply_project_operations(
            &another_person(),
            vec![ProjectOperation::StreamGraph(
                StreamGraphOperationOnAStream {
                    stream: fork.copy,
                    graph_operation: StreamGraphOperation::AddNode {
                        node: StreamGraphDocumentNode {
                            id: gripper,
                            name: "gripper".to_owned(),
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
        .unwrap();

    let refusal = depending.cell.undo().unwrap_err();

    assert!(
        matches!(
            refusal,
            ProjectRepositoryRefusal::StreamDocumentEditedSinceItWasWritten { .. }
        ),
        "{refusal}"
    );
    let copy = depending
        .cell
        .repository
        .stream_graph_document(fork.copy)
        .unwrap();
    assert!(copy.node(gripper).is_ok());
}

#[test]
fn a_fork_points_a_reference_inside_a_stored_fragment_at_the_copy_and_undo_points_it_back() {
    let mut depending = CellDependingOnArm::new();
    let stored_fragment_referencing_the_arm = json!({
        "nodes": [{ "id": Uuid::new_v4(), "name": "inner", "type": "nodes.arm:ArmUser",
                    "config": { "stream": depending.arm_stream_reference() } }],
        "links": [],
    });
    let (generated, _) = depending.cell.add_a_node(
        "generated",
        json!({ "size": 3 }),
        Some(stored_fragment_referencing_the_arm.clone()),
    );

    let (fork, _) = depending
        .cell
        .repository
        .fork_dependency_asset(
            &a_person(),
            depending.arm_project,
            depending.arm.stream,
            None,
        )
        .unwrap();

    let forked = depending
        .cell
        .repository
        .stream_graph_document(depending.cell.stream)
        .unwrap()
        .node(generated)
        .unwrap();
    assert_eq!(forked.config, json!({ "size": 3 }));
    assert_eq!(
        forked.fragment.unwrap()["nodes"][0]["config"],
        json!({ "stream": { "asset": fork.copy } })
    );
    depending.cell.undo().unwrap();
    let restored = depending
        .cell
        .repository
        .stream_graph_document(depending.cell.stream)
        .unwrap()
        .node(generated)
        .unwrap();
    assert_eq!(
        restored.fragment.map(|fragment| *fragment),
        Some(stored_fragment_referencing_the_arm)
    );
}

#[test]
fn a_fork_is_refused_while_a_referencing_node_holds_no_uuid() {
    let mut depending = CellDependingOnArm::new();
    std::fs::write(
        depending
            .cell
            .repository
            .project_directory()
            .join("streams/main.graph.json"),
        json!({ "asset": depending.cell.stream, "nodes": [{
            "name": "arm", "type": "nodes.arm:ArmUser",
            "config": { "stream": depending.arm_stream_reference() } }], "links": [] })
        .to_string(),
    )
    .unwrap();

    let refusal = depending
        .cell
        .repository
        .fork_dependency_asset(
            &a_person(),
            depending.arm_project,
            depending.arm.stream,
            None,
        )
        .unwrap_err();

    assert!(
        matches!(
            refusal,
            ProjectRepositoryRefusal::ForkReachesANodeHoldingNoUuid { ref node_name, .. }
                if node_name == "arm"
        ),
        "{refusal}"
    );
    assert!(
        !depending
            .cell
            .repository
            .project_directory()
            .join("streams/main-fork.graph.json")
            .exists()
    );
}

#[test]
fn a_created_stream_document_referencing_an_asset_the_project_does_not_hold_is_refused() {
    let mut depending = CellDependingOnArm::new();
    let stream = Uuid::new_v4();
    let missing_asset = Uuid::new_v4();
    let nested_node = Uuid::new_v4();

    let refusal = depending
        .cell
        .repository
        .apply_project_operations(
            &a_person(),
            vec![ProjectOperation::StreamDocument(
                ProjectStreamDocumentOperation::CreateStreamDocument {
                    stream,
                    path: ProjectRelativeAssetPath::parse("streams/made.graph.json").unwrap(),
                    document: json!({ "asset": stream, "links": [], "nodes": [{
                        "id": Uuid::new_v4(), "name": "generated", "type": "generators:Look",
                        "config": {}, "fragment": { "links": [], "nodes": [{
                            "id": nested_node, "name": "lut", "type": "nodes:Lut",
                            "config": { "lut": { "asset": missing_asset } } }] } }] }),
                },
            )],
            ProjectTransactionLineage::NewEdit,
        )
        .unwrap_err();

    assert!(
        matches!(
            refusal,
            ProjectRepositoryRefusal::ConfigReferencesNoAsset { node, asset }
                if node == nested_node && asset == missing_asset
        ),
        "{refusal}"
    );
    assert!(
        !depending
            .cell
            .repository
            .project_directory()
            .join("streams/made.graph.json")
            .exists()
    );
}

/// Install `arm` into `cell`'s venv as the distribution `tatolab-robot-arm`, its project
/// directory and history among the package's files as a wheel would lay them out.
fn install_a_package_shipping(cell: &ProjectWithOneStream, arm: &ProjectWithOneStream) {
    let site_packages = cell
        .repository
        .project_directory()
        .join(".venv/lib/python3.12/site-packages");
    let shipped_project = site_packages.join("tatolab_robot_arm/project");
    copy_directory(arm.repository.project_directory(), &shipped_project);
    let dist_info = site_packages.join("tatolab_robot_arm-1.0.0.dist-info");
    std::fs::create_dir_all(&dist_info).unwrap();
    std::fs::write(
        dist_info.join("METADATA"),
        "Metadata-Version: 2.1\nName: Tatolab_Robot_Arm\nVersion: 1.0.0\n\nThe arm.\n",
    )
    .unwrap();
    std::fs::write(
        dist_info.join("RECORD"),
        "tatolab_robot_arm/__init__.py,,\ntatolab_robot_arm/project/tatolab-project.json,sha256=x,2\ntatolab_robot_arm-1.0.0.dist-info/RECORD,,\n",
    )
    .unwrap();
}

fn copy_directory(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let destination = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_directory(&entry.path(), &destination);
        } else {
            std::fs::copy(entry.path(), destination).unwrap();
        }
    }
}

#[test]
fn a_package_that_ships_a_project_is_pinned_at_that_projects_history() {
    let root = tempfile::tempdir().unwrap();
    let mut arm = ProjectWithOneStream::created_in(root.path(), "arm");
    let shipped_revision = arm.add_a_node_named("shoulder");
    let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");
    let arm_project = arm.repository.project_file().project;

    let refused = cell
        .repository
        .add_dependency(&a_person(), "package:tatolab-robot-arm", "main", None)
        .err()
        .unwrap();
    assert!(
        matches!(
            refused,
            ProjectRepositoryRefusal::DependencyPackageNotInstalled { .. }
        ),
        "{refused:?}"
    );

    install_a_package_shipping(&cell, &arm);
    cell.repository
        .add_dependency(&a_person(), "package:tatolab-robot-arm", "main", None)
        .unwrap();
    let pinned = cell.repository.dependency(arm_project).unwrap();
    assert_eq!(pinned.source, "package:tatolab-robot-arm");
    assert_eq!(pinned.revision, shipped_revision);
    assert_eq!(
        node_names_of_the_main_stream_at_the_pin(&cell.repository, arm_project),
        vec!["shoulder"]
    );

    let upgraded_revision = arm.add_a_node_named("elbow");
    std::fs::remove_dir_all(
        cell.repository
            .project_directory()
            .join(".venv/lib/python3.12/site-packages"),
    )
    .unwrap();
    install_a_package_shipping(&cell, &arm);
    cell.repository
        .update_dependency(&a_person(), arm_project, "main", None)
        .unwrap();
    assert_eq!(
        cell.repository.dependency(arm_project).unwrap().revision,
        upgraded_revision
    );
}

#[test]
fn a_package_that_ships_no_project_is_refused_by_name() {
    let root = tempfile::tempdir().unwrap();
    let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");
    let dist_info = cell
        .repository
        .project_directory()
        .join(".venv/lib/python3.12/site-packages/numpy-2.0.0.dist-info");
    std::fs::create_dir_all(&dist_info).unwrap();
    std::fs::write(dist_info.join("METADATA"), "Name: numpy\nVersion: 2.0.0\n").unwrap();
    std::fs::write(dist_info.join("RECORD"), "numpy/__init__.py,,\n").unwrap();
    let refused = cell
        .repository
        .add_dependency(&a_person(), "package:numpy", "main", None)
        .err()
        .unwrap();
    assert!(
        matches!(
            refused,
            ProjectRepositoryRefusal::DependencyPackageShipsNoProject { .. }
        ),
        "{refused:?}"
    );
}

#[test]
fn a_listing_checked_for_newer_answers_the_sources_newest_commit_beside_the_pin() {
    let root = tempfile::tempdir().unwrap();
    let mut arm = ProjectWithOneStream::created_in(root.path(), "arm");
    let pinned_revision = arm.add_a_node_named("shoulder");
    let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");
    cell.repository
        .add_dependency(&a_person(), &arm.source(), &pinned_revision, None)
        .unwrap();
    let newer_revision = arm.add_a_node_named("elbow");

    let unchecked = cell.repository.dependency_listing(false, None);
    assert_eq!(unchecked.len(), 1);
    assert_eq!(unchecked[0].dependency.revision, pinned_revision);
    assert_eq!(unchecked[0].newest_on_the_source, None);

    let checked = cell.repository.dependency_listing(true, None);
    assert_eq!(checked[0].dependency.revision, pinned_revision);
    assert_eq!(checked[0].newest_on_the_source, Some(Ok(newer_revision)));

    std::fs::remove_dir_all(arm.repository.project_directory()).unwrap();
    let source_gone = cell.repository.dependency_listing(true, None);
    assert!(
        matches!(source_gone[0].newest_on_the_source, Some(Err(_))),
        "{source_gone:?}"
    );
}

#[test]
fn a_dependency_is_removed_only_once_no_node_references_it_and_undo_puts_its_pin_back() {
    let root = tempfile::tempdir().unwrap();
    let mut arm = ProjectWithOneStream::created_in(root.path(), "arm");
    let revision = arm.add_a_node_named("shoulder");
    let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");
    let arm_project = arm.repository.project_file().project;
    cell.repository
        .add_dependency(&a_person(), &arm.source(), &revision, None)
        .unwrap();
    let user = Uuid::new_v4();
    let add_the_user = StreamGraphOperation::AddNode {
        node: StreamGraphDocumentNode {
            id: user,
            name: "arm".to_owned(),
            node_type: "tatolab.project:StreamFragment".to_owned(),
            config: json!({ "stream": { "asset": arm.stream, "from": arm_project } }),
            pins: Map::new(),
            exposed_config_fields: Vec::new(),
            studio: Map::new(),
            fragment: None,
        },
        links: Vec::new(),
    };
    cell.repository
        .apply_project_operations(
            &a_person(),
            vec![ProjectOperation::StreamGraph(
                StreamGraphOperationOnAStream {
                    stream: cell.stream,
                    graph_operation: add_the_user,
                },
            )],
            ProjectTransactionLineage::NewEdit,
        )
        .unwrap();

    let refused = cell
        .repository
        .remove_dependency(&a_person(), arm_project)
        .err()
        .unwrap();
    match &refused {
        ProjectRepositoryRefusal::DependencyStillReferenced {
            referencing_nodes, ..
        } => {
            assert_eq!(referencing_nodes.len(), 1);
            assert!(referencing_nodes[0].contains("streams/main.graph.json"));
            assert!(referencing_nodes[0].contains(&user.to_string()));
        }
        other => panic!("{other:?}"),
    }
    assert!(cell.repository.dependency(arm_project).is_ok());

    cell.repository
        .apply_project_operations(
            &a_person(),
            vec![ProjectOperation::StreamGraph(
                StreamGraphOperationOnAStream {
                    stream: cell.stream,
                    graph_operation: StreamGraphOperation::RemoveNode {
                        node: cell
                            .repository
                            .stream_graph_document(cell.stream)
                            .unwrap()
                            .node_named("arm")
                            .unwrap(),
                        links: Vec::new(),
                    },
                },
            )],
            ProjectTransactionLineage::NewEdit,
        )
        .unwrap();
    cell.repository
        .remove_dependency(&a_person(), arm_project)
        .unwrap();
    assert!(matches!(
        cell.repository.dependency(arm_project),
        Err(ProjectRepositoryRefusal::NoSuchDependency(_))
    ));

    let reversal = cell.repository.undo_by(&a_person()).unwrap();
    cell.repository
        .apply_project_operations(&a_person(), reversal.operations, reversal.lineage)
        .unwrap();
    assert_eq!(
        cell.repository.dependency(arm_project).unwrap().revision,
        revision
    );
    assert_eq!(
        node_names_of_the_main_stream_at_the_pin(&cell.repository, arm_project),
        vec!["shoulder"]
    );
}

impl ProjectWithOneStream {
    /// Import `bytes` as the asset at `asset_path`, answering its UUID and the commit.
    fn import_an_asset(&mut self, asset_path: &str, bytes: &[u8]) -> (Uuid, String) {
        let source = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(source.path(), bytes).unwrap();
        let applied = self
            .repository
            .apply_asset_changes(
                &a_person(),
                vec![ProjectAssetChangeRequest::ImportAsset {
                    source: ProjectAssetImportSource::FileOnThisMachine(
                        source.path().to_path_buf(),
                    ),
                    asset_path: asset_path.to_owned(),
                    kind: None,
                    import_settings: Map::new(),
                }],
            )
            .unwrap();
        let imported = self
            .repository
            .assets()
            .unwrap()
            .into_iter()
            .find(|asset| asset.path.to_string() == asset_path)
            .unwrap();
        (imported.asset, applied.commit)
    }
}

#[test]
fn a_dependencys_asset_bytes_are_copied_in_at_each_fetch_and_read_at_its_pin() {
    let root = tempfile::tempdir().unwrap();
    let mut arm = ProjectWithOneStream::created_in(root.path(), "arm");
    let (grade, first_revision) = arm.import_an_asset("looks/grade.cube", b"grade lut");
    let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");
    let arm_project = arm.repository.project_file().project;
    let cell_directory = cell.repository.project_directory().to_path_buf();
    cell.repository
        .add_dependency(&a_person(), &arm.source(), &first_revision, None)
        .unwrap();
    let (night, second_revision) = arm.import_an_asset("looks/night.cube", b"night lut");

    let grade_path =
        the_content_path_of_a_dependency_asset(&cell_directory, arm_project, grade).unwrap();
    assert!(grade_path.starts_with(cell_directory.join(".tatolab/content")));
    assert_eq!(std::fs::read(&grade_path).unwrap(), b"grade lut");
    assert!(matches!(
        the_content_path_of_a_dependency_asset(&cell_directory, arm_project, night),
        Err(ProjectRepositoryRefusal::DependencyHoldsNoAsset { .. })
    ));

    cell.repository
        .update_dependency(&a_person(), arm_project, &second_revision, None)
        .unwrap();
    std::fs::remove_dir_all(arm.repository.project_directory()).unwrap();
    assert_eq!(
        std::fs::read(
            the_content_path_of_a_dependency_asset(&cell_directory, arm_project, night).unwrap()
        )
        .unwrap(),
        b"night lut"
    );

    cell.undo().unwrap();
    assert!(matches!(
        the_content_path_of_a_dependency_asset(&cell_directory, arm_project, night),
        Err(ProjectRepositoryRefusal::DependencyHoldsNoAsset { .. })
    ));
    assert!(matches!(
        the_content_path_of_a_dependency_asset(&cell_directory, Uuid::new_v4(), grade),
        Err(ProjectRepositoryRefusal::NoSuchDependency(_))
    ));
}

#[test]
fn a_forked_data_asset_is_this_projects_own_file_and_undo_points_each_reference_back() {
    let root = tempfile::tempdir().unwrap();
    let mut arm = ProjectWithOneStream::created_in(root.path(), "arm");
    let (grade, revision) = arm.import_an_asset("looks/grade.cube", b"grade lut");
    let mut cell = ProjectWithOneStream::created_in(root.path(), "cell");
    let arm_project = arm.repository.project_file().project;
    let cell_directory = cell.repository.project_directory().to_path_buf();
    cell.repository
        .add_dependency(&a_person(), &arm.source(), &revision, None)
        .unwrap();
    let dependency_reference = json!({ "lut": { "asset": grade, "from": arm_project } });
    let (grader, _) = cell.add_a_node("grader", dependency_reference.clone(), None);

    let (fork, _) = cell
        .repository
        .fork_dependency_asset(&a_person(), arm_project, grade, None)
        .unwrap();

    let copy = cell.repository.asset(fork.copy).unwrap();
    assert_eq!(copy.path.to_string(), "looks/grade-fork.cube");
    assert_eq!(
        copy.forked_from,
        Some(ProjectAssetForkOrigin {
            asset: grade,
            from: arm_project,
            revision: revision.clone(),
        })
    );
    assert_eq!(
        std::fs::read(cell_directory.join("looks/grade-fork.cube")).unwrap(),
        b"grade lut"
    );
    let sidecar: Value = serde_json::from_slice(
        &std::fs::read(cell_directory.join("looks/grade-fork.cube.asset.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        sidecar["forked_from"],
        json!({ "asset": grade, "from": arm_project, "revision": revision })
    );
    assert_eq!(
        std::fs::read(the_content_path_of_an_asset(&cell_directory, fork.copy).unwrap()).unwrap(),
        b"grade lut"
    );
    assert_eq!(
        cell.repository
            .stream_graph_document(cell.stream)
            .unwrap()
            .node(grader)
            .unwrap()
            .config,
        json!({ "lut": { "asset": fork.copy } })
    );

    cell.undo().unwrap();
    assert!(!cell_directory.join("looks/grade-fork.cube").exists());
    assert!(matches!(
        cell.repository.asset(fork.copy),
        Err(ProjectRepositoryRefusal::NoSuchAsset(_))
    ));
    assert_eq!(
        cell.repository
            .stream_graph_document(cell.stream)
            .unwrap()
            .node(grader)
            .unwrap()
            .config,
        dependency_reference
    );
}
