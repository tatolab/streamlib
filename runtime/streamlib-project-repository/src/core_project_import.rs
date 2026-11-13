// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The core project the runtime unit carries, as an import of every project: linked at
//! `imports/core-nodes`, never copied, and never recorded in the project file or its history.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::project_asset::PROJECT_IMPORTS_DIRECTORY;
use crate::project_file::{ProjectImport, THIS_RUNTIME_VERSION, TatolabProjectFile};
use crate::project_import_folders::is_one_folder_name;
use crate::project_repository_refusal::ProjectRepositoryRefusal;

/// The folder under `imports/` every project holds the core project in. No import a person
/// adds takes it.
pub const CORE_PROJECT_IMPORT_FOLDER: &str = "core-nodes";

/// Where the link to the core project sits in the project at `project_directory`.
pub fn the_core_project_link_in(project_directory: &Path) -> PathBuf {
    project_directory
        .join(PROJECT_IMPORTS_DIRECTORY)
        .join(CORE_PROJECT_IMPORT_FOLDER)
}

/// The core project as an import of the project at `project_directory`, when its link is there
/// and what it points at reads as a project. Its `source` is the directory the link points at,
/// its `version` this runtime's, and it lists no copied file, since nothing was copied.
pub fn the_core_project_linked_into(project_directory: &Path) -> Option<ProjectImport> {
    let link = the_core_project_link_in(project_directory);
    let core_project_directory = std::fs::read_link(&link).ok()?;
    let core_project_file = TatolabProjectFile::read_from_project_directory(&link).ok()?;
    Some(ProjectImport {
        project: core_project_file.project,
        folder: CORE_PROJECT_IMPORT_FOLDER.to_owned(),
        source: core_project_directory.to_string_lossy().into_owned(),
        version: THIS_RUNTIME_VERSION.to_owned(),
        copied_files: BTreeMap::new(),
    })
}

/// Every import the project at `project_directory` resolves: each one `project_file` records,
/// then the core project when it is linked in.
pub fn every_import_a_project_resolves(
    project_directory: &Path,
    project_file: &TatolabProjectFile,
) -> Vec<ProjectImport> {
    let mut imports = project_file.dependencies.clone();
    imports.extend(the_core_project_linked_into(project_directory));
    imports
}

/// Link the core project at `core_project_directory` into the project at `project_directory`,
/// answering whether a link stands there now. The core project opened as a project itself takes
/// no link, and a link standing at another directory is pointed at this one. A folder or file a
/// person put at the link's path is refused by name and left as it is. The link names the core
/// project by its absolute path, since a relative one would resolve from `imports/`.
pub fn link_the_core_project_into(
    project_directory: &Path,
    core_project_directory: &Path,
) -> Result<bool, ProjectRepositoryRefusal> {
    let core_project_directory = &std::path::absolute(core_project_directory)?;
    let core_project = TatolabProjectFile::read_from_project_directory(core_project_directory)?;
    let project_file = TatolabProjectFile::read_from_project_directory(project_directory)?;
    let link = the_core_project_link_in(project_directory);
    let what_stands_at_the_link = std::fs::symlink_metadata(&link).ok();
    let a_link_stands_there = what_stands_at_the_link
        .as_ref()
        .is_some_and(|metadata| metadata.file_type().is_symlink());
    if what_stands_at_the_link.is_some() && !a_link_stands_there {
        return Err(ProjectRepositoryRefusal::CoreProjectImportFolderTaken { path: link });
    }
    if project_file.project == core_project.project {
        if a_link_stands_there {
            std::fs::remove_file(&link)?;
        }
        return Ok(false);
    }
    if std::fs::read_link(&link).is_ok_and(|target| &target == core_project_directory) {
        return Ok(true);
    }
    if a_link_stands_there {
        std::fs::remove_file(&link)?;
    }
    if let Some(imports_directory) = link.parent() {
        std::fs::create_dir_all(imports_directory)?;
    }
    std::os::unix::fs::symlink(core_project_directory, &link)?;
    Ok(true)
}

