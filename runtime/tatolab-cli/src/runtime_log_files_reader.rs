// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The on-disk side of `tatolab logs`: one runtime instance's JSONL segments, rotated ones oldest
//! first and then the active one, each record rendered as the runtime mirrored it — and, when
//! following, carried on across every rotation and into a restart's newer instance.

use std::collections::VecDeque;
use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::IgnoredAny;
use streamlib_runtime_client_contract::runtime_log_event::{LogLevel, RuntimeLogEvent, Source};
use streamlib_runtime_client_contract::runtime_log_event_pretty_rendering::format_event_pretty;
use streamlib_runtime_client_contract::runtime_log_file_paths::{
    RuntimeLogInstanceOnDisk, newest_runtime_log_instance_in_directory,
    rotated_runtime_log_segment_path, rotated_runtime_log_segment_sequences_on_disk,
};

/// How long a follow waits at the live edge before looking for appended records again.
pub(crate) const RUNTIME_LOG_FOLLOW_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The `--processor` / `--pipeline` / `--rhi` / `--level` / `--source` / `--intercepted-only`
/// narrowing applied to each record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RuntimeLogRecordFilters {
    /// Only records from this processor id.
    pub(crate) processor_id: Option<String>,
    /// Only records from this pipeline id.
    pub(crate) pipeline_id: Option<String>,
    /// Only records carrying an `rhi_op`.
    pub(crate) rhi_operations_only: bool,
    /// Only records at this severity or above.
    pub(crate) minimum_level: Option<LogLevel>,
    /// Only records emitted by this runtime language.
    pub(crate) source: Option<Source>,
    /// Only records an interceptor captured.
    pub(crate) intercepted_only: bool,
}

impl RuntimeLogRecordFilters {
    /// Whether `event` survives every filter that is set.
    pub(crate) fn admits(&self, event: &RuntimeLogEvent) -> bool {
        if let Some(processor_id) = &self.processor_id
            && event.processor_id.as_ref() != Some(processor_id)
        {
            return false;
        }
        if let Some(pipeline_id) = &self.pipeline_id
            && event.pipeline_id.as_ref() != Some(pipeline_id)
        {
            return false;
        }
        if self.rhi_operations_only && event.rhi_op.is_none() {
            return false;
        }
        if let Some(minimum_level) = self.minimum_level
            && event.level < minimum_level
        {
            return false;
        }
        if let Some(source) = self.source
            && event.source != source
        {
            return false;
        }
        if self.intercepted_only && !event.intercepted {
            return false;
        }
        true
    }
}

/// A runtime log segment that could not be opened or read, for a reason other than being gone.
#[derive(Debug)]
pub(crate) struct RuntimeLogSegmentReadFailure {
    /// The segment, or the active name, the read failed on.
    pub(crate) segment_path: PathBuf,
    /// What the operating system answered.
    pub(crate) read_failure: io::Error,
}

impl RuntimeLogSegmentReadFailure {
    fn of_segment(segment_path: &Path, read_failure: io::Error) -> Self {
        Self {
            segment_path: segment_path.to_path_buf(),
            read_failure,
        }
    }
}

impl fmt::Display for RuntimeLogSegmentReadFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "cannot read the runtime log segment {}: {}",
            self.segment_path.display(),
            self.read_failure
        )
    }
}

impl std::error::Error for RuntimeLogSegmentReadFailure {}

/// One JSONL line as a record, or `None` — with a warning on `warning_output` — when it is not
/// one. A truncated or foreign line is skipped rather than ending the read.
fn decode_runtime_log_line(line: &[u8], warning_output: &mut dyn Write) -> Option<RuntimeLogEvent> {
    let line_text = String::from_utf8_lossy(line);
    let trimmed_line = line_text.trim();
    if trimmed_line.is_empty() {
        return None;
    }
    if let Ok(event) = serde_json::from_str::<RuntimeLogEvent>(trimmed_line) {
        return Some(event);
    }
    // serde_json reports a record whose enum field holds a number as a syntax error, so whether
    // the line is JSON at all is asked of it on its own.
    let _ = match serde_json::from_str::<IgnoredAny>(trimmed_line) {
        Err(json_syntax_failure) => writeln!(
            warning_output,
            "warning: skipping malformed JSONL line: {json_syntax_failure}"
        ),
        Ok(IgnoredAny) if !trimmed_line.starts_with('{') => writeln!(
            warning_output,
            "warning: skipping JSONL line that is not a record object"
        ),
        Ok(IgnoredAny) => writeln!(
            warning_output,
            "warning: skipping JSONL line whose fields do not match the record schema"
        ),
    };
    None
}

/// Whole lines from one open segment, holding back a line still being written.
///
/// A batch lands in the file across more than one write, so a reader at the live edge can see
/// the front of a record before its newline; decoding that front would report a healthy record
/// as malformed and lose it.
struct RuntimeLogSegmentLineReader {
    segment_path: PathBuf,
    segment_reader: BufReader<File>,
    unfinished_line: Vec<u8>,
}

impl RuntimeLogSegmentLineReader {
    fn reading(segment_path: &Path, segment_file: File) -> Self {
        Self {
            segment_path: segment_path.to_path_buf(),
            segment_reader: BufReader::new(segment_file),
            unfinished_line: Vec::new(),
        }
    }

    /// The next line the segment holds whole right now, its newline included.
    fn next_complete_line(&mut self) -> Result<Option<Vec<u8>>, RuntimeLogSegmentReadFailure> {
        self.segment_reader
            .read_until(b'\n', &mut self.unfinished_line)
            .map_err(|read_failure| {
                RuntimeLogSegmentReadFailure::of_segment(&self.segment_path, read_failure)
            })?;
        if self.unfinished_line.ends_with(b"\n") {
            return Ok(Some(std::mem::take(&mut self.unfinished_line)));
        }
        Ok(None)
    }

    /// The line left without its newline, handed over once the segment is done.
    fn take_unfinished_line(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.unfinished_line)
    }

    fn held_segment_file(&self) -> &File {
        self.segment_reader.get_ref()
    }
}

/// What one step of reading a runtime instance's segments came to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RuntimeLogInstanceReadStep {
    /// One line, as the segment holds it.
    Line(Vec<u8>),
    /// A follow has read everything on disk and nothing new has landed yet.
    LiveEdgeReached,
    /// Every segment is read and the read does not follow.
    InstanceFinished,
}

