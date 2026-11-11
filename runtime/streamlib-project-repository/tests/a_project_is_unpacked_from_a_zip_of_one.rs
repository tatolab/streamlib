// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Map, json};
use streamlib_project_repository::{
    PROJECT_HISTORY_DIRECTORY, ProjectAssetAction, ProjectAssetImportSource, ProjectRepository,
    ProjectRepositoryRefusal, ProjectTransactionAuthor, ProjectTransactionAuthorKind,
};
use uuid::Uuid;

fn a_person() -> ProjectTransactionAuthor {
    ProjectTransactionAuthor {
        kind: ProjectTransactionAuthorKind::Person,
        name: "jo".to_owned(),
    }
}

/// A project named `robot-cell` holding one stream, one LUT asset, a `.venv` and its own
/// history, zipped whole into `robot-cell.zip` beside it.
struct ZippedProject {
    root: tempfile::TempDir,
    project: Uuid,
    stream: Uuid,
    lut: Uuid,
}

impl ZippedProject {
    fn zipped_under(top_folder: Option<&str>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let project_directory = root.path().join("robot-cell");
        let mut repository =
            ProjectRepository::create(&project_directory, "robot-cell", &a_person()).unwrap();
        let stream = Uuid::new_v4();
        std::fs::create_dir_all(project_directory.join("streams")).unwrap();
        std::fs::write(
            project_directory.join("streams/main.graph.json"),
            json!({ "asset": stream, "nodes": [], "links": [] }).to_string(),
        )
        .unwrap();
        let lut_source = root.path().join("grade.cube");
        std::fs::write(&lut_source, b"LUT_3D_SIZE 2\n").unwrap();
        repository
            .apply_asset_changes(
                &a_person(),
                None,
                vec![ProjectAssetAction::ImportAsset {
                    source: ProjectAssetImportSource::FileOnThisMachine(lut_source),
                    asset_path: Some("luts/grade.cube".to_owned()),
                    file_name: None,
                    kind: None,
                    import_settings: Map::new(),
                }],
            )
            .unwrap();
        let lut = repository
            .assets()
            .unwrap()
            .into_iter()
            .find(|asset| asset.path.as_str() == "luts/grade.cube")
            .unwrap()
            .asset;
        std::fs::create_dir_all(project_directory.join(".venv/lib")).unwrap();
        std::fs::write(project_directory.join(".venv/pyvenv.cfg"), "home = /usr\n").unwrap();
        std::fs::write(project_directory.join(".venv/lib/site.py"), "").unwrap();
        let zipped = Self {
            project: repository.project_file().project,
            root,
            stream,
            lut,
        };
        zip_the_folder(&project_directory, &zipped.archive(), top_folder);
        zipped
    }

    fn archive(&self) -> PathBuf {
        self.root.path().join("robot-cell.zip")
    }

    fn source(&self) -> String {
        self.archive().display().to_string()
    }

    fn a_new_project_directory(&self, name: &str) -> PathBuf {
        self.root.path().join("imported").join(name)
    }
}

/// `directory` zipped into `archive`, every path under `top_folder/` when given, as a GitHub
/// archive wraps a repository.
fn zip_the_folder(directory: &Path, archive: &Path, top_folder: Option<&str>) {
    let mut writer = zip::ZipWriter::new(std::fs::File::create(archive).unwrap());
    let mut directories = vec![directory.to_path_buf()];
    while let Some(reading) = directories.pop() {
        for entry in std::fs::read_dir(&reading).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                directories.push(entry.path());
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(directory)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let name = match top_folder {
                Some(top_folder) => format!("{top_folder}/{relative}"),
                None => relative,
            };
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer
                .write_all(&std::fs::read(entry.path()).unwrap())
                .unwrap();
        }
    }
    writer.finish().unwrap();
}

fn the_entries_of(directory: &Path) -> Vec<String> {
    let mut entries: Vec<String> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    entries
}

#[test]
fn a_zipped_project_unpacks_as_a_project_of_its_own_with_its_streams_and_assets() {
    for top_folder in [None, Some("robot-cell-main")] {
        let zipped = ZippedProject::zipped_under(top_folder);
        let project_directory = zipped.a_new_project_directory("cell");

        let imported = ProjectRepository::import_from_a_project_archive(
            &project_directory,
            &zipped.source(),
            &a_person(),
        )
        .unwrap();

        assert_ne!(imported.project_file().project, zipped.project);
        assert_eq!(imported.project_file().name, "robot-cell");
        let reopened = ProjectRepository::open(&project_directory).unwrap();
        assert_eq!(reopened.project_file(), imported.project_file());
        assert_eq!(
            reopened.asset(zipped.lut).unwrap().path.as_str(),
            "luts/grade.cube"
        );
        assert!(reopened.stream_graph_document(zipped.stream).is_ok());
        assert!(!project_directory.join(".venv").exists());
        assert_eq!(
            the_entries_of(&project_directory.join(".tatolab")),
            vec![PROJECT_HISTORY_DIRECTORY.trim_start_matches(".tatolab/")]
        );
    }
}

