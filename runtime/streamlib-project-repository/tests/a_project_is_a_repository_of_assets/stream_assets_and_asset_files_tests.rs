// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A stream as the asset its graph document names, and the file an asset
//! resolves to.

use super::*;

#[test]
fn a_stream_is_an_asset_its_graph_document_names_and_it_moves_and_deletes_as_one_file() {
    let fixture = ProjectBesideItsImportSources::new();
    let stream_asset = Uuid::new_v4();
    write_stream_document(
        &fixture.project_directory,
        "streams/arm-control.graph.json",
        &json!({"asset": stream_asset.to_string(), "nodes": [], "links": []}),
    );
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    assert!(
        repository
            .history()
            .files_at_head()
            .unwrap()
            .contains_key("streams/arm-control.graph.json")
    );

    let listed = repository.asset(stream_asset).unwrap();
    assert_eq!(listed.kind, ProjectAssetKind::Stream);
    assert_eq!(listed.path.as_str(), "streams/arm-control.graph.json");

    assert!(matches!(
        repository.apply_asset_changes(
            &agent(),
            None,
            vec![ProjectAssetAction::RelocateAsset {
                asset: stream_asset,
                to_path: "streams/arm-control.json".to_owned(),
            }],
        ),
        Err(ProjectRepositoryRefusal::AssetPathRefused { .. })
    ));
    assert!(matches!(
        repository.apply_asset_changes(
            &agent(), None,
            vec![ProjectAssetAction::SetAssetSettings {
                asset: stream_asset,
                import_settings: Map::new(),
            }],
        ),
        Err(ProjectRepositoryRefusal::StreamAssetHasNoImportSettings(refused)) if refused == stream_asset
    ));

    repository
        .apply_asset_changes(
            &agent(),
            None,
            vec![ProjectAssetAction::RelocateAsset {
                asset: stream_asset,
                to_path: "streams/cell/arm-control.graph.json".to_owned(),
            }],
        )
        .unwrap()
        .into_applied_transaction();
    let moved = repository.asset(stream_asset).unwrap();
    assert_eq!(moved.path.as_str(), "streams/cell/arm-control.graph.json");
    let files_at_head = repository.history().files_at_head().unwrap();
    assert!(files_at_head.contains_key("streams/cell/arm-control.graph.json"));
    assert!(!files_at_head.contains_key("streams/arm-control.graph.json"));
    assert!(
        !files_at_head
            .keys()
            .any(|path| path.ends_with(".graph.json.asset.json"))
    );

    write_stream_document(
        &fixture.project_directory,
        "streams/reader.graph.json",
        &json!({
            "asset": Uuid::new_v4().to_string(),
            "nodes": [{"name": "player",
                       "input_values": {"stream": {"asset": stream_asset.to_string()}}}],
        }),
    );
    match repository.apply_asset_changes(
        &agent(),
        None,
        vec![ProjectAssetAction::DeleteAsset {
            asset: stream_asset,
        }],
    ) {
        Err(ProjectRepositoryRefusal::AssetStillReferenced {
            referencing_documents,
            ..
        }) => assert_eq!(referencing_documents, ["streams/reader.graph.json"]),
        other => panic!("expected the referenced stream to be kept, got {other:?}"),
    }
    std::fs::remove_file(fixture.project_directory.join("streams/reader.graph.json")).unwrap();

    let deleted = repository
        .apply_asset_changes(
            &agent(),
            None,
            vec![ProjectAssetAction::DeleteAsset {
                asset: stream_asset,
            }],
        )
        .unwrap()
        .into_applied_transaction();
    assert!(
        !fixture
            .project_directory
            .join("streams/cell/arm-control.graph.json")
            .exists()
    );
    assert!(matches!(
        repository.asset(stream_asset),
        Err(ProjectRepositoryRefusal::NoSuchAsset(_))
    ));
    match &deleted.transaction.inverse_operations[..] {
        [
            ProjectOperation::StreamDocument(
                ProjectStreamDocumentOperation::CreateStreamDocument {
                    stream, document, ..
                },
            ),
        ] => {
            assert_eq!(*stream, stream_asset);
            assert_eq!(document["asset"], stream_asset.to_string());
        }
        other => panic!("expected the delete's inverse to write its document back, got {other:?}"),
    }
}

#[test]
fn a_stream_document_whose_asset_cannot_be_read_is_still_one_stream_asset() {
    let fixture = ProjectBesideItsImportSources::new();
    write_stream_document(
        &fixture.project_directory,
        "streams/main.graph.json",
        &json!({"nodes": []}),
    );
    let repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let assets = repository.assets().unwrap();
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0].kind, ProjectAssetKind::Stream);
    assert_eq!(
        ProjectRepository::open(&fixture.project_directory)
            .unwrap()
            .assets()
            .unwrap()[0]
            .asset,
        assets[0].asset
    );
}

#[test]
fn an_assets_file_is_the_file_at_its_path_and_an_unknown_or_missing_asset_is_refused() {
    let fixture = ProjectBesideItsImportSources::new();
    let mut repository =
        ProjectRepository::create(&fixture.project_directory, "robot-cell", &agent()).unwrap();
    let lut = fixture.import_source("grade.cube", b"LUT_3D_SIZE 2");
    let applied = repository
        .apply_asset_changes(&agent(), None, vec![import_from(&lut, "looks/grade.cube")])
        .unwrap()
        .into_applied_transaction();
    let asset = imported_asset(&applied);

    let file = the_file_of_an_asset(&fixture.project_directory, asset).unwrap();
    assert_eq!(file, fixture.project_directory.join("looks/grade.cube"));
    assert_eq!(std::fs::read(&file).unwrap(), b"LUT_3D_SIZE 2");

    let unknown_asset = Uuid::new_v4();
    assert!(matches!(
        the_file_of_an_asset(&fixture.project_directory, unknown_asset),
        Err(ProjectRepositoryRefusal::NoSuchAsset(refused)) if refused == unknown_asset
    ));

    std::fs::remove_file(&file).unwrap();
    assert!(matches!(
        the_file_of_an_asset(&fixture.project_directory, asset),
        Err(ProjectRepositoryRefusal::AssetFileMissing { .. })
    ));
}