/// Link the core project at `core_project_directory` into each import of the project at
/// `project_directory`, and each import nested in one, since an import's own `pyproject.toml`
/// names the core project at its own `imports/core-nodes`. Every link stands at the one
/// directory, so the project's environment still holds one core project.
pub fn link_the_core_project_into_each_import_of(
    project_directory: &Path,
    core_project_directory: &Path,
) -> Result<(), ProjectRepositoryRefusal> {
    let mut importing_directories = vec![project_directory.to_path_buf()];
    let mut directories_read = BTreeSet::new();
    while let Some(importing_directory) = importing_directories.pop() {
        let Ok(directory_on_disk) = importing_directory.canonicalize() else {
            continue;
        };
        if !directories_read.insert(directory_on_disk) {
            continue;
        }
        let Ok(importing_project) =
            TatolabProjectFile::read_from_project_directory(&importing_directory)
        else {
            continue;
        };
        for import in importing_project
            .dependencies
            .iter()
            .filter(|import| is_one_folder_name(&import.folder))
        {
            let import_directory = importing_directory
                .join(PROJECT_IMPORTS_DIRECTORY)
                .join(&import.folder);
            if TatolabProjectFile::read_from_project_directory(&import_directory).is_err() {
                continue;
            }
            link_the_core_project_into(&import_directory, core_project_directory)?;
            importing_directories.push(import_directory);
        }
    }
    Ok(())
}

