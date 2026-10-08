// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! JSONL log directory resolution — collocated in the install's
//! generated working tree — and the segment file names a runtime's log is
//! written under and read back by.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

/// Base directory for JSONL log files: `<STREAMLIB_HOME>/.streamlib/logs/`.
/// Collocated in the install's generated working tree
/// ([`get_streamlib_data_dir`]) so logs live in the same self-contained
/// folder as the rest of a runtime's state, and honor the `STREAMLIB_HOME`
/// override.
///
/// [`get_streamlib_data_dir`]: crate::streamlib_home::get_streamlib_data_dir
pub fn log_dir() -> PathBuf {
    crate::streamlib_home::get_streamlib_data_dir().join("logs")
}

/// The active segment's file name for one runtime instance:
/// `<runtime_id>-<started_at_millis>.jsonl`.
pub fn active_runtime_log_segment_file_name(
    runtime_id: &str,
    started_at_millis: impl std::fmt::Display,
) -> String {
    format!("{runtime_id}-{started_at_millis}.jsonl")
}

/// Path of the active JSONL segment for one runtime instance, named by
/// [`active_runtime_log_segment_file_name`].
pub fn runtime_log_path(runtime_id: &str, started_at_millis: u128) -> PathBuf {
    log_dir().join(active_runtime_log_segment_file_name(
        runtime_id,
        started_at_millis,
    ))
}

/// Path a rotated segment of `active_segment_path` is renamed to:
/// `<runtime_id>-<started_at_millis>.<rotation_sequence>.jsonl`.
///
/// The sequence is dot-separated because a pinned `STREAMLIB_RUNTIME_ID` may
/// carry dashes, and `camera-2-1700000000000.jsonl` would read two ways.
pub fn rotated_runtime_log_segment_path(
    active_segment_path: &Path,
    rotation_sequence: u64,
) -> PathBuf {
    active_segment_path.with_extension(format!("{rotation_sequence}.jsonl"))
}

/// The rotation sequence `candidate_file_name` carries when it names a rotated
/// segment of `active_segment_path` — the inverse of [`rotated_runtime_log_segment_path`].
pub fn rotated_runtime_log_segment_sequence(
    active_segment_path: &Path,
    candidate_file_name: &str,
) -> Option<u64> {
    let active_stem = active_segment_path.file_stem()?.to_str()?;
    let sequence_digits = candidate_file_name
        .strip_prefix(active_stem)?
        .strip_prefix('.')?
        .strip_suffix(".jsonl")?;
    if !is_ascii_digits(sequence_digits) {
        return None;
    }
    sequence_digits.parse().ok()
}

/// Path a rotation creates the next active segment at before renaming it into place.
pub fn replacement_runtime_log_segment_path(active_segment_path: &Path) -> PathBuf {
    active_segment_path.with_extension("jsonl.rotating")
}

/// The sequence of every rotated segment of `active_segment_path` on disk, oldest first.
pub fn rotated_runtime_log_segment_sequences_on_disk(
    active_segment_path: &Path,
) -> io::Result<Vec<u64>> {
    let directory = match active_segment_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let mut rotated_sequences = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let file_name = entry?.file_name();
        if let Some(rotated_sequence) = file_name
            .to_str()
            .and_then(|name| rotated_runtime_log_segment_sequence(active_segment_path, name))
        {
            rotated_sequences.push(rotated_sequence);
        }
    }
    rotated_sequences.sort_unstable();
    Ok(rotated_sequences)
}

/// What a JSONL log segment's file name says about it, whichever runtime wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLogSegmentFileName {
    /// The runtime's `RuntimeUniqueId`, which may carry dashes and dots.
    pub runtime_id: String,
    /// The instance's start in epoch milliseconds, spelled as the file name spells it.
    pub started_at_millis_digits: String,
    /// The rotation's sequence for a rotated segment; `None` for the active one.
    pub rotation_sequence: Option<u64>,
}