/// Where a read of the active segment stands; the rotated segments listed beside it are read
/// before any of it.
enum ActiveSegmentReadState {
    /// The next step lists the rotated segments and opens the active one.
    ListRotatedSegmentsAndOpenTheActiveOne,
    /// The active name named no file at the open.
    ActiveSegmentAbsent,
    /// The active segment, held open and read up to its live edge.
    ActiveSegmentOpen(RuntimeLogSegmentLineReader),
    /// The active name now names another file: the held one's last lines come before the
    /// segments rotated after it.
    DrainingTheSegmentRotatedAway(RuntimeLogSegmentLineReader),
    /// A read that does not follow hands over the held segment's unfinished line and ends.
    HandingOverTheLastUnfinishedLine(RuntimeLogSegmentLineReader),
    /// Every segment is read.
    Finished,
}

/// Every line of one runtime instance's log, oldest segment first.
///
/// Rotation moves the active segment to a numbered name and puts a new file under the active
/// name, which is checked against the held file at every live edge: once it names a different
/// file, the held file is drained, the rotated segments newer than it are read, and the new
/// active segment is opened.
pub(crate) struct RuntimeLogInstanceSegmentsReader {
    active_segment_path: PathBuf,
    follow_appended_records: bool,
    rotation_sequences_listed_at_the_open: VecDeque<u64>,
    rotated_segment_being_read: Option<RuntimeLogSegmentLineReader>,
    last_read_rotation_sequence: u64,
    active_segment_read_state: ActiveSegmentReadState,
}

impl RuntimeLogInstanceSegmentsReader {
    /// A read of the instance whose active segment is `active_segment_path`, from its oldest
    /// rotated segment on.
    pub(crate) fn reading(active_segment_path: PathBuf, follow_appended_records: bool) -> Self {
        Self {
            active_segment_path,
            follow_appended_records,
            rotation_sequences_listed_at_the_open: VecDeque::new(),
            rotated_segment_being_read: None,
            last_read_rotation_sequence: 0,
            active_segment_read_state:
                ActiveSegmentReadState::ListRotatedSegmentsAndOpenTheActiveOne,
        }
    }

    /// The next line, live edge, or end; a rotated segment retention removed before it was read
    /// is skipped with a note on `note_output`.
    pub(crate) fn next_step(
        &mut self,
        note_output: &mut dyn Write,
    ) -> Result<RuntimeLogInstanceReadStep, RuntimeLogSegmentReadFailure> {
        loop {
            if let Some(rotated_segment_line_reader) = &mut self.rotated_segment_being_read {
                if let Some(line) = rotated_segment_line_reader.next_complete_line()? {
                    return Ok(RuntimeLogInstanceReadStep::Line(line));
                }
                let unfinished_line = rotated_segment_line_reader.take_unfinished_line();
                self.rotated_segment_being_read = None;
                if !unfinished_line.is_empty() {
                    return Ok(RuntimeLogInstanceReadStep::Line(unfinished_line));
                }
                continue;
            }
            if let Some(rotation_sequence) = self.rotation_sequences_listed_at_the_open.pop_front()
            {
                if rotation_sequence > self.last_read_rotation_sequence {
                    self.rotated_segment_being_read = open_rotated_segment(
                        &self.active_segment_path,
                        rotation_sequence,
                        note_output,
                    )?;
                    self.last_read_rotation_sequence = rotation_sequence;
                }
                continue;
            }
            match std::mem::replace(
                &mut self.active_segment_read_state,
                ActiveSegmentReadState::Finished,
            ) {
                ActiveSegmentReadState::ListRotatedSegmentsAndOpenTheActiveOne => {
                    let (opened_active_segment_file, rotation_sequences_listed_at_the_open) =
                        open_active_segment_after_listing(&self.active_segment_path)?;
                    self.rotation_sequences_listed_at_the_open =
                        rotation_sequences_listed_at_the_open.into();
                    self.active_segment_read_state = match opened_active_segment_file {
                        Some(active_segment_file) => ActiveSegmentReadState::ActiveSegmentOpen(
                            RuntimeLogSegmentLineReader::reading(
                                &self.active_segment_path,
                                active_segment_file,
                            ),
                        ),
                        None => ActiveSegmentReadState::ActiveSegmentAbsent,
                    };
                }
                ActiveSegmentReadState::ActiveSegmentAbsent => {
                    if !self.follow_appended_records {
                        return Ok(RuntimeLogInstanceReadStep::InstanceFinished);
                    }
                    self.active_segment_read_state =
                        ActiveSegmentReadState::ListRotatedSegmentsAndOpenTheActiveOne;
                    return Ok(RuntimeLogInstanceReadStep::LiveEdgeReached);
                }
                ActiveSegmentReadState::ActiveSegmentOpen(mut active_segment_line_reader) => {
                    if let Some(line) = active_segment_line_reader.next_complete_line()? {
                        self.active_segment_read_state =
                            ActiveSegmentReadState::ActiveSegmentOpen(active_segment_line_reader);
                        return Ok(RuntimeLogInstanceReadStep::Line(line));
                    }
                    if held_segment_was_rotated_away(
                        active_segment_line_reader.held_segment_file(),
                        &self.active_segment_path,
                    )? {
                        self.active_segment_read_state =
                            ActiveSegmentReadState::DrainingTheSegmentRotatedAway(
                                active_segment_line_reader,
                            );
                    } else if !self.follow_appended_records {
                        self.active_segment_read_state =
                            ActiveSegmentReadState::HandingOverTheLastUnfinishedLine(
                                active_segment_line_reader,
                            );
                    } else {
                        self.active_segment_read_state =
                            ActiveSegmentReadState::ActiveSegmentOpen(active_segment_line_reader);
                        return Ok(RuntimeLogInstanceReadStep::LiveEdgeReached);
                    }
                }
                ActiveSegmentReadState::DrainingTheSegmentRotatedAway(
                    mut rotated_away_segment_line_reader,
                ) => {
                    if let Some(line) = rotated_away_segment_line_reader.next_complete_line()? {
                        self.active_segment_read_state =
                            ActiveSegmentReadState::DrainingTheSegmentRotatedAway(
                                rotated_away_segment_line_reader,
                            );
                        return Ok(RuntimeLogInstanceReadStep::Line(line));
                    }
                    self.last_read_rotation_sequence = rotation_sequence_of_held_segment(
                        rotated_away_segment_line_reader.held_segment_file(),
                        &self.active_segment_path,
                        self.last_read_rotation_sequence,
                    )?;
                    let unfinished_line = rotated_away_segment_line_reader.take_unfinished_line();
                    drop(rotated_away_segment_line_reader);
                    self.active_segment_read_state =
                        ActiveSegmentReadState::ListRotatedSegmentsAndOpenTheActiveOne;
                    if !unfinished_line.is_empty() {
                        return Ok(RuntimeLogInstanceReadStep::Line(unfinished_line));
                    }
                }
                ActiveSegmentReadState::HandingOverTheLastUnfinishedLine(
                    mut active_segment_line_reader,
                ) => {
                    let unfinished_line = active_segment_line_reader.take_unfinished_line();
                    if !unfinished_line.is_empty() {
                        return Ok(RuntimeLogInstanceReadStep::Line(unfinished_line));
                    }
                }
                ActiveSegmentReadState::Finished => {
                    return Ok(RuntimeLogInstanceReadStep::InstanceFinished);
                }
            }
        }
    }
}

