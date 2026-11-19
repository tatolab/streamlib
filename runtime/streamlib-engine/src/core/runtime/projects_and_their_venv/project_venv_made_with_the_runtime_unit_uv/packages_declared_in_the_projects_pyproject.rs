// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Declaring the packages the runtime unit carries in a project: each first-party module, the core nodes project and the carried wheels, linked into the project and named in its `pyproject.toml`.

use std::path::{Path, PathBuf};

use streamlib_project_repository::{
    FirstPartyModuleDeclaredByAProject, PROJECT_CARRIED_WHEELS_LINK,
    PROJECT_FIRST_PARTY_MODULE_LINKS_DIRECTORY, ProjectRepositoryRefusal, TatolabProjectFile,
    every_import_a_project_resolves, link_the_core_project_into,
    link_the_core_project_into_each_import_of,
    pyproject_with_each_first_party_module_as_a_uv_path_source,
    pyproject_with_each_import_as_a_uv_path_source,
    pyproject_with_the_carried_wheels_as_a_uv_find_links_source,
};

use super::project_venv_sync_stamp::the_distribution_name_of_the_module_wheel;
use super::{ProjectVenvNotMade, remove_what_stands_at};
use streamlib_runtime_client_contract::process_umask_kept_from_iceoryx2_listener_binds::create_directory_and_its_missing_parents_at_the_process_umask;

/// Link each wheel of `first_party_module_wheels` into the project's
/// [`PROJECT_FIRST_PARTY_MODULE_LINKS_DIRECTORY`], take out each link to a module
/// the runtime no longer carries, and declare each module in the project's
/// `pyproject.toml` as a uv path source through its link. Nothing is written
/// when the runtime carries no first-party module or the project holds no
/// `pyproject.toml`.
pub(super) fn declare_each_first_party_module_in_the_project(
    project_directory: &Path,
    first_party_module_wheels: &[PathBuf],
) -> Result<(), ProjectVenvNotMade> {
    let pyproject_path = project_directory.join("pyproject.toml");
    if first_party_module_wheels.is_empty() || !pyproject_path.is_file() {
        return Ok(());
    }
    let not_declared_because = |why: String| ProjectVenvNotMade::Failed {
        failed_because: format!(
            "the first-party modules the runtime carries could not be declared in {}: {why}",
            pyproject_path.display()
        ),
    };
    let links_directory = project_directory.join(PROJECT_FIRST_PARTY_MODULE_LINKS_DIRECTORY);
    create_directory_and_its_missing_parents_at_the_process_umask(&links_directory).map_err(
        |not_made| {
            not_declared_because(format!(
                "{} was not made: {not_made}",
                links_directory.display()
            ))
        },
    )?;
    let declared_modules: Vec<FirstPartyModuleDeclaredByAProject> = first_party_module_wheels
        .iter()
        .filter_map(|module_wheel| {
            Some(FirstPartyModuleDeclaredByAProject {
                distribution_name: the_distribution_name_of_the_module_wheel(module_wheel)?,
                link_file_name: module_wheel.file_name()?.to_str()?.to_owned(),
            })
        })
        .collect();
    for module_wheel in first_party_module_wheels {
        let Some(link_file_name) = module_wheel.file_name() else {
            continue;
        };
        let link = links_directory.join(link_file_name);
        if std::fs::read_link(&link).is_ok_and(|target| target == *module_wheel) {
            continue;
        }
        remove_what_stands_at(&link)?;
        std::os::unix::fs::symlink(module_wheel, &link).map_err(|not_linked| {
            not_declared_because(format!(
                "{} was not linked to {}: {not_linked}",
                link.display(),
                module_wheel.display()
            ))
        })?;
    }
    let links_the_runtime_no_longer_carries: Vec<PathBuf> = std::fs::read_dir(&links_directory)
        .map_err(|not_read| {
            not_declared_because(format!(
                "{} was not read: {not_read}",
                links_directory.display()
            ))
        })?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            !declared_modules
                .iter()
                .any(|module| entry.file_name() == module.link_file_name.as_str())
        })
        .map(|entry| entry.path())
        .collect();
    for link in links_the_runtime_no_longer_carries {
        remove_what_stands_at(&link)?;
    }
    let pyproject_as_it_was = std::fs::read_to_string(&pyproject_path)
        .map_err(|not_read| not_declared_because(format!("it was not read: {not_read}")))?;
    let pyproject = pyproject_with_each_first_party_module_as_a_uv_path_source(
        &pyproject_as_it_was,
        project_directory,
        &declared_modules,
    )
    .map_err(|refusal| not_declared_because(refusal.to_string()))?;
    if pyproject != pyproject_as_it_was {
        std::fs::write(&pyproject_path, pyproject).map_err(|not_written| {
            not_declared_because(format!("it was not written: {not_written}"))
        })?;
    }
    for import_folder in each_import_folder_below(project_directory) {
        link_each_first_party_module_the_import_names(&import_folder, first_party_module_wheels)?;
    }
    Ok(())
}