/// Whether `relative_path`, a path inside a project with `/` between its folders, is the
/// folder the runtime links the core project at, in the project or in an import at any depth,
/// or a path inside it. No copy of a project carries it: the runtime that opens the copy links
/// its own.
pub(crate) fn is_at_or_inside_a_core_project_import_folder(relative_path: &str) -> bool {
    let mut folders = relative_path.split('/').peekable();
    while let Some(folder) = folders.next() {
        if folder == PROJECT_IMPORTS_DIRECTORY
            && folders.peek() == Some(&CORE_PROJECT_IMPORT_FOLDER)
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_file::PROJECT_FILE_NAME;

    fn a_project_named(directory: &Path, name: &str) -> TatolabProjectFile {
        std::fs::create_dir_all(directory).unwrap();
        let project_file = TatolabProjectFile::of_a_new_project(name);
        std::fs::write(
            directory.join(PROJECT_FILE_NAME),
            project_file.canonical_bytes().unwrap(),
        )
        .unwrap();
        project_file
    }

    #[test]
    fn a_project_resolves_the_core_project_after_the_imports_it_records() {
        let root = tempfile::tempdir().unwrap();
        let core = a_project_named(&root.path().join("unit/core-nodes"), "Core Nodes");
        let project_directory = root.path().join("show");
        let project_file = a_project_named(&project_directory, "show");

        assert!(every_import_a_project_resolves(&project_directory, &project_file).is_empty());
        assert!(
            link_the_core_project_into(&project_directory, &root.path().join("unit/core-nodes"))
                .unwrap()
        );

        let resolved = every_import_a_project_resolves(&project_directory, &project_file);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].project, core.project);
        assert_eq!(resolved[0].folder, CORE_PROJECT_IMPORT_FOLDER);
        assert!(resolved[0].copied_files.is_empty());
        assert_eq!(
            TatolabProjectFile::read_from_project_directory(&project_directory).unwrap(),
            project_file,
            "the project file never records the core project"
        );
    }

    #[test]
    fn a_link_to_another_runtimes_copy_is_pointed_at_this_one() {
        let root = tempfile::tempdir().unwrap();
        a_project_named(&root.path().join("old-unit/core-nodes"), "Core Nodes");
        a_project_named(&root.path().join("new-unit/core-nodes"), "Core Nodes");
        let project_directory = root.path().join("show");
        a_project_named(&project_directory, "show");

        link_the_core_project_into(&project_directory, &root.path().join("old-unit/core-nodes"))
            .unwrap();
        link_the_core_project_into(&project_directory, &root.path().join("new-unit/core-nodes"))
            .unwrap();

        assert_eq!(
            std::fs::read_link(the_core_project_link_in(&project_directory)).unwrap(),
            root.path().join("new-unit/core-nodes")
        );
    }

    #[test]
    fn the_core_project_opened_as_a_project_does_not_import_itself() {
        let root = tempfile::tempdir().unwrap();
        let unit_copy = root.path().join("unit/core-nodes");
        let core = a_project_named(&unit_copy, "Core Nodes");
        let opened = root.path().join("packages/tatolab-core-nodes");
        std::fs::create_dir_all(&opened).unwrap();
        std::fs::write(
            opened.join(PROJECT_FILE_NAME),
            core.canonical_bytes().unwrap(),
        )
        .unwrap();

        assert!(!link_the_core_project_into(&opened, &unit_copy).unwrap());
        assert!(the_core_project_linked_into(&opened).is_none());
    }

    #[test]
    fn each_import_and_each_import_nested_in_one_is_linked_to_the_one_core_project() {
        let root = tempfile::tempdir().unwrap();
        let unit_copy = root.path().join("unit/core-nodes");
        a_project_named(&unit_copy, "Core Nodes");
        let project_directory = root.path().join("show");
        let mut show = a_project_named(&project_directory, "show");
        let looks_directory = project_directory.join("imports/looks");
        let mut looks = a_project_named(&looks_directory, "looks");
        let grades_directory = looks_directory.join("imports/grades");
        let grades = a_project_named(&grades_directory, "grades");
        let an_import_of = |project: &TatolabProjectFile, folder: &str| ProjectImport {
            project: project.project,
            folder: folder.to_owned(),
            source: String::new(),
            version: String::new(),
            copied_files: BTreeMap::new(),
        };
        looks.dependencies.push(an_import_of(&grades, "grades"));
        std::fs::write(
            looks_directory.join(PROJECT_FILE_NAME),
            looks.canonical_bytes().unwrap(),
        )
        .unwrap();
        show.dependencies.push(an_import_of(&looks, "looks"));
        std::fs::write(
            project_directory.join(PROJECT_FILE_NAME),
            show.canonical_bytes().unwrap(),
        )
        .unwrap();

        link_the_core_project_into_each_import_of(&project_directory, &unit_copy).unwrap();

        for import_directory in [&looks_directory, &grades_directory] {
            assert_eq!(
                std::fs::read_link(the_core_project_link_in(import_directory)).unwrap(),
                unit_copy,
                "{}",
                import_directory.display()
            );
        }
        assert!(
            std::fs::symlink_metadata(the_core_project_link_in(&project_directory)).is_err(),
            "the project's own link is made by `link_the_core_project_into`"
        );
    }

    #[test]
    fn the_core_project_folder_is_named_in_a_project_and_in_an_import_at_any_depth() {
        for (relative_path, is_the_core_project_folder) in [
            ("imports/core-nodes", true),
            ("imports/core-nodes/core_nodes/color.py", true),
            ("imports/looks/imports/core-nodes/pyproject.toml", true),
            ("imports/looks/core-nodes/notes.txt", false),
            ("imports/core-nodes-of-mine/color.py", false),
            ("core-nodes/imports/color.py", false),
        ] {
            assert_eq!(
                is_at_or_inside_a_core_project_import_folder(relative_path),
                is_the_core_project_folder,
                "{relative_path}"
            );
        }
    }

    #[test]
    fn a_folder_a_person_put_at_the_links_path_is_refused_and_left() {
        let root = tempfile::tempdir().unwrap();
        let unit_copy = root.path().join("unit/core-nodes");
        a_project_named(&unit_copy, "Core Nodes");
        let project_directory = root.path().join("show");
        a_project_named(&project_directory, "show");
        let taken = the_core_project_link_in(&project_directory);
        std::fs::create_dir_all(&taken).unwrap();
        std::fs::write(taken.join("kept.txt"), b"mine").unwrap();

        let refusal = link_the_core_project_into(&project_directory, &unit_copy).unwrap_err();

        assert!(refusal.to_string().contains("core-nodes"), "{refusal}");
        assert_eq!(std::fs::read(taken.join("kept.txt")).unwrap(), b"mine");
    }
}
