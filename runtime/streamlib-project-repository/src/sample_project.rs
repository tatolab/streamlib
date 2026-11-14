// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The sample projects a runtime unit carries, each a project directory in its samples
//! directory, listed by name, title and summary, and copied whole into a new project.

use std::path::{Path, PathBuf};

use serde_json::Value;
use uuid::Uuid;

use crate::project_file::{PROJECT_FILE_NAME, THIS_RUNTIME_VERSION, TatolabProjectFile};
use crate::project_repository_refusal::ProjectRepositoryRefusal;
use crate::project_stream_document::STREAM_DOCUMENT_ASSET_KEY;

/// Where a sample's one stream asset that a new project runs first sits in it.
pub const SAMPLE_PROJECT_MAIN_STREAM_DOCUMENT_PATH: &str = "streams/main.graph.json";

/// Folders a sample's copy never carries, beside every hidden folder.
const FOLDERS_A_SAMPLE_COPY_NEVER_CARRIES: [&str; 1] = ["__pycache__"];

/// One sample project a runtime unit carries, as a listing shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleProjectListed {
    /// Its folder's name in the samples directory, which a create names it by.
    pub name: String,
    /// The name its project file gives it.
    pub title: String,
    /// Its project file's description; empty when it gives none.
    pub summary: String,
}

/// One sample project a runtime unit carries, read from its folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleProjectTheRuntimeUnitCarries {
    /// Its folder in the samples directory.
    pub directory: PathBuf,
    /// Its name, title and summary.
    pub listed: SampleProjectListed,
    /// Its project file as the sample writes it.
    pub project_file: TatolabProjectFile,
    /// The UUID of its `streams/main.graph.json` stream asset.
    pub main_stream_asset: Uuid,
}

/// Every sample project in `samples_directory`, in name order; none when it is not there. A
/// folder that does not read as a sample is left out of the listing, with why logged.
pub fn every_sample_project_in(
    samples_directory: &Path,
) -> Vec<SampleProjectTheRuntimeUnitCarries> {
    let Ok(entries) = std::fs::read_dir(samples_directory) else {
        return Vec::new();
    };
    let mut sample_folders: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_dir()))
        .map(|entry| entry.path())
        .collect();
    sample_folders.sort();
    sample_folders
        .into_iter()
        .filter_map(
            |sample_folder| match read_a_sample_project_folder(&sample_folder) {
                Ok(sample) => Some(sample),
                Err(not_a_sample) => {
                    tracing::warn!(
                        "{} is left out of the samples: {not_a_sample}",
                        sample_folder.display()
                    );
                    None
                }
            },
        )
        .collect()
}

/// The sample `sample_name` in `samples_directory`; refused, naming every sample it holds,
/// when it holds none by that name.
pub fn the_sample_project_named(
    samples_directory: Option<&Path>,
    sample_name: &str,
) -> Result<SampleProjectTheRuntimeUnitCarries, ProjectRepositoryRefusal> {
    let carried = samples_directory
        .map(every_sample_project_in)
        .unwrap_or_default();
    if let Some(sample) = carried
        .iter()
        .find(|sample| sample.listed.name == sample_name)
    {
        return Ok(sample.clone());
    }
    let carried_names: Vec<&str> = carried
        .iter()
        .map(|sample| sample.listed.name.as_str())
        .collect();
    Err(ProjectRepositoryRefusal::NoSuchSampleProject {
        sample: sample_name.to_owned(),
        carried: if carried_names.is_empty() {
            "none".to_owned()
        } else {
            carried_names.join(", ")
        },
    })
}

