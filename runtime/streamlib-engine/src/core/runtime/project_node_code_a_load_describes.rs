// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

/// The content hash of every file a load describes a project's node types from — its Python
/// sources, its dependency files and its views' manifests — so a reload tells whether any of
/// them changed since a stream's load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProjectNodeCodeContentHash(u64);

impl ProjectNodeCodeContentHash {
    /// The hash of the node code the project at `project_directory` holds on disk now.
    pub(super) fn of_the_project_at(project_directory: &Path) -> Self {
        let mut node_code_files_by_path = BTreeMap::new();
        read_every_node_code_file_under(
            project_directory,
            project_directory,
            &mut node_code_files_by_path,
        );
        let mut hasher = DefaultHasher::new();
        node_code_files_by_path.hash(&mut hasher);
        Self(hasher.finish())
    }
}

fn is_a_node_code_file(file_name: &str) -> bool {
    file_name.ends_with(".py")
        || file_name == "pyproject.toml"
        || file_name == "uv.lock"
        || file_name == "tatolab_view.json"
}

fn is_a_directory_holding_no_node_code(directory: &Path, directory_name: &str) -> bool {
    directory_name.starts_with('.')
        || directory_name == "__pycache__"
        || directory_name == "node_modules"
        || directory.join("pyvenv.cfg").exists()
}

fn read_every_node_code_file_under(
    project_directory: &Path,
    directory: &Path,
    node_code_files_by_path: &mut BTreeMap<PathBuf, Vec<u8>>,
) {
    let Ok(directory_entries) = std::fs::read_dir(directory) else {
        return;
    };
    for directory_entry in directory_entries.flatten() {
        let Ok(entry_file_type) = directory_entry.file_type() else {
            continue;
        };
        let entry_name = directory_entry.file_name().to_string_lossy().into_owned();
        let entry_path = directory_entry.path();
        if entry_file_type.is_dir() {
            if !is_a_directory_holding_no_node_code(&entry_path, &entry_name) {
                read_every_node_code_file_under(
                    project_directory,
                    &entry_path,
                    node_code_files_by_path,
                );
            }
        } else if is_a_node_code_file(&entry_name)
            && let Ok(contents) = std::fs::read(&entry_path)
        {
            let path_in_the_project = entry_path
                .strip_prefix(project_directory)
                .map_or_else(|_| entry_path.clone(), Path::to_path_buf);
            node_code_files_by_path.insert(path_in_the_project, contents);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_project_holding(files: &[(&str, &str)]) -> tempfile::TempDir {
        let project_directory = tempfile::tempdir().unwrap();
        for (path, contents) in files {
            let file_path = project_directory.path().join(path);
            std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
            std::fs::write(file_path, contents).unwrap();
        }
        project_directory
    }

    #[test]
    fn an_edit_of_a_node_file_or_a_dependency_file_changes_the_hash() {
        let project = a_project_holding(&[
            ("nodes/blur.py", "class Blur: pass\n"),
            ("pyproject.toml", "[project]\nname = \"p\"\n"),
        ]);
        let before = ProjectNodeCodeContentHash::of_the_project_at(project.path());
        std::fs::write(project.path().join("nodes/blur.py"), "class Blur: x = 1\n").unwrap();
        let after_the_node_edit = ProjectNodeCodeContentHash::of_the_project_at(project.path());
        std::fs::write(
            project.path().join("pyproject.toml"),
            "[project]\nname = \"q\"\n",
        )
        .unwrap();
        let after_the_dependency_edit =
            ProjectNodeCodeContentHash::of_the_project_at(project.path());

        assert_ne!(before, after_the_node_edit);
        assert_ne!(after_the_node_edit, after_the_dependency_edit);
    }

    #[test]
    fn a_graph_document_an_asset_or_a_venv_edit_leaves_the_hash_as_it_was() {
        let project = a_project_holding(&[
            ("nodes/blur.py", "class Blur: pass\n"),
            ("streams/main.graph.json", "{}"),
            (".venv/pyvenv.cfg", "home = /usr\n"),
            (".venv/lib/site.py", "x = 1\n"),
        ]);
        let before = ProjectNodeCodeContentHash::of_the_project_at(project.path());
        std::fs::write(
            project.path().join("streams/main.graph.json"),
            "{\"nodes\": []}",
        )
        .unwrap();
        std::fs::write(project.path().join(".venv/lib/site.py"), "x = 2\n").unwrap();
        std::fs::write(project.path().join("media.png"), "not node code").unwrap();

        assert_eq!(
            before,
            ProjectNodeCodeContentHash::of_the_project_at(project.path())
        );
    }

    #[test]
    fn a_new_node_file_changes_the_hash() {
        let project = a_project_holding(&[("nodes/blur.py", "class Blur: pass\n")]);
        let before = ProjectNodeCodeContentHash::of_the_project_at(project.path());
        std::fs::write(
            project.path().join("nodes/sharpen.py"),
            "class Sharpen: pass\n",
        )
        .unwrap();

        assert_ne!(
            before,
            ProjectNodeCodeContentHash::of_the_project_at(project.path())
        );
    }
}