/// The segment `file_name` names — the inverse of [`runtime_log_path`] and
/// [`rotated_runtime_log_segment_path`] together — or `None` for any other file.
///
/// An active stem always ends `-<digits>`, so the text after its last dot is never all
/// digits and the two shapes cannot be confused. A sequence past `u64` is no rotation, as
/// [`rotated_runtime_log_segment_sequence`] reads it.
pub fn parse_runtime_log_segment_file_name(file_name: &str) -> Option<RuntimeLogSegmentFileName> {
    let segment_stem = file_name.strip_suffix(".jsonl")?;
    let (instance_stem, rotation_sequence) = match segment_stem.rsplit_once('.') {
        Some((before_sequence, sequence_digits)) if is_ascii_digits(sequence_digits) => {
            (before_sequence, Some(sequence_digits.parse().ok()?))
        }
        _ => (segment_stem, None),
    };
    let (runtime_id, started_at_millis_digits) = instance_stem.rsplit_once('-')?;
    if runtime_id.is_empty() || !is_ascii_digits(started_at_millis_digits) {
        return None;
    }
    Some(RuntimeLogSegmentFileName {
        runtime_id: runtime_id.to_owned(),
        started_at_millis_digits: started_at_millis_digits.to_owned(),
        rotation_sequence,
    })
}

fn is_ascii_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// One start of one runtime, as its log segments on disk record it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLogInstanceOnDisk {
    /// The runtime's `RuntimeUniqueId`.
    pub runtime_id: String,
    /// The instance's start in epoch milliseconds, spelled as its segments' names spell it.
    pub started_at_millis_digits: String,
    /// `<runtime_id>-<started_at_millis>.jsonl` beside the instance's segments, whether or not
    /// the active segment is on disk.
    pub active_segment_path: PathBuf,
    /// The bytes of every segment of the instance on disk, rotated ones included.
    pub total_segment_bytes: u64,
}

impl RuntimeLogInstanceOnDisk {
    /// Order by start time as a number rather than as text, so `999` started before `1000`.
    pub fn compare_started_at(&self, other: &Self) -> Ordering {
        numeric_ordering_key_of_digits(&self.started_at_millis_digits).cmp(
            &numeric_ordering_key_of_digits(&other.started_at_millis_digits),
        )
    }
}

/// A key under which ASCII digit strings of any length sort as the numbers they spell.
fn numeric_ordering_key_of_digits(digits: &str) -> (usize, &str) {
    let significant_digits = digits.trim_start_matches('0');
    (significant_digits.len(), significant_digits)
}

/// Every runtime instance with a segment in `log_directory`, ordered by runtime_id then start
/// text; none when the directory does not exist.
pub fn runtime_log_instances_in_directory(
    log_directory: &Path,
) -> io::Result<Vec<RuntimeLogInstanceOnDisk>> {
    runtime_log_instances_in_directory_whose_runtime_id(log_directory, |_| true)
}

/// The most recently started instance of `runtime_id` with a segment in `log_directory`; none
/// when the directory does not exist.
pub fn newest_runtime_log_instance_in_directory(
    log_directory: &Path,
    runtime_id: &str,
) -> io::Result<Option<RuntimeLogInstanceOnDisk>> {
    Ok(
        runtime_log_instances_in_directory_whose_runtime_id(log_directory, |segment_runtime_id| {
            segment_runtime_id == runtime_id
        })?
        .into_iter()
        .max_by(RuntimeLogInstanceOnDisk::compare_started_at),
    )
}