/// Link the Core Nodes project the runtime unit carries into the project as its import, and
/// declare it in the project's `pyproject.toml` as a uv path source through that link, so any
/// `uv sync` keeps its requirements. Nothing is linked when the unit carries no Core Nodes
/// project, and the Core Nodes project opened as a project itself takes no link.
pub(super) fn declare_the_core_nodes_project_in_the_project(
    project_directory: &Path,
    core_nodes_project_directory: Option<&Path>,
) -> Result<(), ProjectVenvNotMade> {
    let Some(core_nodes_project_directory) = core_nodes_project_directory else {
        return Ok(());
    };
    let pyproject_path = project_directory.join("pyproject.toml");
    let not_declared_because = |why: String| ProjectVenvNotMade::Failed {
        failed_because: format!(
            "the Core Nodes project the runtime carries at {} could not be declared in {}: {why}",
            core_nodes_project_directory.display(),
            project_directory.display()
        ),
    };
    let project_file = match TatolabProjectFile::read_from_project_directory(project_directory) {
        Ok(project_file) => project_file,
        Err(ProjectRepositoryRefusal::NotAProject { .. }) => return Ok(()),
        Err(refusal) => return Err(not_declared_because(refusal.to_string())),
    };
    link_the_core_project_into(project_directory, core_nodes_project_directory)
        .map_err(|refusal| not_declared_because(refusal.to_string()))?;
    link_the_core_project_into_each_import_of(project_directory, core_nodes_project_directory)
        .map_err(|refusal| not_declared_because(refusal.to_string()))?;
    let pyproject_as_it_was = match std::fs::read_to_string(&pyproject_path) {
        Ok(pyproject_as_it_was) => pyproject_as_it_was,
        Err(not_read) if not_read.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(not_read) => {
            return Err(not_declared_because(format!(
                "{} was not read: {not_read}",
                pyproject_path.display()
            )));
        }
    };
    let pyproject = pyproject_with_each_import_as_a_uv_path_source(
        &pyproject_as_it_was,
        project_directory,
        &every_import_a_project_resolves(project_directory, &project_file),
    )
    .map_err(|refusal| not_declared_because(refusal.to_string()))?;
    if pyproject != pyproject_as_it_was {
        std::fs::write(&pyproject_path, pyproject).map_err(|not_written| {
            not_declared_because(format!(
                "{} was not written: {not_written}",
                pyproject_path.display()
            ))
        })?;
    }
    Ok(())
}

/// Every folder of the project's `imports/`, and of each import's own `imports/` below it, in
/// name order: uv follows each one's path sources from the project's own.
fn each_import_folder_below(project_directory: &Path) -> Vec<PathBuf> {
    let mut import_folders = Vec::new();
    let mut imports_directories_to_read =
        vec![project_directory.join(streamlib_project_repository::PROJECT_IMPORTS_DIRECTORY)];
    while let Some(imports_directory) = imports_directories_to_read.pop() {
        let Ok(entries) = std::fs::read_dir(&imports_directory) else {
            continue;
        };
        for entry in entries.filter_map(|entry| entry.ok()) {
            if entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
                imports_directories_to_read.push(
                    entry
                        .path()
                        .join(streamlib_project_repository::PROJECT_IMPORTS_DIRECTORY),
                );
                import_folders.push(entry.path());
            }
        }
    }
    import_folders.sort();
    import_folders
}