/// A moment in a read at which a test lands a change to the log directory, so a writer's
/// rotation or a retention falls exactly there rather than racing the read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeLogReadInterleavingPoint {
    AfterListingRotatedSegments,
    BeforeCheckingWhetherTheHeldSegmentWasRotatedAway,
    AfterCheckingWhetherTheHeldSegmentWasRotatedAway,
}

#[cfg(not(test))]
fn reached_interleaving_point(_interleaving_point: RuntimeLogReadInterleavingPoint) {}

#[cfg(test)]
fn reached_interleaving_point(interleaving_point: RuntimeLogReadInterleavingPoint) {
    tests::land_the_change_registered_for(interleaving_point);
}

/// The rotated segments of `active_segment_path` on disk, oldest first; none when the directory
/// cannot be listed.
fn list_rotated_segment_sequences(active_segment_path: &Path) -> Vec<u64> {
    let listed_rotation_sequences =
        rotated_runtime_log_segment_sequences_on_disk(active_segment_path).unwrap_or_default();
    reached_interleaving_point(RuntimeLogReadInterleavingPoint::AfterListingRotatedSegments);
    listed_rotation_sequences
}

/// The active segment opened, beside the rotated segments that precede it.
///
/// A rotation landing between the listing and the open would hand back an active segment the
/// listing does not account for, so the listing is retaken until its newest sequence is
/// unchanged across the open.
fn open_active_segment_after_listing(
    active_segment_path: &Path,
) -> Result<(Option<File>, Vec<u64>), RuntimeLogSegmentReadFailure> {
    loop {
        let listed_rotation_sequences = list_rotated_segment_sequences(active_segment_path);
        let active_segment_file = match File::open(active_segment_path) {
            Ok(active_segment_file) => active_segment_file,
            Err(open_failure) if open_failure.kind() == io::ErrorKind::NotFound => {
                return Ok((None, listed_rotation_sequences));
            }
            Err(open_failure) => {
                return Err(RuntimeLogSegmentReadFailure::of_segment(
                    active_segment_path,
                    open_failure,
                ));
            }
        };
        let relisted_rotation_sequences = list_rotated_segment_sequences(active_segment_path);
        if relisted_rotation_sequences.last() == listed_rotation_sequences.last() {
            return Ok((Some(active_segment_file), listed_rotation_sequences));
        }
    }
}

/// Rotation `rotation_sequence` of `active_segment_path` opened, or `None` with a note when
/// retention removed it first.
fn open_rotated_segment(
    active_segment_path: &Path,
    rotation_sequence: u64,
    note_output: &mut dyn Write,
) -> Result<Option<RuntimeLogSegmentLineReader>, RuntimeLogSegmentReadFailure> {
    let rotated_segment_path =
        rotated_runtime_log_segment_path(active_segment_path, rotation_sequence);
    match File::open(&rotated_segment_path) {
        Ok(rotated_segment_file) => Ok(Some(RuntimeLogSegmentLineReader::reading(
            &rotated_segment_path,
            rotated_segment_file,
        ))),
        Err(open_failure) if open_failure.kind() == io::ErrorKind::NotFound => {
            let _ = writeln!(
                note_output,
                "note: log segment {} was removed by retention before it was read; skipping.",
                rotated_segment_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            );
            Ok(None)
        }
        Err(open_failure) => Err(RuntimeLogSegmentReadFailure::of_segment(
            &rotated_segment_path,
            open_failure,
        )),
    }
}

fn is_the_same_file(first: &std::fs::Metadata, second: &std::fs::Metadata) -> bool {
    (first.dev(), first.ino()) == (second.dev(), second.ino())
}

/// Whether the active name now names a different file than the held one.
///
/// A missing name is not a rotation: a rotation's two renames leave it absent for a moment, and
/// so does a rotation the writer backed out of, which gives the name back to the very file held
/// here.
fn held_segment_was_rotated_away(
    held_segment_file: &File,
    active_segment_path: &Path,
) -> Result<bool, RuntimeLogSegmentReadFailure> {
    reached_interleaving_point(
        RuntimeLogReadInterleavingPoint::BeforeCheckingWhetherTheHeldSegmentWasRotatedAway,
    );
    let rotated_away = held_segment_file
        .metadata()
        .and_then(
            |held_segment_metadata| match std::fs::metadata(active_segment_path) {
                Ok(named_segment_metadata) => Ok(!is_the_same_file(
                    &named_segment_metadata,
                    &held_segment_metadata,
                )),
                Err(stat_failure) if stat_failure.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(stat_failure) => Err(stat_failure),
            },
        )
        .map_err(|stat_failure| {
            RuntimeLogSegmentReadFailure::of_segment(active_segment_path, stat_failure)
        });
    reached_interleaving_point(
        RuntimeLogReadInterleavingPoint::AfterCheckingWhetherTheHeldSegmentWasRotatedAway,
    );
    rotated_away
}