fn read_a_sample_project_folder(
    sample_folder: &Path,
) -> Result<SampleProjectTheRuntimeUnitCarries, ProjectRepositoryRefusal> {
    let project_file = TatolabProjectFile::read_from_project_directory(sample_folder)?;
    let main_stream_document_path = sample_folder.join(SAMPLE_PROJECT_MAIN_STREAM_DOCUMENT_PATH);
    let main_stream_document: Value =
        serde_json::from_slice(&std::fs::read(&main_stream_document_path)?)?;
    let main_stream_asset = main_stream_document
        .get(STREAM_DOCUMENT_ASSET_KEY)
        .and_then(Value::as_str)
        .and_then(|written| Uuid::parse_str(written).ok())
        .ok_or_else(
            || ProjectRepositoryRefusal::SampleProjectMainStreamHoldsNoAsset {
                path: main_stream_document_path.clone(),
            },
        )?;
    Ok(SampleProjectTheRuntimeUnitCarries {
        directory: sample_folder.to_path_buf(),
        listed: SampleProjectListed {
            name: sample_folder
                .file_name()
                .map(|folder_name| folder_name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            title: project_file.name.clone(),
            summary: project_file.description.clone().unwrap_or_default(),
        },
        project_file,
        main_stream_asset,
    })
}

impl SampleProjectTheRuntimeUnitCarries {
    /// Copy every file of the sample into `project_directory`, made when missing, as the project
    /// `project` named `project_name`, created with this runtime. Refused, with nothing written,
    /// when `project_directory` already holds any file the copy would write.
    pub fn copy_into(
        &self,
        project_directory: &Path,
        project: Uuid,
        project_name: &str,
    ) -> Result<(), ProjectRepositoryRefusal> {
        let files_the_copy_writes = every_file_a_sample_copy_carries(&self.directory)?;
        let mut already_present: Vec<&str> = files_the_copy_writes
            .iter()
            .map(String::as_str)
            .filter(|relative| project_directory.join(relative).exists())
            .collect();
        already_present.sort_unstable();
        if !already_present.is_empty() {
            return Err(ProjectRepositoryRefusal::SampleProjectCopyWouldOverwrite {
                project_directory: project_directory.to_path_buf(),
                already_present: already_present.join(", "),
            });
        }
        for relative in &files_the_copy_writes {
            let copied_to = project_directory.join(relative);
            if let Some(parent) = copied_to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(self.directory.join(relative), &copied_to)?;
        }
        let project_file = TatolabProjectFile {
            project,
            name: project_name.to_owned(),
            created_with_runtime: Some(THIS_RUNTIME_VERSION.to_owned()),
            ..self.project_file.clone()
        };
        std::fs::write(
            project_directory.join(PROJECT_FILE_NAME),
            project_file.canonical_bytes()?,
        )?;
        Ok(())
    }
}

/// Every regular file under `sample_folder`, relative and sorted, outside a hidden folder and
/// a Python bytecode cache.
fn every_file_a_sample_copy_carries(
    sample_folder: &Path,
) -> Result<Vec<String>, ProjectRepositoryRefusal> {
    let mut found = Vec::new();
    let mut directories_to_read = vec![(sample_folder.to_path_buf(), String::new())];
    while let Some((reading, relative_directory)) = directories_to_read.pop() {
        for entry in std::fs::read_dir(&reading)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if relative_directory.is_empty() {
                name.clone()
            } else {
                format!("{relative_directory}/{name}")
            };
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                if !name.starts_with('.')
                    && !FOLDERS_A_SAMPLE_COPY_NEVER_CARRIES.contains(&name.as_str())
                {
                    directories_to_read.push((entry.path(), relative));
                }
            } else if file_type.is_file() {
                found.push(relative);
            }
        }
    }
    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN_STREAM_ASSET: &str = "0eaf9ee2-0f73-4154-a3f6-a99c261b850d";

    fn a_sample_in(samples_directory: &Path, name: &str, description: Option<&str>) -> PathBuf {
        let sample_folder = samples_directory.join(name);
        std::fs::create_dir_all(sample_folder.join("streams")).unwrap();
        std::fs::create_dir_all(sample_folder.join("nodes/__pycache__")).unwrap();
        std::fs::create_dir_all(sample_folder.join(".venv")).unwrap();
        let mut project_file = TatolabProjectFile::of_a_new_project(&format!("{name} title"));
        project_file.description = description.map(str::to_owned);
        std::fs::write(
            sample_folder.join(PROJECT_FILE_NAME),
            project_file.canonical_bytes().unwrap(),
        )
        .unwrap();
        std::fs::write(
            sample_folder.join(SAMPLE_PROJECT_MAIN_STREAM_DOCUMENT_PATH),
            format!("{{\"asset\": \"{MAIN_STREAM_ASSET}\", \"nodes\": [], \"links\": []}}"),
        )
        .unwrap();
        std::fs::write(sample_folder.join("nodes/lens.py"), "LENS = 1\n").unwrap();
        std::fs::write(sample_folder.join("nodes/__pycache__/lens.pyc"), "bytecode").unwrap();
        std::fs::write(sample_folder.join(".venv/pyvenv.cfg"), "home = /").unwrap();
        std::fs::write(sample_folder.join(".python-version"), "3.12\n").unwrap();
        sample_folder
    }

    #[test]
    fn the_samples_are_listed_in_name_order_by_title_and_summary() {
        let samples_directory = tempfile::tempdir().unwrap();
        a_sample_in(
            samples_directory.path(),
            "lens",
            Some("A lens over the pattern."),
        );
        a_sample_in(samples_directory.path(), "chord", None);
        std::fs::create_dir_all(samples_directory.path().join("not-a-project")).unwrap();

        let listed: Vec<SampleProjectListed> = every_sample_project_in(samples_directory.path())
            .into_iter()
            .map(|sample| sample.listed)
            .collect();

        assert_eq!(
            listed,
            [
                SampleProjectListed {
                    name: "chord".to_owned(),
                    title: "chord title".to_owned(),
                    summary: String::new(),
                },
                SampleProjectListed {
                    name: "lens".to_owned(),
                    title: "lens title".to_owned(),
                    summary: "A lens over the pattern.".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn no_samples_directory_lists_none() {
        assert!(every_sample_project_in(Path::new("/nowhere/samples")).is_empty());
    }

    #[test]
    fn a_sample_the_unit_does_not_carry_is_refused_naming_the_ones_it_does() {
        let samples_directory = tempfile::tempdir().unwrap();
        a_sample_in(samples_directory.path(), "lens", None);
        a_sample_in(samples_directory.path(), "chord", None);

        let refusal = the_sample_project_named(Some(samples_directory.path()), "cube")
            .unwrap_err()
            .to_string();

        assert_eq!(
            refusal,
            "the runtime unit carries no sample `cube`; it carries chord, lens"
        );
        assert_eq!(
            the_sample_project_named(None, "cube")
                .unwrap_err()
                .to_string(),
            "the runtime unit carries no sample `cube`; it carries none"
        );
    }

    #[test]
    fn a_copy_carries_every_file_but_caches_and_hidden_folders_under_the_new_project() {
        let samples_directory = tempfile::tempdir().unwrap();
        a_sample_in(samples_directory.path(), "lens", Some("A lens."));
        let sample = the_sample_project_named(Some(samples_directory.path()), "lens").unwrap();
        assert_eq!(sample.main_stream_asset.to_string(), MAIN_STREAM_ASSET);
        let project_directory = tempfile::tempdir().unwrap();
        let project = Uuid::new_v4();

        sample
            .copy_into(project_directory.path(), project, "Bright Lens")
            .unwrap();

        assert_eq!(
            every_file_a_sample_copy_carries(project_directory.path()).unwrap(),
            [
                ".python-version",
                "nodes/lens.py",
                "streams/main.graph.json",
                "tatolab-project.json"
            ]
        );
        assert!(!project_directory.path().join(".venv").exists());
        let project_file =
            TatolabProjectFile::read_from_project_directory(project_directory.path()).unwrap();
        assert_eq!(project_file.project, project);
        assert_eq!(project_file.name, "Bright Lens");
        assert_eq!(project_file.description.as_deref(), Some("A lens."));
        assert_eq!(
            project_file.created_with_runtime.as_deref(),
            Some(THIS_RUNTIME_VERSION)
        );
    }

    #[test]
    fn a_copy_over_a_file_it_would_write_is_refused_with_nothing_written() {
        let samples_directory = tempfile::tempdir().unwrap();
        a_sample_in(samples_directory.path(), "lens", None);
        let sample = the_sample_project_named(Some(samples_directory.path()), "lens").unwrap();
        let project_directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project_directory.path().join("nodes")).unwrap();
        std::fs::write(project_directory.path().join("nodes/lens.py"), "# mine\n").unwrap();

        let refusal = sample
            .copy_into(project_directory.path(), Uuid::new_v4(), "lens")
            .unwrap_err()
            .to_string();

        assert!(refusal.contains("already has nodes/lens.py"), "{refusal}");
        assert!(!project_directory.path().join(PROJECT_FILE_NAME).exists());
        assert_eq!(
            std::fs::read_to_string(project_directory.path().join("nodes/lens.py")).unwrap(),
            "# mine\n"
        );
    }
}