#[test]
fn the_unpacked_project_starts_a_history_of_its_own_with_one_first_commit() {
    let zipped = ZippedProject::zipped_under(None);
    let project_directory = zipped.a_new_project_directory("cell");

    let imported = ProjectRepository::import_from_a_project_archive(
        &project_directory,
        &zipped.source(),
        &a_person(),
    )
    .unwrap();

    let first_commit = imported.history().head_commit().unwrap();
    assert!(first_commit.is_some());
    assert!(
        imported
            .applied_transactions_newest_first(10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn the_same_zip_unpacked_twice_makes_two_projects_with_their_own_uuids() {
    let zipped = ZippedProject::zipped_under(None);

    let first = ProjectRepository::import_from_a_project_archive(
        &zipped.a_new_project_directory("first"),
        &zipped.source(),
        &a_person(),
    )
    .unwrap();
    let second = ProjectRepository::import_from_a_project_archive(
        &zipped.a_new_project_directory("second"),
        &zipped.source(),
        &a_person(),
    )
    .unwrap();

    assert_ne!(first.project_file().project, second.project_file().project);
}

#[test]
fn a_zip_holding_no_project_is_refused_with_nothing_left_behind() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("photos.zip");
    let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
    writer
        .start_file("holiday.jpg", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(b"not a project").unwrap();
    writer.finish().unwrap();
    let parent_directory = root.path().join("imported");
    std::fs::create_dir_all(&parent_directory).unwrap();

    let refusal = ProjectRepository::import_from_a_project_archive(
        &parent_directory.join("photos"),
        &archive.display().to_string(),
        &a_person(),
    )
    .err()
    .expect("the import is refused");

    assert!(
        matches!(
            refusal,
            ProjectRepositoryRefusal::ImportSourceHoldsNoProject { .. }
        ),
        "{refusal}"
    );
    assert!(
        refusal.to_string().contains("tatolab-project.json"),
        "{refusal}"
    );
    assert!(the_entries_of(&parent_directory).is_empty());
}

#[test]
fn a_file_that_is_no_zip_is_refused_with_nothing_left_behind() {
    let root = tempfile::tempdir().unwrap();
    let not_a_zip = root.path().join("notes.zip");
    std::fs::write(&not_a_zip, "plain text").unwrap();
    let parent_directory = root.path().join("imported");
    std::fs::create_dir_all(&parent_directory).unwrap();

    let refusal = ProjectRepository::import_from_a_project_archive(
        &parent_directory.join("notes"),
        &not_a_zip.display().to_string(),
        &a_person(),
    )
    .err()
    .expect("the import is refused");

    assert!(
        refusal.to_string().contains("no folder and no zip archive"),
        "{refusal}"
    );
    assert!(the_entries_of(&parent_directory).is_empty());
}

#[test]
fn a_project_directory_that_already_exists_is_refused_and_left_as_it_was() {
    let zipped = ZippedProject::zipped_under(None);
    let project_directory = zipped.a_new_project_directory("cell");
    std::fs::create_dir_all(&project_directory).unwrap();
    std::fs::write(project_directory.join("notes.txt"), "mine").unwrap();

    let refusal = ProjectRepository::import_from_a_project_archive(
        &project_directory,
        &zipped.source(),
        &a_person(),
    )
    .err()
    .expect("the import is refused");

    assert!(
        matches!(
            refusal,
            ProjectRepositoryRefusal::ProjectDirectoryAlreadyExists { .. }
        ),
        "{refusal}"
    );
    assert_eq!(the_entries_of(&project_directory), vec!["notes.txt"]);
    assert_eq!(
        the_entries_of(project_directory.parent().unwrap()),
        vec!["cell"]
    );
}

#[test]
fn a_project_folder_is_refused_as_the_source() {
    let zipped = ZippedProject::zipped_under(None);

    let refusal = ProjectRepository::import_from_a_project_archive(
        &zipped.a_new_project_directory("cell"),
        &zipped.root.path().join("robot-cell").display().to_string(),
        &a_person(),
    )
    .err()
    .expect("the import is refused");

    assert!(
        matches!(
            refusal,
            ProjectRepositoryRefusal::ProjectArchiveIsAFolder { .. }
        ),
        "{refusal}"
    );
}

#[test]
fn a_relative_project_directory_or_source_is_refused() {
    let zipped = ZippedProject::zipped_under(None);

    let relative_directory = ProjectRepository::import_from_a_project_archive(
        Path::new("imported/cell"),
        &zipped.source(),
        &a_person(),
    )
    .err()
    .expect("the import is refused");
    let relative_source = ProjectRepository::import_from_a_project_archive(
        &zipped.a_new_project_directory("cell"),
        "robot-cell.zip",
        &a_person(),
    )
    .err()
    .expect("the import is refused");

    assert!(matches!(
        relative_directory,
        ProjectRepositoryRefusal::ProjectDirectoryNotAbsolute(_)
    ));
    assert!(matches!(
        relative_source,
        ProjectRepositoryRefusal::ImportSourceNotAnAbsolutePath { .. }
    ));
}