/// The sequence the held file was rotated to, matched by inode rather than counted.
///
/// Asked while the file is still held open, so its inode cannot have been reused. A held file
/// retention already deleted matches nothing, and every rotated segment newer than
/// `last_read_rotation_sequence` is then newer than it too.
fn rotation_sequence_of_held_segment(
    held_segment_file: &File,
    active_segment_path: &Path,
    last_read_rotation_sequence: u64,
) -> Result<u64, RuntimeLogSegmentReadFailure> {
    let held_segment_metadata = held_segment_file.metadata().map_err(|stat_failure| {
        RuntimeLogSegmentReadFailure::of_segment(active_segment_path, stat_failure)
    })?;
    for rotation_sequence in list_rotated_segment_sequences(active_segment_path) {
        let rotated_segment_path =
            rotated_runtime_log_segment_path(active_segment_path, rotation_sequence);
        let rotated_segment_metadata = match std::fs::metadata(&rotated_segment_path) {
            Ok(rotated_segment_metadata) => rotated_segment_metadata,
            Err(stat_failure) if stat_failure.kind() == io::ErrorKind::NotFound => continue,
            Err(stat_failure) => {
                return Err(RuntimeLogSegmentReadFailure::of_segment(
                    &rotated_segment_path,
                    stat_failure,
                ));
            }
        };
        if is_the_same_file(&rotated_segment_metadata, &held_segment_metadata) {
            return Ok(rotation_sequence);
        }
    }
    Ok(last_read_rotation_sequence)
}

/// What one step of reading a runtime's log came to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RuntimeLogReadStep {
    /// One record that passed the filters, rendered with its newline.
    RenderedRecord(String),
    /// A follow has read everything on disk and nothing new has landed yet.
    LiveEdgeReached,
    /// Every segment is read and the read does not follow.
    Finished,
}

/// A runtime's log rendered record by record, starting from one instance of it.
///
/// A restart under a pinned runtime_id writes a second instance for the same runtime, so a
/// follow switches to it and says so; without that the follow sits on a file that will never
/// grow again and goes silently quiet.
pub(crate) struct RuntimeLogRecordsReader {
    log_directory: PathBuf,
    instance_being_read: RuntimeLogInstanceOnDisk,
    instance_segments_reader: RuntimeLogInstanceSegmentsReader,
    record_filters: RuntimeLogRecordFilters,
    follow_appended_records: bool,
}

impl RuntimeLogRecordsReader {
    /// A read of `instance_to_read`, whose segments lie in `log_directory`.
    pub(crate) fn reading(
        log_directory: &Path,
        instance_to_read: RuntimeLogInstanceOnDisk,
        record_filters: RuntimeLogRecordFilters,
        follow_appended_records: bool,
    ) -> Self {
        Self {
            log_directory: log_directory.to_path_buf(),
            instance_segments_reader: RuntimeLogInstanceSegmentsReader::reading(
                instance_to_read.active_segment_path.clone(),
                follow_appended_records,
            ),
            instance_being_read: instance_to_read,
            record_filters,
            follow_appended_records,
        }
    }

