// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! A folder moved to the user's home Trash as the freedesktop.org Trash specification lays it
//! out, so a file manager lists it and can put it back.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Move `folder` into the user's home Trash, `$XDG_DATA_HOME/Trash`, or
/// `~/.local/share/Trash` when that is unset.
pub(crate) fn move_the_folder_to_the_trash(folder: &Path) -> Result<(), String> {
    let data_home = match std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        Some(data_home) => PathBuf::from(data_home),
        None => match std::env::var_os("HOME").filter(|value| !value.is_empty()) {
            Some(home) => PathBuf::from(home).join(".local/share"),
            None => {
                return Err(
                    "neither XDG_DATA_HOME nor HOME is set, so there is no Trash".to_owned(),
                );
            }
        },
    };
    move_the_folder_into_the_trash_at(folder, &data_home.join("Trash"), &the_local_time_now())
}

/// Move `folder` into the Trash at `trash_directory`, its `.trashinfo` recording it deleted at
/// `deletion_date`, `YYYY-MM-DDThh:mm:ss` in local time as the specification asks.
pub(crate) fn move_the_folder_into_the_trash_at(
    folder: &Path,
    trash_directory: &Path,
    deletion_date: &str,
) -> Result<(), String> {
    let folder_name = folder
        .file_name()
        .ok_or_else(|| format!("{} names no folder", folder.display()))?
        .to_string_lossy()
        .into_owned();
    let files_directory = trash_directory.join("files");
    let info_directory = trash_directory.join("info");
    for directory in [&files_directory, &info_directory] {
        std::fs::create_dir_all(directory).map_err(|not_made| {
            format!("the Trash {} was not made: {not_made}", directory.display())
        })?;
    }
    for attempt in 1..=u32::MAX {
        let trashed_name = if attempt == 1 {
            folder_name.clone()
        } else {
            format!("{folder_name}.{attempt}")
        };
        let info_path = info_directory.join(format!("{trashed_name}.trashinfo"));
        let trashed_path = files_directory.join(&trashed_name);
        if trashed_path.symlink_metadata().is_ok() {
            continue;
        }
        let mut info_file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&info_path)
        {
            Ok(info_file) => info_file,
            Err(taken) if taken.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(not_written) => {
                return Err(format!(
                    "{} was not written: {not_written}",
                    info_path.display()
                ));
            }
        };
        let written = info_file.write_all(
            format!(
                "[Trash Info]\nPath={}\nDeletionDate={deletion_date}\n",
                percent_encoded_path(folder)
            )
            .as_bytes(),
        );
        let moved = written
            .map_err(|not_written| {
                format!("{} was not written: {not_written}", info_path.display())
            })
            .and_then(|()| {
                std::fs::rename(folder, &trashed_path).map_err(|not_moved| {
                    if not_moved.raw_os_error() == Some(libc::EXDEV) {
                        format!(
                            "{} is on another disk than the Trash in {}",
                            folder.display(),
                            trash_directory.display()
                        )
                    } else {
                        not_moved.to_string()
                    }
                })
            });
        if moved.is_err() {
            let _ = std::fs::remove_file(&info_path);
        }
        return moved;
    }
    Err(format!(
        "the Trash {} has no free name left",
        trash_directory.display()
    ))
}

/// `path` as a `Path=` key holds it: every byte but an unreserved one or `/` percent-encoded.
fn percent_encoded_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut encoded = String::new();
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn the_local_time_now() -> String {
    let mut now: libc::time_t = 0;
    let mut local = std::mem::MaybeUninit::<libc::tm>::zeroed();
    // SAFETY: `time` writes the current time through a valid pointer, and `localtime_r` fills
    // the caller-owned `tm` it is handed, returning null only on failure, checked below.
    let local = unsafe {
        libc::time(&mut now);
        if libc::localtime_r(&now, local.as_mut_ptr()).is_null() {
            return "1970-01-01T00:00:00".to_owned();
        }
        local.assume_init()
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        local.tm_year + 1900,
        local.tm_mon + 1,
        local.tm_mday,
        local.tm_hour,
        local.tm_min,
        local.tm_sec
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_lands_in_the_trash_with_its_info_and_a_second_one_of_its_name_beside_it() {
        let root = tempfile::tempdir().unwrap();
        let trash = root.path().join("Trash");
        for round in ["first", "second"] {
            let folder = root.path().join("robot cell");
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("tatolab-project.json"), round).unwrap();
            move_the_folder_into_the_trash_at(&folder, &trash, "2026-11-10T01:02:03").unwrap();
            assert!(!folder.exists());
        }
        assert_eq!(
            std::fs::read_to_string(trash.join("files/robot cell/tatolab-project.json")).unwrap(),
            "first"
        );
        assert_eq!(
            std::fs::read_to_string(trash.join("files/robot cell.2/tatolab-project.json")).unwrap(),
            "second"
        );
        let info = std::fs::read_to_string(trash.join("info/robot cell.trashinfo")).unwrap();
        let expected_path = percent_encoded_path(&root.path().join("robot cell"));
        assert_eq!(
            info,
            format!("[Trash Info]\nPath={expected_path}\nDeletionDate=2026-11-10T01:02:03\n")
        );
        assert!(expected_path.ends_with("/robot%20cell"));
    }

    #[test]
    fn a_folder_that_does_not_move_leaves_no_info_behind() {
        let root = tempfile::tempdir().unwrap();
        let trash = root.path().join("Trash");
        let missing = root.path().join("gone");
        assert!(
            move_the_folder_into_the_trash_at(&missing, &trash, "2026-11-10T01:02:03").is_err()
        );
        assert_eq!(std::fs::read_dir(trash.join("info")).unwrap().count(), 0);
    }

    #[test]
    fn the_local_time_is_spelled_as_the_specification_asks() {
        let now = the_local_time_now();
        assert_eq!(now.len(), "2026-11-10T01:02:03".len());
        assert_eq!(&now[10..11], "T");
    }
}