/// The instances [`runtime_log_instances_in_directory`] lists, of the runtimes
/// `admits_runtime_id` admits; a segment of any other runtime is never read for its size. A
/// segment gone between the listing and the read of its size is left out: a rotation renamed
/// it, or a cleanup removed it.
fn runtime_log_instances_in_directory_whose_runtime_id(
    log_directory: &Path,
    admits_runtime_id: impl Fn(&str) -> bool,
) -> io::Result<Vec<RuntimeLogInstanceOnDisk>> {
    let directory_entries = match std::fs::read_dir(log_directory) {
        Ok(directory_entries) => directory_entries,
        Err(listing_failure) if listing_failure.kind() == io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        }
        Err(listing_failure) => return Err(listing_failure),
    };
    let mut total_segment_bytes_by_instance: BTreeMap<(String, String), u64> = BTreeMap::new();
    for directory_entry in directory_entries {
        let directory_entry = directory_entry?;
        let Some(segment_file_name) = directory_entry
            .file_name()
            .to_str()
            .and_then(parse_runtime_log_segment_file_name)
        else {
            continue;
        };
        if !admits_runtime_id(&segment_file_name.runtime_id) {
            continue;
        }
        let segment_byte_len = match std::fs::metadata(directory_entry.path()) {
            Ok(segment_metadata) => segment_metadata.len(),
            Err(stat_failure) if stat_failure.kind() == io::ErrorKind::NotFound => continue,
            Err(stat_failure) => return Err(stat_failure),
        };
        *total_segment_bytes_by_instance
            .entry((
                segment_file_name.runtime_id,
                segment_file_name.started_at_millis_digits,
            ))
            .or_default() += segment_byte_len;
    }
    Ok(total_segment_bytes_by_instance
        .into_iter()
        .map(
            |((runtime_id, started_at_millis_digits), total_segment_bytes)| {
                RuntimeLogInstanceOnDisk {
                    active_segment_path: log_directory.join(active_runtime_log_segment_file_name(
                        &runtime_id,
                        &started_at_millis_digits,
                    )),
                    runtime_id,
                    started_at_millis_digits,
                    total_segment_bytes,
                }
            },
        )
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn log_dir_under_streamlib_home() {
        // SAFETY: test modifies env; `#[serial]` keeps it off the other
        // STREAMLIB_HOME-mutating tests.
        let prev = std::env::var_os("STREAMLIB_HOME");
        unsafe { std::env::set_var("STREAMLIB_HOME", "/tmp/slh-logging-test") };
        assert_eq!(
            log_dir(),
            PathBuf::from("/tmp/slh-logging-test/.streamlib/logs")
        );
        unsafe {
            match prev {
                Some(v) => std::env::set_var("STREAMLIB_HOME", v),
                None => std::env::remove_var("STREAMLIB_HOME"),
            }
        }
    }

    #[test]
    fn runtime_log_path_has_stable_shape() {
        let path = runtime_log_path("Rabc123", 1_700_000_000_000);
        let file_name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(file_name, "Rabc123-1700000000000.jsonl");
    }

    #[test]
    #[serial]
    fn concurrent_runtime_paths_do_not_collide() {
        let streamlib_home =
            crate::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
        let prev = std::env::var_os("STREAMLIB_HOME");
        // SAFETY: test modifies env; `#[serial]` keeps it off the other
        // STREAMLIB_HOME-mutating tests.
        unsafe { std::env::set_var("STREAMLIB_HOME", streamlib_home.path()) };

        let dir = log_dir();
        let p1 = runtime_log_path("RtestA", 111);
        let p2 = runtime_log_path("RtestB", 111);
        let p3 = runtime_log_path("RtestA", 222);

        assert_ne!(p1, p2);
        assert_ne!(p1, p3);
        assert!(p1.starts_with(&dir));
        assert!(p2.starts_with(&dir));
        assert!(p3.starts_with(&dir));

        unsafe {
            match prev {
                Some(v) => std::env::set_var("STREAMLIB_HOME", v),
                None => std::env::remove_var("STREAMLIB_HOME"),
            }
        }
    }

    /// The name a reader finds a runtime's rotated segments by, parsing it back
    /// with [`rotated_runtime_log_segment_sequence`].
    #[test]
    fn a_rotated_segment_is_named_with_a_dot_separated_sequence() {
        let active_segment_path = runtime_log_path("Rabc123", 1_700_000_000_000);
        let rotated = rotated_runtime_log_segment_path(&active_segment_path, 3);

        assert_eq!(rotated.parent(), active_segment_path.parent());
        assert_eq!(
            rotated.file_name().unwrap(),
            "Rabc123-1700000000000.3.jsonl"
        );
    }

    #[test]
    fn filename_shape_round_trips() {
        for (active_segment_path, rotation_sequence) in [
            (runtime_log_path("Rabc123", 1_700_000_000_000), 1),
            (runtime_log_path("Rabc123", 1_700_000_000_000), 3),
            (PathBuf::from("/logs/my.node-2-1700000000000.jsonl"), 12),
            (PathBuf::from("/logs/camera-2-1000.jsonl"), u64::MAX),
        ] {
            let rotated = rotated_runtime_log_segment_path(&active_segment_path, rotation_sequence);
            let rotated_file_name = rotated.file_name().unwrap().to_str().unwrap();

            assert_eq!(
                rotated_runtime_log_segment_sequence(&active_segment_path, rotated_file_name),
                Some(rotation_sequence),
                "{rotated_file_name} does not parse back to {rotation_sequence}"
            );
        }
    }

    #[test]
    fn a_file_that_is_not_one_of_this_segments_rotations_parses_to_no_sequence() {
        let active_segment_path = Path::new("/logs/Rabc-1000.jsonl");

        for foreign_file_name in [
            "Rabc-1000.jsonl",
            "Rabc-10000.7.jsonl",
            "Rabc-100.7.jsonl",
            "Rabc-1000.+5.jsonl",
            "Rabc-1000..jsonl",
            "Rabc-1000.5a.jsonl",
            "Rabc-1000.jsonl.rotating",
            "Rabc-1000.99999999999999999999999.jsonl",
        ] {
            assert_eq!(
                rotated_runtime_log_segment_sequence(active_segment_path, foreign_file_name),
                None,
                "{foreign_file_name} was read as a rotation of Rabc-1000.jsonl"
            );
        }
        assert_eq!(
            replacement_runtime_log_segment_path(active_segment_path),
            Path::new("/logs/Rabc-1000.jsonl.rotating")
        );
    }

    #[test]
    fn a_runtime_id_carrying_dots_and_dashes_keeps_its_whole_name_when_rotated() {
        let active_segment_path = Path::new("/logs/my.node-2-1700000000000.jsonl");

        assert_eq!(
            rotated_runtime_log_segment_path(active_segment_path, 12),
            Path::new("/logs/my.node-2-1700000000000.12.jsonl")
        );
    }

    fn segment_file_name(
        runtime_id: &str,
        started_at_millis_digits: &str,
        rotation_sequence: Option<u64>,
    ) -> Option<RuntimeLogSegmentFileName> {
        Some(RuntimeLogSegmentFileName {
            runtime_id: runtime_id.to_owned(),
            started_at_millis_digits: started_at_millis_digits.to_owned(),
            rotation_sequence,
        })
    }

    #[test]
    fn every_segment_name_the_writer_produces_parses_back_to_its_instance() {
        let active_segment_path = runtime_log_path("Rabc123", 1_700_000_000_000);
        let rotated_segment_path = rotated_runtime_log_segment_path(&active_segment_path, 3);

        assert_eq!(
            parse_runtime_log_segment_file_name(
                active_segment_path.file_name().unwrap().to_str().unwrap()
            ),
            segment_file_name("Rabc123", "1700000000000", None)
        );
        assert_eq!(
            parse_runtime_log_segment_file_name(
                rotated_segment_path.file_name().unwrap().to_str().unwrap()
            ),
            segment_file_name("Rabc123", "1700000000000", Some(3))
        );
    }

    #[test]
    fn a_runtime_id_carrying_dashes_and_dots_parses_whole_from_either_shape() {
        assert_eq!(
            parse_runtime_log_segment_file_name("R-with-dashes-1234.jsonl"),
            segment_file_name("R-with-dashes", "1234", None)
        );
        assert_eq!(
            parse_runtime_log_segment_file_name("camera-2-1000.2.jsonl"),
            segment_file_name("camera-2", "1000", Some(2))
        );
        assert_eq!(
            parse_runtime_log_segment_file_name("my.node-2000.4.jsonl"),
            segment_file_name("my.node", "2000", Some(4))
        );
        assert_eq!(
            parse_runtime_log_segment_file_name("x.5-1000.jsonl"),
            segment_file_name("x.5", "1000", None)
        );
    }

    #[test]
    fn a_file_that_is_no_segment_parses_to_nothing() {
        for foreign_file_name in [
            "Rabc-1000.jsonl.rotating",
            "Rabc-1000.json",
            "Rabc.jsonl",
            "-1000.jsonl",
            "Rabc-.jsonl",
            "Rabc-10a0.jsonl",
            "Rabc-1000..jsonl",
            "Rabc-1000.99999999999999999999999.jsonl",
            "Rabc-１０００.jsonl",
            "notes.txt",
        ] {
            assert_eq!(
                parse_runtime_log_segment_file_name(foreign_file_name),
                None,
                "{foreign_file_name} was read as a segment"
            );
        }
    }

    fn write_segment_of_bytes(directory: &Path, file_name: &str, byte_count: usize) {
        std::fs::write(directory.join(file_name), vec![b'x'; byte_count]).unwrap();
    }

    #[test]
    fn a_rotated_segment_is_listed_under_its_runtime_rather_than_as_a_runtime_of_its_own() {
        let log_directory = tempfile::tempdir().unwrap();
        write_segment_of_bytes(log_directory.path(), "Rabc123-1700000000000.3.jsonl", 7);

        assert_eq!(
            runtime_log_instances_in_directory(log_directory.path()).unwrap(),
            [RuntimeLogInstanceOnDisk {
                runtime_id: "Rabc123".to_owned(),
                started_at_millis_digits: "1700000000000".to_owned(),
                active_segment_path: log_directory.path().join("Rabc123-1700000000000.jsonl"),
                total_segment_bytes: 7,
            }]
        );
    }

    #[test]
    fn a_runtime_instance_is_listed_once_with_the_bytes_of_every_segment() {
        let log_directory = tempfile::tempdir().unwrap();
        write_segment_of_bytes(log_directory.path(), "camera-2-1000.jsonl", 10);
        write_segment_of_bytes(log_directory.path(), "camera-2-1000.1.jsonl", 200);
        write_segment_of_bytes(log_directory.path(), "camera-2-1000.2.jsonl", 3000);
        write_segment_of_bytes(log_directory.path(), "my.node-2000.4.jsonl", 5);
        write_segment_of_bytes(log_directory.path(), "camera-2-1000.jsonl.rotating", 9);
        write_segment_of_bytes(log_directory.path(), "README", 9);

        let listed: Vec<(String, String, u64)> =
            runtime_log_instances_in_directory(log_directory.path())
                .unwrap()
                .into_iter()
                .map(|instance| {
                    (
                        instance.runtime_id,
                        instance.started_at_millis_digits,
                        instance.total_segment_bytes,
                    )
                })
                .collect();

        assert_eq!(
            listed,
            [
                ("camera-2".to_owned(), "1000".to_owned(), 3210),
                ("my.node".to_owned(), "2000".to_owned(), 5),
            ]
        );
    }

    #[test]
    fn a_directory_that_is_missing_holds_no_runtime_instance() {
        let log_directory = tempfile::tempdir().unwrap();
        let absent_log_directory = log_directory.path().join("absent");

        assert_eq!(
            runtime_log_instances_in_directory(&absent_log_directory).unwrap(),
            []
        );
        assert_eq!(
            newest_runtime_log_instance_in_directory(&absent_log_directory, "Rabc").unwrap(),
            None
        );
    }

    /// Only a directory that is not there reads as empty; one that cannot be listed is the
    /// caller's to report.
    #[test]
    fn a_path_that_is_no_directory_is_an_error_rather_than_no_runtime_instance() {
        let log_directory = tempfile::tempdir().unwrap();
        let not_a_directory = log_directory.path().join("Rabc-1000.jsonl");
        write_segment_of_bytes(log_directory.path(), "Rabc-1000.jsonl", 1);

        assert!(runtime_log_instances_in_directory(&not_a_directory).is_err());
        assert!(newest_runtime_log_instance_in_directory(&not_a_directory, "Rabc").is_err());
    }

    /// A link to itself cannot be read for its size, and names another runtime's segment, so
    /// the lookup succeeds only when it never reads that segment.
    #[test]
    fn the_newest_instance_lookup_reads_no_other_runtimes_segment() {
        let log_directory = tempfile::tempdir().unwrap();
        write_segment_of_bytes(log_directory.path(), "Rabc-1000.jsonl", 3);
        std::os::unix::fs::symlink(
            "Rother-2000.jsonl",
            log_directory.path().join("Rother-2000.jsonl"),
        )
        .unwrap();

        assert!(runtime_log_instances_in_directory(log_directory.path()).is_err());
        assert_eq!(
            newest_runtime_log_instance_in_directory(log_directory.path(), "Rabc")
                .unwrap()
                .map(|instance| instance.total_segment_bytes),
            Some(3)
        );
    }

    /// A dangling link stands in for a segment a rotation renamed between the listing and the
    /// read of its size.
    #[test]
    fn a_segment_gone_before_its_size_is_read_is_left_out() {
        let log_directory = tempfile::tempdir().unwrap();
        write_segment_of_bytes(log_directory.path(), "Rabc-1000.jsonl", 3);
        std::os::unix::fs::symlink(
            "renamed-by-a-rotation",
            log_directory.path().join("Rabc-1000.1.jsonl"),
        )
        .unwrap();

        assert_eq!(
            runtime_log_instances_in_directory(log_directory.path())
                .unwrap()
                .into_iter()
                .map(|instance| instance.total_segment_bytes)
                .collect::<Vec<_>>(),
            [3]
        );
    }

    #[test]
    fn an_instance_names_its_active_segment_as_the_writer_does() {
        let log_directory = tempfile::tempdir().unwrap();
        write_segment_of_bytes(log_directory.path(), "my.node-2-1700000000000.4.jsonl", 1);

        assert_eq!(
            runtime_log_instances_in_directory(log_directory.path())
                .unwrap()
                .into_iter()
                .map(|instance| instance.active_segment_path)
                .collect::<Vec<_>>(),
            [log_directory
                .path()
                .join(active_runtime_log_segment_file_name(
                    "my.node-2",
                    1_700_000_000_000_u128
                ))]
        );
    }

    #[test]
    fn the_newest_instance_of_a_runtime_wins_by_start_time_as_a_number() {
        let log_directory = tempfile::tempdir().unwrap();
        write_segment_of_bytes(log_directory.path(), "Rabc-1000.jsonl", 1);
        write_segment_of_bytes(log_directory.path(), "Rabc-999.jsonl", 1);
        write_segment_of_bytes(log_directory.path(), "Rabc-2000.1.jsonl", 1);
        write_segment_of_bytes(log_directory.path(), "Rother-9000.jsonl", 1);

        assert_eq!(
            newest_runtime_log_instance_in_directory(log_directory.path(), "Rabc")
                .unwrap()
                .map(|instance| instance.active_segment_path),
            Some(log_directory.path().join("Rabc-2000.jsonl"))
        );
        assert_eq!(
            newest_runtime_log_instance_in_directory(log_directory.path(), "Rnone").unwrap(),
            None
        );
    }

    #[test]
    fn a_start_time_past_every_integer_type_still_orders_as_a_number() {
        let log_directory = tempfile::tempdir().unwrap();
        write_segment_of_bytes(
            log_directory.path(),
            "Rabc-99999999999999999999999999999999999999999.jsonl",
            1,
        );
        write_segment_of_bytes(log_directory.path(), "Rabc-1000.jsonl", 1);

        assert_eq!(
            newest_runtime_log_instance_in_directory(log_directory.path(), "Rabc")
                .unwrap()
                .map(|instance| instance.started_at_millis_digits),
            Some("99999999999999999999999999999999999999999".to_owned())
        );
    }

    #[test]
    fn the_rotated_segments_on_disk_are_this_instances_alone_oldest_first() {
        let log_directory = tempfile::tempdir().unwrap();
        let active_segment_path = log_directory.path().join("Rabc-1000.jsonl");
        write_segment_of_bytes(log_directory.path(), "Rabc-1000.jsonl", 1);
        write_segment_of_bytes(log_directory.path(), "Rabc-1000.10.jsonl", 1);
        write_segment_of_bytes(log_directory.path(), "Rabc-1000.9.jsonl", 1);
        write_segment_of_bytes(log_directory.path(), "Rabc-2000.1.jsonl", 1);
        write_segment_of_bytes(log_directory.path(), "Rabc-1000.jsonl.rotating", 1);

        assert_eq!(
            rotated_runtime_log_segment_sequences_on_disk(&active_segment_path).unwrap(),
            [9, 10]
        );
    }
}