    /// The next rendered record, live edge, or end; warnings and notes go to `note_output`.
    pub(crate) fn next_step(
        &mut self,
        note_output: &mut dyn Write,
    ) -> Result<RuntimeLogReadStep, RuntimeLogSegmentReadFailure> {
        loop {
            match self.instance_segments_reader.next_step(note_output)? {
                RuntimeLogInstanceReadStep::Line(line) => {
                    if let Some(event) = decode_runtime_log_line(&line, note_output)
                        && self.record_filters.admits(&event)
                    {
                        let mut rendered_record = String::new();
                        format_event_pretty(&event, &mut rendered_record);
                        return Ok(RuntimeLogReadStep::RenderedRecord(rendered_record));
                    }
                }
                RuntimeLogInstanceReadStep::LiveEdgeReached => {
                    if let Some(newer_instance) = newest_runtime_log_instance_in_directory(
                        &self.log_directory,
                        &self.instance_being_read.runtime_id,
                    )
                    .map_err(|listing_failure| {
                        RuntimeLogSegmentReadFailure::of_segment(
                            &self.log_directory,
                            listing_failure,
                        )
                    })? && newer_instance
                        .compare_started_at(&self.instance_being_read)
                        .is_gt()
                    {
                        let _ = writeln!(
                            note_output,
                            "note: runtime '{}' restarted into a newer log file; switching.",
                            self.instance_being_read.runtime_id
                        );
                        self.instance_segments_reader = RuntimeLogInstanceSegmentsReader::reading(
                            newer_instance.active_segment_path.clone(),
                            self.follow_appended_records,
                        );
                        self.instance_being_read = newer_instance;
                        continue;
                    }
                    return Ok(RuntimeLogReadStep::LiveEdgeReached);
                }
                RuntimeLogInstanceReadStep::InstanceFinished => {
                    return Ok(RuntimeLogReadStep::Finished);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use serde_json::{Value, json};
    use streamlib_runtime_client_contract::runtime_log_file_paths::runtime_log_instances_in_directory;

    use super::*;

    type InterleavedDirectoryChange = Box<dyn FnMut(RuntimeLogReadInterleavingPoint)>;

    thread_local! {
        static INTERLEAVED_DIRECTORY_CHANGE: RefCell<Option<InterleavedDirectoryChange>> =
            RefCell::new(None);
    }

    pub(super) fn land_the_change_registered_for(
        interleaving_point: RuntimeLogReadInterleavingPoint,
    ) {
        INTERLEAVED_DIRECTORY_CHANGE.with(|interleaved_directory_change| {
            if let Some(change) = interleaved_directory_change.borrow_mut().as_mut() {
                change(interleaving_point);
            }
        });
    }

    /// Run `change` at every interleaving point this test thread's reads reach.
    fn interleave_directory_change(change: impl FnMut(RuntimeLogReadInterleavingPoint) + 'static) {
        INTERLEAVED_DIRECTORY_CHANGE.with(|interleaved_directory_change| {
            *interleaved_directory_change.borrow_mut() = Some(Box::new(change));
        });
    }

    /// Run `change` once, the first time a read reaches `interleaving_point`.
    fn interleave_directory_change_once(
        interleaving_point: RuntimeLogReadInterleavingPoint,
        change: impl FnOnce() + 'static,
    ) {
        let mut change_not_yet_landed = Some(change);
        interleave_directory_change(move |reached_interleaving_point| {
            if reached_interleaving_point == interleaving_point
                && let Some(change) = change_not_yet_landed.take()
            {
                change();
            }
        });
    }

    /// More lines than any rotation scenario writes, so a reader that re-reads a segment in a
    /// loop fails the assertion rather than hanging.
    const MOST_LINES_A_ROTATION_SCENARIO_READS: usize = 20;

    fn a_log_record(overrides: Value) -> Value {
        let mut record = json!({
            "schema_version": 1,
            "host_ts": 1_786_136_667_573_387_556_u64,
            "runtime_id": "Rabc",
            "source": "rust",
            "level": "info",
            "message": "Creating Runner",
            "target": "tatolabd",
            "intercepted": false,
        });
        for (field_name, field_value) in overrides.as_object().unwrap() {
            record[field_name] = field_value.clone();
        }
        record
    }

    fn a_log_line_with_message(message: &str) -> String {
        format!("{}\n", a_log_record(json!({"message": message})))
    }

    fn log_lines_of(records: &[Value]) -> String {
        records.iter().map(|record| format!("{record}\n")).collect()
    }

    fn write_segment(segment_path: &Path, messages: &[&str]) {
        std::fs::write(
            segment_path,
            messages
                .iter()
                .map(|message| a_log_line_with_message(message))
                .collect::<String>(),
        )
        .unwrap();
    }

    fn append_to_segment(segment_path: &Path, appended_text: &str) {
        std::fs::OpenOptions::new()
            .append(true)
            .open(segment_path)
            .unwrap()
            .write_all(appended_text.as_bytes())
            .unwrap();
    }

    /// Move the active segment to its rotated name and leave an empty file under the active
    /// name. The engine gets there through a `.rotating` replacement and two renames; what a
    /// reader can observe of that is this end state, or the active name briefly absent.
    fn rotate_like_the_engine(active_segment_path: &Path, rotation_sequence: u64) {
        std::fs::rename(
            active_segment_path,
            rotated_runtime_log_segment_path(active_segment_path, rotation_sequence),
        )
        .unwrap();
        File::create(active_segment_path).unwrap();
    }

    /// The instance `Rabc-1000`, whether or not any of its segments is on disk yet.
    fn instance_rabc_1000(log_directory: &Path) -> RuntimeLogInstanceOnDisk {
        RuntimeLogInstanceOnDisk {
            runtime_id: "Rabc".to_owned(),
            started_at_millis_digits: "1000".to_owned(),
            active_segment_path: log_directory.join("Rabc-1000.jsonl"),
            total_segment_bytes: 0,
        }
    }

    fn message_of(rendered_record: &str) -> String {
        rendered_record
            .trim_end_matches('\n')
            .split_once(" — ")
            .unwrap()
            .1
            .to_owned()
    }

    /// The rendered records up to the end or the next live edge, at most
    /// [`MOST_LINES_A_ROTATION_SCENARIO_READS`].
    fn rendered_records_up_to_the_live_edge(
        runtime_log_records_reader: &mut RuntimeLogRecordsReader,
        note_output: &mut Vec<u8>,
    ) -> Vec<String> {
        let mut rendered_records = Vec::new();
        while rendered_records.len() < MOST_LINES_A_ROTATION_SCENARIO_READS {
            match runtime_log_records_reader.next_step(note_output).unwrap() {
                RuntimeLogReadStep::RenderedRecord(rendered_record) => {
                    rendered_records.push(rendered_record)
                }
                RuntimeLogReadStep::LiveEdgeReached | RuntimeLogReadStep::Finished => break,
            }
        }
        rendered_records
    }

    /// A read of `Rabc-1000` that does not follow: its rendered records and its notes.
    fn read_every_record_of_rabc_1000(
        log_directory: &Path,
        record_filters: RuntimeLogRecordFilters,
    ) -> (Vec<String>, String) {
        let mut note_output = Vec::new();
        let mut runtime_log_records_reader = RuntimeLogRecordsReader::reading(
            log_directory,
            instance_rabc_1000(log_directory),
            record_filters,
            false,
        );
        let rendered_records =
            rendered_records_up_to_the_live_edge(&mut runtime_log_records_reader, &mut note_output);
        assert_eq!(
            runtime_log_records_reader
                .next_step(&mut note_output)
                .unwrap(),
            RuntimeLogReadStep::Finished
        );
        (rendered_records, String::from_utf8(note_output).unwrap())
    }

    /// The messages of every record of `Rabc-1000` a read that does not follow renders.
    fn messages_of_every_record_of_rabc_1000(log_directory: &Path) -> Vec<String> {
        messages_of(
            &read_every_record_of_rabc_1000(log_directory, RuntimeLogRecordFilters::default()).0,
        )
    }

    fn messages_of(rendered_records: &[String]) -> Vec<String> {
        rendered_records
            .iter()
            .map(|rendered| message_of(rendered))
            .collect()
    }

    /// A `--follow` read under test, and everything it has written to stderr so far.
    struct FollowedRuntimeLog {
        runtime_log_records_reader: RuntimeLogRecordsReader,
        note_output: Vec<u8>,
    }

    impl FollowedRuntimeLog {
        fn following(log_directory: &Path, instance_to_follow: RuntimeLogInstanceOnDisk) -> Self {
            Self {
                runtime_log_records_reader: RuntimeLogRecordsReader::reading(
                    log_directory,
                    instance_to_follow,
                    RuntimeLogRecordFilters::default(),
                    true,
                ),
                note_output: Vec::new(),
            }
        }

        /// The messages of the records read before the follow next reaches the live edge.
        fn messages_up_to_the_live_edge(&mut self) -> Vec<String> {
            messages_of(&rendered_records_up_to_the_live_edge(
                &mut self.runtime_log_records_reader,
                &mut self.note_output,
            ))
        }

        fn notes(&self) -> String {
            String::from_utf8(self.note_output.clone()).unwrap()
        }
    }

    #[test]
    fn a_replayed_record_renders_exactly_as_the_runtime_mirrored_it() {
        let log_directory = tempfile::tempdir().unwrap();
        std::fs::write(
            log_directory.path().join("Rabc-1000.jsonl"),
            log_lines_of(&[a_log_record(json!({
                "target": "streamlib_media_builtins::camera_source",
                "pipeline_id": "pipe",
                "processor_id": "proc",
                "rhi_op": "acquire_texture",
                "attrs": {
                    "width": 1920,
                    "origin": "Settings::defaults()",
                    "device": "Logitech Café",
                    "v": 1e-5,
                },
            }))]),
        )
        .unwrap();

        let (rendered_records, notes) = read_every_record_of_rabc_1000(
            log_directory.path(),
            RuntimeLogRecordFilters::default(),
        );

        assert_eq!(
            rendered_records,
            [
                "21:04:27.573 [ INFO] [Rabc/rust] streamlib_media_builtins::camera_source — \
              Creating Runner pipeline_id=pipe processor_id=proc rhi_op=acquire_texture \
              device=\"Logitech Café\" origin=\"Settings::defaults()\" v=0.00001 width=1920\n"
            ]
        );
        assert_eq!(notes, "");
    }

    #[test]
    fn each_filter_narrows_to_the_records_it_names() {
        let log_directory = tempfile::tempdir().unwrap();
        std::fs::write(
            log_directory.path().join("Rabc-1000.jsonl"),
            log_lines_of(&[
                a_log_record(json!({"message": "info-rust"})),
                a_log_record(
                    json!({"message": "warn-python", "level": "warn", "source": "python"}),
                ),
                a_log_record(
                    json!({"message": "rhi-op", "rhi_op": "acquire", "processor_id": "proc-1"}),
                ),
                a_log_record(
                    json!({"message": "intercepted", "intercepted": true, "pipeline_id": "pipe-1"}),
                ),
            ]),
        )
        .unwrap();

        for (record_filters, expected_messages) in [
            (
                RuntimeLogRecordFilters::default(),
                &["info-rust", "warn-python", "rhi-op", "intercepted"][..],
            ),
            (
                RuntimeLogRecordFilters {
                    minimum_level: Some(LogLevel::Warn),
                    ..Default::default()
                },
                &["warn-python"],
            ),
            (
                RuntimeLogRecordFilters {
                    source: Some(Source::Python),
                    ..Default::default()
                },
                &["warn-python"],
            ),
            (
                RuntimeLogRecordFilters {
                    rhi_operations_only: true,
                    ..Default::default()
                },
                &["rhi-op"],
            ),
            (
                RuntimeLogRecordFilters {
                    intercepted_only: true,
                    ..Default::default()
                },
                &["intercepted"],
            ),
            (
                RuntimeLogRecordFilters {
                    processor_id: Some("proc-1".to_owned()),
                    ..Default::default()
                },
                &["rhi-op"],
            ),
            (
                RuntimeLogRecordFilters {
                    pipeline_id: Some("pipe-1".to_owned()),
                    ..Default::default()
                },
                &["intercepted"],
            ),
            (
                RuntimeLogRecordFilters {
                    processor_id: Some("absent".to_owned()),
                    ..Default::default()
                },
                &[],
            ),
        ] {
            let (rendered_records, _) =
                read_every_record_of_rabc_1000(log_directory.path(), record_filters.clone());

            assert_eq!(
                rendered_records
                    .iter()
                    .map(|rendered| message_of(rendered).split(' ').next().unwrap().to_owned())
                    .collect::<Vec<_>>(),
                expected_messages,
                "{record_filters:?}"
            );
        }
    }

    #[test]
    fn a_level_floor_admits_its_own_level_and_every_more_severe_one() {
        let log_directory = tempfile::tempdir().unwrap();
        let levels_least_severe_first = ["trace", "debug", "info", "warn", "error"];
        std::fs::write(
            log_directory.path().join("Rabc-1000.jsonl"),
            log_lines_of(
                &levels_least_severe_first
                    .map(|level| a_log_record(json!({"level": level, "message": level}))),
            ),
        )
        .unwrap();

        for (floor_index, minimum_level) in [
            LogLevel::Trace,
            LogLevel::Debug,
            LogLevel::Info,
            LogLevel::Warn,
            LogLevel::Error,
        ]
        .into_iter()
        .enumerate()
        {
            let (rendered_records, _) = read_every_record_of_rabc_1000(
                log_directory.path(),
                RuntimeLogRecordFilters {
                    minimum_level: Some(minimum_level),
                    ..Default::default()
                },
            );

            assert_eq!(
                messages_of(&rendered_records),
                levels_least_severe_first[floor_index..],
                "{minimum_level:?}"
            );
        }
    }

    /// Each decodes as JSON but is not shaped like a record; carrying it into the renderer would
    /// end the whole read over one bad line. An unknown level is one of them: the record's level
    /// is the schema's enum.
    #[test]
    fn a_schema_invalid_record_is_skipped_with_a_warning_like_a_malformed_line() {
        const SCHEMA_WARNING: &str =
            "warning: skipping JSONL line whose fields do not match the record schema\n";
        for (schema_invalid_line, expected_warning) in [
            (json!({"host_ts": null, "level": "info"}), SCHEMA_WARNING),
            (json!({"host_ts": 1, "level": 7}), SCHEMA_WARNING),
            (
                a_log_record(json!({"attrs": "not a mapping"})),
                SCHEMA_WARNING,
            ),
            (a_log_record(json!({"level": "fatal"})), SCHEMA_WARNING),
            (
                json!(["not", "an", "object"]),
                "warning: skipping JSONL line that is not a record object\n",
            ),
        ] {
            let log_directory = tempfile::tempdir().unwrap();
            std::fs::write(
                log_directory.path().join("Rabc-1000.jsonl"),
                log_lines_of(&[
                    schema_invalid_line.clone(),
                    a_log_record(json!({"message": "good"})),
                ]),
            )
            .unwrap();

            let (rendered_records, notes) = read_every_record_of_rabc_1000(
                log_directory.path(),
                RuntimeLogRecordFilters::default(),
            );

            assert_eq!(
                messages_of(&rendered_records),
                ["good"],
                "{schema_invalid_line}"
            );
            assert_eq!(notes, expected_warning, "{schema_invalid_line}");
        }
    }

    #[test]
    fn a_malformed_line_is_reported_and_skipped_not_fatal() {
        let log_directory = tempfile::tempdir().unwrap();
        std::fs::write(
            log_directory.path().join("Rabc-1000.jsonl"),
            format!(
                "{}{{ truncated\n\n{}",
                a_log_line_with_message("before"),
                a_log_line_with_message("after")
            ),
        )
        .unwrap();

        let (rendered_records, notes) = read_every_record_of_rabc_1000(
            log_directory.path(),
            RuntimeLogRecordFilters::default(),
        );

        assert_eq!(messages_of(&rendered_records), ["before", "after"]);
        assert!(
            notes.starts_with("warning: skipping malformed JSONL line: "),
            "{notes}"
        );
        assert_eq!(
            notes.lines().count(),
            1,
            "a blank line is skipped without a word: {notes}"
        );
    }

    #[test]
    fn follow_yields_lines_appended_after_the_drain() {
        let log_directory = tempfile::tempdir().unwrap();
        let active_segment_path = log_directory.path().join("Rabc-1000.jsonl");
        write_segment(&active_segment_path, &["first"]);
        let mut followed_runtime_log = FollowedRuntimeLog::following(
            log_directory.path(),
            instance_rabc_1000(log_directory.path()),
        );
        assert_eq!(
            followed_runtime_log.messages_up_to_the_live_edge(),
            ["first"]
        );

        append_to_segment(&active_segment_path, &a_log_line_with_message("second"));

        assert_eq!(
            followed_runtime_log.messages_up_to_the_live_edge(),
            ["second"]
        );
        assert!(
            followed_runtime_log
                .messages_up_to_the_live_edge()
                .is_empty()
        );
        assert_eq!(followed_runtime_log.notes(), "");
    }

    /// A restart under a pinned runtime_id writes a second file for the same runtime. Without the
    /// switch the follow sits on a file that will never grow again and goes silently quiet.
    #[test]
    fn follow_switches_to_a_newer_file_when_the_runtime_restarts() {
        let log_directory = tempfile::tempdir().unwrap();
        write_segment(&log_directory.path().join("Rabc-1000.jsonl"), &["before"]);
        let mut followed_runtime_log = FollowedRuntimeLog::following(
            log_directory.path(),
            instance_rabc_1000(log_directory.path()),
        );
        assert_eq!(
            followed_runtime_log.messages_up_to_the_live_edge(),
            ["before"]
        );

        write_segment(
            &log_directory.path().join("Rabc-2000.jsonl"),
            &["after-restart"],
        );

        assert_eq!(
            followed_runtime_log.messages_up_to_the_live_edge(),
            ["after-restart"]
        );
        assert_eq!(
            followed_runtime_log.notes(),
            "note: runtime 'Rabc' restarted into a newer log file; switching.\n"
        );
    }

    #[test]
    fn reading_a_runtime_walks_its_rotated_segments_oldest_first() {
        let log_directory = tempfile::tempdir().unwrap();
        write_segment(&log_directory.path().join("Rabc-1000.10.jsonl"), &["ten"]);
        write_segment(&log_directory.path().join("Rabc-1000.9.jsonl"), &["nine"]);
        write_segment(
            &log_directory.path().join("Rabc-2000.1.jsonl"),
            &["another-instance"],
        );
        write_segment(&log_directory.path().join("Rabc-1000.jsonl"), &["active"]);

        assert_eq!(
            messages_of_every_record_of_rabc_1000(log_directory.path()),
            ["nine", "ten", "active"]
        );
    }

    #[test]
    fn a_runtime_whose_active_segment_is_missing_still_reads_its_rotated_ones() {
        let log_directory = tempfile::tempdir().unwrap();
        write_segment(
            &log_directory.path().join("Rabc-1000.1.jsonl"),
            &["rotated"],
        );

        assert_eq!(
            messages_of_every_record_of_rabc_1000(log_directory.path()),
            ["rotated"]
        );
    }

    #[test]
    fn a_rotated_segments_last_line_without_its_newline_is_still_read() {
        let log_directory = tempfile::tempdir().unwrap();
        std::fs::write(
            log_directory.path().join("Rabc-1000.1.jsonl"),
            a_log_line_with_message("unterminated").trim_end(),
        )
        .unwrap();
        write_segment(&log_directory.path().join("Rabc-1000.jsonl"), &["active"]);

        assert_eq!(
            messages_of_every_record_of_rabc_1000(log_directory.path()),
            ["unterminated", "active"]
        );
    }

    #[test]
    fn follow_carries_on_across_a_rotation() {
        let log_directory = tempfile::tempdir().unwrap();
        let active_segment_path = log_directory.path().join("Rabc-1000.jsonl");
        write_segment(&active_segment_path, &["first"]);
        let mut followed_runtime_log = FollowedRuntimeLog::following(
            log_directory.path(),
            instance_rabc_1000(log_directory.path()),
        );
        assert_eq!(
            followed_runtime_log.messages_up_to_the_live_edge(),
            ["first"]
        );

        append_to_segment(&active_segment_path, &a_log_line_with_message("second"));
        rotate_like_the_engine(&active_segment_path, 1);
        append_to_segment(&active_segment_path, &a_log_line_with_message("third"));

        assert_eq!(
            followed_runtime_log.messages_up_to_the_live_edge(),
            ["second", "third"]
        );
        assert_eq!(followed_runtime_log.notes(), "");
    }

    #[test]
    fn follow_reads_every_segment_rotated_between_two_polls() {
        let log_directory = tempfile::tempdir().unwrap();
        let active_segment_path = log_directory.path().join("Rabc-1000.jsonl");
        write_segment(&active_segment_path, &["first"]);
        let mut followed_runtime_log = FollowedRuntimeLog::following(
            log_directory.path(),
            instance_rabc_1000(log_directory.path()),
        );
        assert_eq!(
            followed_runtime_log.messages_up_to_the_live_edge(),
            ["first"]
        );

        append_to_segment(&active_segment_path, &a_log_line_with_message("second"));
        rotate_like_the_engine(&active_segment_path, 1);
        write_segment(&active_segment_path, &["third"]);
        rotate_like_the_engine(&active_segment_path, 2);
        write_segment(&active_segment_path, &["fourth"]);

        assert_eq!(
            followed_runtime_log.messages_up_to_the_live_edge(),
            ["second", "third", "fourth"]
        );
    }

    #[test]
    fn follow_with_the_active_segment_missing_waits_at_the_edge_for_it() {
        let log_directory = tempfile::tempdir().unwrap();
        write_segment(
            &log_directory.path().join("Rabc-1000.1.jsonl"),
            &["rotated"],
        );
        let mut followed_runtime_log = FollowedRuntimeLog::following(
            log_directory.path(),
            instance_rabc_1000(log_directory.path()),
        );
        assert_eq!(
            followed_runtime_log.messages_up_to_the_live_edge(),
            ["rotated"]
        );

        write_segment(&log_directory.path().join("Rabc-1000.jsonl"), &["active"]);

        assert_eq!(
            followed_runtime_log.messages_up_to_the_live_edge(),
            ["active"]
        );
    }

    #[test]
    fn a_segment_retention_removed_before_it_was_read_is_skipped_with_a_note() {
        let log_directory = tempfile::tempdir().unwrap();
        let doomed_segment_path = log_directory.path().join("Rabc-1000.1.jsonl");
        write_segment(&doomed_segment_path, &["doomed"]);
        write_segment(
            &log_directory.path().join("Rabc-1000.2.jsonl"),
            &["survivor"],
        );
        write_segment(&log_directory.path().join("Rabc-1000.jsonl"), &["active"]);
        interleave_directory_change_once(
            RuntimeLogReadInterleavingPoint::AfterListingRotatedSegments,
            move || std::fs::remove_file(&doomed_segment_path).unwrap(),
        );

        let (rendered_records, notes) = read_every_record_of_rabc_1000(
            log_directory.path(),
            RuntimeLogRecordFilters::default(),
        );

        assert_eq!(messages_of(&rendered_records), ["survivor", "active"]);
        assert_eq!(
            notes,
            "note: log segment Rabc-1000.1.jsonl was removed by retention before it was read; \
             skipping.\n"
        );
    }

    #[test]
    fn a_record_flushed_just_before_a_rotation_is_read_before_the_segment_after_it() {
        let log_directory = tempfile::tempdir().unwrap();
        let active_segment_path = log_directory.path().join("Rabc-1000.jsonl");
        write_segment(&active_segment_path, &["first"]);
        interleave_directory_change_once(
            RuntimeLogReadInterleavingPoint::BeforeCheckingWhetherTheHeldSegmentWasRotatedAway,
            move || {
                append_to_segment(
                    &active_segment_path,
                    &a_log_line_with_message("flushed-before-rotation"),
                );
                rotate_like_the_engine(&active_segment_path, 1);
                write_segment(&active_segment_path, &["after"]);
            },
        );

        let (rendered_records, notes) = read_every_record_of_rabc_1000(
            log_directory.path(),
            RuntimeLogRecordFilters::default(),
        );

        assert_eq!(
            messages_of(&rendered_records),
            ["first", "flushed-before-rotation", "after"]
        );
        assert_eq!(notes, "");
    }

    #[test]
    fn a_rotation_between_listing_and_opening_keeps_the_segment_it_rotated() {
        let log_directory = tempfile::tempdir().unwrap();
        let active_segment_path = log_directory.path().join("Rabc-1000.jsonl");
        write_segment(&active_segment_path, &["before-rotation"]);
        interleave_directory_change_once(
            RuntimeLogReadInterleavingPoint::AfterListingRotatedSegments,
            move || {
                rotate_like_the_engine(&active_segment_path, 1);
                write_segment(&active_segment_path, &["after"]);
            },
        );

        assert_eq!(
            messages_of_every_record_of_rabc_1000(log_directory.path()),
            ["before-rotation", "after"]
        );
    }

    /// The writer renames the active segment away, fails to put a replacement in its place, and
    /// renames it back. A reader looking in that gap sees no name.
    #[test]
    fn a_rotation_the_writer_backed_out_of_repeats_no_record() {
        let log_directory = tempfile::tempdir().unwrap();
        let active_segment_path = log_directory.path().join("Rabc-1000.jsonl");
        let rotated_segment_path = log_directory.path().join("Rabc-1000.1.jsonl");
        write_segment(&active_segment_path, &["a1", "a2"]);
        interleave_directory_change(move |interleaving_point| match interleaving_point {
            RuntimeLogReadInterleavingPoint::BeforeCheckingWhetherTheHeldSegmentWasRotatedAway => {
                std::fs::rename(&active_segment_path, &rotated_segment_path).unwrap();
            }
            RuntimeLogReadInterleavingPoint::AfterCheckingWhetherTheHeldSegmentWasRotatedAway => {
                std::fs::rename(&rotated_segment_path, &active_segment_path).unwrap();
            }
            RuntimeLogReadInterleavingPoint::AfterListingRotatedSegments => {}
        });

        assert_eq!(
            messages_of_every_record_of_rabc_1000(log_directory.path()),
            ["a1", "a2"]
        );
    }

    /// With one segment retained, earlier rotations are already deleted, so the held file becomes
    /// `.5` while the reader has read no rotated segment at all.
    #[test]
    fn a_held_segment_is_matched_to_its_rotated_name_rather_than_counted() {
        let log_directory = tempfile::tempdir().unwrap();
        let active_segment_path = log_directory.path().join("Rabc-1000.jsonl");
        write_segment(&active_segment_path, &["held"]);
        interleave_directory_change_once(
            RuntimeLogReadInterleavingPoint::BeforeCheckingWhetherTheHeldSegmentWasRotatedAway,
            move || {
                rotate_like_the_engine(&active_segment_path, 5);
                write_segment(&active_segment_path, &["after"]);
            },
        );

        let (rendered_records, notes) = read_every_record_of_rabc_1000(
            log_directory.path(),
            RuntimeLogRecordFilters::default(),
        );

        assert_eq!(messages_of(&rendered_records), ["held", "after"]);
        assert_eq!(notes, "");
    }

    #[test]
    fn a_record_caught_half_written_is_held_until_its_newline_lands() {
        let log_directory = tempfile::tempdir().unwrap();
        let active_segment_path = log_directory.path().join("Rabc-1000.jsonl");
        let whole_record = a_log_line_with_message("second");
        let (record_front, record_rest) = whole_record.split_at(20);
        std::fs::write(
            &active_segment_path,
            format!("{}{record_front}", a_log_line_with_message("first")),
        )
        .unwrap();
        let mut followed_runtime_log = FollowedRuntimeLog::following(
            log_directory.path(),
            instance_rabc_1000(log_directory.path()),
        );
        assert_eq!(
            followed_runtime_log.messages_up_to_the_live_edge(),
            ["first"]
        );

        append_to_segment(&active_segment_path, record_rest);

        assert_eq!(
            followed_runtime_log.messages_up_to_the_live_edge(),
            ["second"]
        );
        assert_eq!(followed_runtime_log.notes(), "");
    }

    #[test]
    fn a_runtime_id_containing_dashes_reads_its_own_segments() {
        let log_directory = tempfile::tempdir().unwrap();
        write_segment(
            &log_directory.path().join("R-with-dashes-1234.jsonl"),
            &["dashed"],
        );
        let mut listed_instances =
            runtime_log_instances_in_directory(log_directory.path()).unwrap();
        assert_eq!(listed_instances.len(), 1);
        let mut note_output = Vec::new();
        let mut runtime_log_records_reader = RuntimeLogRecordsReader::reading(
            log_directory.path(),
            listed_instances.remove(0),
            RuntimeLogRecordFilters::default(),
            false,
        );

        assert_eq!(
            messages_of(&rendered_records_up_to_the_live_edge(
                &mut runtime_log_records_reader,
                &mut note_output
            )),
            ["dashed"]
        );
    }
}