/// Link each first-party module the import's `pyproject.toml` names through its own
/// [`PROJECT_FIRST_PARTY_MODULE_LINKS_DIRECTORY`] to the wheel of that distribution the runtime
/// carries, under the file name the import names, and take out every other link there. A copy
/// never carries a project's `.tatolab/`, and uv reads an import's path sources from its own
/// folder, so without these links uv finds none of the wheels the import names. The import's
/// own files are never written. A module the runtime does not carry is left for uv to refuse.
fn link_each_first_party_module_the_import_names(
    import_folder: &Path,
    first_party_module_wheels: &[PathBuf],
) -> Result<(), ProjectVenvNotMade> {
    let Ok(import_pyproject) = std::fs::read_to_string(import_folder.join("pyproject.toml")) else {
        return Ok(());
    };
    let Ok(links_the_import_names) =
        streamlib_project_repository::first_party_module_links_a_pyproject_names(
            &import_pyproject,
            import_folder,
        )
    else {
        return Ok(());
    };
    let not_linked_because = |why: String| ProjectVenvNotMade::Failed {
        failed_because: format!(
            "the first-party modules the import in {} names could not be linked: {why}",
            import_folder.display()
        ),
    };
    let links_directory = import_folder.join(PROJECT_FIRST_PARTY_MODULE_LINKS_DIRECTORY);
    let mut link_file_names_kept = Vec::new();
    for named_link in &links_the_import_names {
        let Some(carried_wheel) = first_party_module_wheels.iter().find(|module_wheel| {
            the_distribution_name_of_the_module_wheel(module_wheel).is_some_and(|carried| {
                streamlib_project_repository::distribution_names_are_one(
                    &carried,
                    &named_link.distribution_name,
                )
            })
        }) else {
            continue;
        };
        link_file_names_kept.push(named_link.link_file_name.as_str());
        let link = links_directory.join(&named_link.link_file_name);
        if std::fs::read_link(&link).is_ok_and(|target| target == *carried_wheel) {
            continue;
        }
        create_directory_and_its_missing_parents_at_the_process_umask(&links_directory).map_err(
            |not_made| {
                not_linked_because(format!(
                    "{} was not made: {not_made}",
                    links_directory.display()
                ))
            },
        )?;
        remove_what_stands_at(&link)?;
        std::os::unix::fs::symlink(carried_wheel, &link).map_err(|not_linked| {
            not_linked_because(format!(
                "{} was not linked to {}: {not_linked}",
                link.display(),
                carried_wheel.display()
            ))
        })?;
    }
    let Ok(entries) = std::fs::read_dir(&links_directory) else {
        return Ok(());
    };
    let links_no_longer_named: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            !link_file_names_kept
                .iter()
                .any(|kept| entry.file_name() == *kept)
        })
        .map(|entry| entry.path())
        .collect();
    for link in links_no_longer_named {
        remove_what_stands_at(&link)?;
    }
    Ok(())
}

/// Link the project's [`PROJECT_CARRIED_WHEELS_LINK`] to `carried_wheels_directory` and name
/// it in the project's `pyproject.toml` as a uv find-links source, or take both out when the
/// unit carries no wheels. Nothing is written when the project holds no `pyproject.toml`.
pub(super) fn declare_the_carried_wheels_in_the_project(
    project_directory: &Path,
    carried_wheels_directory: Option<&Path>,
) -> Result<(), ProjectVenvNotMade> {
    let pyproject_path = project_directory.join("pyproject.toml");
    if !pyproject_path.is_file() {
        return Ok(());
    }
    let not_declared_because = |why: String| ProjectVenvNotMade::Failed {
        failed_because: format!(
            "the wheels the runtime carries could not be declared in {}: {why}",
            pyproject_path.display()
        ),
    };
    let link = project_directory.join(PROJECT_CARRIED_WHEELS_LINK);
    match carried_wheels_directory {
        Some(carried_wheels_directory) => {
            if !std::fs::read_link(&link).is_ok_and(|target| target == carried_wheels_directory) {
                remove_what_stands_at(&link)?;
                if let Some(links_parent) = link.parent() {
                    create_directory_and_its_missing_parents_at_the_process_umask(links_parent)
                        .map_err(|not_made| {
                            not_declared_because(format!(
                                "{} was not made: {not_made}",
                                links_parent.display()
                            ))
                        })?;
                }
                std::os::unix::fs::symlink(carried_wheels_directory, &link).map_err(
                    |not_linked| {
                        not_declared_because(format!(
                            "{} was not linked to {}: {not_linked}",
                            link.display(),
                            carried_wheels_directory.display()
                        ))
                    },
                )?;
            }
        }
        None => remove_what_stands_at(&link)?,
    }
    let pyproject_as_it_was = std::fs::read_to_string(&pyproject_path)
        .map_err(|not_read| not_declared_because(format!("it was not read: {not_read}")))?;
    let pyproject = pyproject_with_the_carried_wheels_as_a_uv_find_links_source(
        &pyproject_as_it_was,
        project_directory,
        carried_wheels_directory.is_some(),
    )
    .map_err(|refusal| not_declared_because(refusal.to_string()))?;
    if pyproject != pyproject_as_it_was {
        std::fs::write(&pyproject_path, pyproject).map_err(|not_written| {
            not_declared_because(format!("it was not written: {not_written}"))
        })?;
    }
    Ok(())
}
