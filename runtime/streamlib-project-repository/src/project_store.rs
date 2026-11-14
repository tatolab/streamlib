// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A project's store: the one embedded database the runtime keeps what it holds for a project
//! in, outside the project's history. Every concern is a named table in it. A write lands in
//! memory at once, and one writer thread commits every write waiting at most once per debounce
//! window, all of them in one write transaction, so a value changed at a control's rate never
//! reaches the disk at that rate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use redb::{ReadableDatabase, TableDefinition};
use serde_json::Value as JsonValue;

use crate::PROJECT_RUNTIME_DIRECTORY;

/// The store's file inside the project's runtime directory.
pub const PROJECT_STORE_FILE_NAME: &str = "project.redb";

/// The longest a write waits in memory before the store's writer commits it.
pub const PROJECT_STORE_WRITE_DEBOUNCE_WINDOW: Duration = Duration::from_millis(250);

/// One named table of a project's store, its keys text and its values JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectStoreTableName(&'static str);

impl ProjectStoreTableName {
    /// The table named `name`.
    pub const fn named(name: &'static str) -> Self {
        Self(name)
    }

    /// The table's name in the store's file.
    pub const fn as_str(self) -> &'static str {
        self.0
    }

    fn definition(self) -> TableDefinition<'static, &'static str, &'static [u8]> {
        TableDefinition::new(self.0)
    }
}

/// Why a project's store refused to open, read or commit.
#[derive(Debug, thiserror::Error)]
pub enum ProjectStoreRefusal {
    /// The store's directory could not be made.
    #[error("the project store's directory `{directory}` could not be made: {why}")]
    DirectoryNotMade {
        /// The directory the store lives in.
        directory: PathBuf,
        /// What the filesystem answered.
        why: std::io::Error,
    },
    /// The store's file did not open.
    #[error("the project store `{file}` did not open: {why}")]
    NotOpened {
        /// The store's file.
        file: PathBuf,
        /// What redb answered.
        why: String,
    },
    /// A read or a commit of the store failed.
    #[error("the project store `{file}` failed to {doing}: {why}")]
    Failed {
        /// The store's file.
        file: PathBuf,
        /// What the store was doing.
        doing: &'static str,
        /// What redb or the JSON codec answered.
        why: String,
    },
}

type TableAndKey = (ProjectStoreTableName, String);

type LatestValueOfEachKey = BTreeMap<TableAndKey, Option<Vec<u8>>>;

/// The writes made since the writer last committed, the latest of each key, `None` a removal,
/// and the batch a commit is writing now, which reads still see until it lands.
#[derive(Default)]
struct WritesWaitingForTheNextCommit {
    latest_of_each_key: LatestValueOfEachKey,
    batch_being_committed: LatestValueOfEachKey,
    first_waiting_since: Option<Instant>,
    the_store_is_closing: bool,
}

struct ProjectStoreSharedWithItsWriter {
    store_file: PathBuf,
    database: redb::Database,
    debounce_window: Duration,
    writes_waiting: Mutex<WritesWaitingForTheNextCommit>,
    a_write_is_waiting_or_the_store_closes: Condvar,
    #[cfg(test)]
    write_transactions_committed: AtomicU64,
    commit_in_progress: Mutex<()>,
}

/// A project's store, open with its one writer thread; dropped, it commits what still waits.
pub struct ProjectStore {
    shared_with_its_writer: Arc<ProjectStoreSharedWithItsWriter>,
    writer_thread: Option<JoinHandle<()>>,
}

impl ProjectStore {
    /// Open the store of the project at `project_directory`, making it on first use.
    pub fn open_in_the_project_at(project_directory: &Path) -> Result<Self, ProjectStoreRefusal> {
        Self::open_at_the_file(
            &project_directory
                .join(PROJECT_RUNTIME_DIRECTORY)
                .join(PROJECT_STORE_FILE_NAME),
            PROJECT_STORE_WRITE_DEBOUNCE_WINDOW,
        )
    }

    /// Open the store at `store_file`, its writer committing at most once per `debounce_window`.
    pub fn open_at_the_file(
        store_file: &Path,
        debounce_window: Duration,
    ) -> Result<Self, ProjectStoreRefusal> {
        if let Some(directory) = store_file.parent() {
            std::fs::create_dir_all(directory).map_err(|why| {
                ProjectStoreRefusal::DirectoryNotMade {
                    directory: directory.to_path_buf(),
                    why,
                }
            })?;
        }
        let database =
            redb::Database::create(store_file).map_err(|why| ProjectStoreRefusal::NotOpened {
                file: store_file.to_path_buf(),
                why: why.to_string(),
            })?;
        let shared_with_its_writer = Arc::new(ProjectStoreSharedWithItsWriter {
            store_file: store_file.to_path_buf(),
            database,
            debounce_window,
            writes_waiting: Mutex::default(),
            a_write_is_waiting_or_the_store_closes: Condvar::new(),
            #[cfg(test)]
            write_transactions_committed: AtomicU64::new(0),
            commit_in_progress: Mutex::new(()),
        });
        let writer_shared = Arc::clone(&shared_with_its_writer);
        let writer_thread = std::thread::Builder::new()
            .name("project-store-writer".to_string())
            .spawn(move || writer_shared.commit_waiting_writes_until_the_store_closes())
            .map_err(|why| ProjectStoreRefusal::NotOpened {
                file: store_file.to_path_buf(),
                why: format!("its writer thread did not start: {why}"),
            })?;
        Ok(Self {
            shared_with_its_writer,
            writer_thread: Some(writer_thread),
        })
    }

    /// Set `key` of `table` to `value`, readable at once and committed within the debounce window.
    pub fn write(
        &self,
        table: ProjectStoreTableName,
        key: &str,
        value: &JsonValue,
    ) -> Result<(), ProjectStoreRefusal> {
        let encoded = serde_json::to_vec(value).map_err(|why| ProjectStoreRefusal::Failed {
            file: self.shared_with_its_writer.store_file.clone(),
            doing: "encode a value",
            why: why.to_string(),
        })?;
        self.shared_with_its_writer
            .wait_to_commit((table, key.to_string()), Some(encoded));
        Ok(())
    }

    /// Remove `key` of `table`, gone at once and committed within the debounce window.
    pub fn remove(&self, table: ProjectStoreTableName, key: &str) {
        self.shared_with_its_writer
            .wait_to_commit((table, key.to_string()), None);
    }

    /// The value of `key` in `table`: the latest written, whether or not it is committed yet.
    pub fn read(
        &self,
        table: ProjectStoreTableName,
        key: &str,
    ) -> Result<Option<JsonValue>, ProjectStoreRefusal> {
        let table_and_key = (table, key.to_string());
        let waiting = {
            let writes_waiting = self.shared_with_its_writer.writes_waiting.lock();
            writes_waiting
                .latest_of_each_key
                .get(&table_and_key)
                .or_else(|| writes_waiting.batch_being_committed.get(&table_and_key))
                .cloned()
        };
        let encoded = match waiting {
            Some(waiting) => waiting,
            None => self.shared_with_its_writer.read_committed(table, key)?,
        };
        encoded
            .map(|encoded| self.shared_with_its_writer.decoded(&encoded))
            .transpose()
    }

    /// Every key `table` holds, written or committed, and none removed since.
    pub fn keys_in(
        &self,
        table: ProjectStoreTableName,
    ) -> Result<Vec<String>, ProjectStoreRefusal> {
        let _no_batch_lands_between_the_two_reads =
            self.shared_with_its_writer.commit_in_progress.lock();
        let mut held: std::collections::BTreeSet<String> = self
            .shared_with_its_writer
            .keys_committed_in(table)?
            .into_iter()
            .collect();
        let writes_waiting = self.shared_with_its_writer.writes_waiting.lock();
        for ((written_table, key), value) in writes_waiting
            .batch_being_committed
            .iter()
            .chain(writes_waiting.latest_of_each_key.iter())
        {
            if *written_table != table {
                continue;
            }
            match value {
                Some(_) => held.insert(key.clone()),
                None => held.remove(key),
            };
        }
        Ok(held.into_iter().collect())
    }

    /// Commit every write waiting now, without waiting for the debounce window; a commit that
    /// fails leaves its writes waiting for the writer to try again.
    pub fn commit_every_waiting_write_now(&self) -> Result<(), ProjectStoreRefusal> {
        self.shared_with_its_writer
            .take_and_commit_every_waiting_write()
    }

    #[cfg(test)]
    fn write_transactions_committed(&self) -> u64 {
        self.shared_with_its_writer
            .write_transactions_committed
            .load(Ordering::Acquire)
    }
}

impl Drop for ProjectStore {
    fn drop(&mut self) {
        self.shared_with_its_writer
            .writes_waiting
            .lock()
            .the_store_is_closing = true;
        self.shared_with_its_writer
            .a_write_is_waiting_or_the_store_closes
            .notify_all();
        if let Some(writer_thread) = self.writer_thread.take()
            && writer_thread.join().is_err()
        {
            tracing::error!(
                store_file = %self.shared_with_its_writer.store_file.display(),
                "the project store's writer thread panicked; writes it had not committed are lost"
            );
        }
    }
}

impl ProjectStoreSharedWithItsWriter {
    fn wait_to_commit(&self, table_and_key: TableAndKey, encoded: Option<Vec<u8>>) {
        let mut writes_waiting = self.writes_waiting.lock();
        writes_waiting
            .latest_of_each_key
            .insert(table_and_key, encoded);
        if writes_waiting.first_waiting_since.is_none() {
            writes_waiting.first_waiting_since = Some(Instant::now());
            self.a_write_is_waiting_or_the_store_closes.notify_all();
        }
    }

    fn take_every_waiting_write_into_the_batch_being_committed(&self) -> LatestValueOfEachKey {
        let mut writes_waiting = self.writes_waiting.lock();
        writes_waiting.first_waiting_since = None;
        writes_waiting.batch_being_committed =
            std::mem::take(&mut writes_waiting.latest_of_each_key);
        writes_waiting.batch_being_committed.clone()
    }

    /// The batch landed: reads find it on disk now.
    fn forget_the_batch_committed(&self) {
        self.writes_waiting.lock().batch_being_committed.clear();
    }

    /// The batch did not land: put each of its writes back unless a newer one waits, so the
    /// writer tries them again within the window.
    fn put_the_batch_back_to_wait(&self) {
        let mut writes_waiting = self.writes_waiting.lock();
        let batch = std::mem::take(&mut writes_waiting.batch_being_committed);
        for (table_and_key, encoded) in batch {
            writes_waiting
                .latest_of_each_key
                .entry(table_and_key)
                .or_insert(encoded);
        }
        if writes_waiting.first_waiting_since.is_none() {
            writes_waiting.first_waiting_since = Some(Instant::now());
        }
    }

    fn commit_waiting_writes_until_the_store_closes(&self) {
        loop {
            let mut writes_waiting = self.writes_waiting.lock();
            while writes_waiting.first_waiting_since.is_none()
                && !writes_waiting.the_store_is_closing
            {
                self.a_write_is_waiting_or_the_store_closes
                    .wait(&mut writes_waiting);
            }
            if let Some(first_waiting_since) = writes_waiting.first_waiting_since {
                let commit_at = first_waiting_since + self.debounce_window;
                while !writes_waiting.the_store_is_closing && Instant::now() < commit_at {
                    self.a_write_is_waiting_or_the_store_closes
                        .wait_until(&mut writes_waiting, commit_at);
                }
            }
            let the_store_is_closing = writes_waiting.the_store_is_closing;
            drop(writes_waiting);
            if let Err(refusal) = self.take_and_commit_every_waiting_write() {
                tracing::error!(%refusal, "the project store's writer could not commit");
            }
            if the_store_is_closing {
                return;
            }
        }
    }

    /// Take every waiting write and commit it, one taker at a time so an older batch never
    /// lands over a newer one.
    fn take_and_commit_every_waiting_write(&self) -> Result<(), ProjectStoreRefusal> {
        let _one_commit_at_a_time = self.commit_in_progress.lock();
        let batch = self.take_every_waiting_write_into_the_batch_being_committed();
        if batch.is_empty() {
            return Ok(());
        }
        match self.write_the_batch(&batch) {
            Ok(()) => {
                self.forget_the_batch_committed();
                Ok(())
            }
            Err(refusal) => {
                self.put_the_batch_back_to_wait();
                Err(refusal)
            }
        }
    }

    fn write_the_batch(&self, batch: &LatestValueOfEachKey) -> Result<(), ProjectStoreRefusal> {
        let failed = |doing: &'static str| {
            let store_file = self.store_file.clone();
            move |why: &dyn std::fmt::Display| ProjectStoreRefusal::Failed {
                file: store_file,
                doing,
                why: why.to_string(),
            }
        };
        let write_transaction = self
            .database
            .begin_write()
            .map_err(|why| failed("begin a write")(&why))?;
        {
            let mut tables_open: BTreeMap<ProjectStoreTableName, redb::Table<&str, &[u8]>> =
                BTreeMap::new();
            for ((table, key), encoded) in batch {
                if !tables_open.contains_key(table) {
                    let opened = write_transaction
                        .open_table(table.definition())
                        .map_err(|why| failed("open a table")(&why))?;
                    tables_open.insert(*table, opened);
                }
                let Some(opened) = tables_open.get_mut(table) else {
                    continue;
                };
                match encoded {
                    Some(encoded) => {
                        opened
                            .insert(key.as_str(), encoded.as_slice())
                            .map_err(|why| failed("write a value")(&why))?;
                    }
                    None => {
                        opened
                            .remove(key.as_str())
                            .map_err(|why| failed("remove a value")(&why))?;
                    }
                }
            }
        }
        write_transaction
            .commit()
            .map_err(|why| failed("commit")(&why))?;
        #[cfg(test)]
        self.write_transactions_committed
            .fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    fn keys_committed_in(
        &self,
        table: ProjectStoreTableName,
    ) -> Result<Vec<String>, ProjectStoreRefusal> {
        use redb::ReadableTable;
        let failed =
            |doing: &'static str, why: &dyn std::fmt::Display| ProjectStoreRefusal::Failed {
                file: self.store_file.clone(),
                doing,
                why: why.to_string(),
            };
        let read_transaction = self
            .database
            .begin_read()
            .map_err(|why| failed("begin a read", &why))?;
        let opened = match read_transaction.open_table(table.definition()) {
            Ok(opened) => opened,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(why) => return Err(failed("open a table", &why)),
        };
        opened
            .iter()
            .map_err(|why| failed("list the keys", &why))?
            .map(|entry| {
                entry
                    .map(|(key, _)| key.value().to_string())
                    .map_err(|why| failed("list the keys", &why))
            })
            .collect()
    }

    fn read_committed(
        &self,
        table: ProjectStoreTableName,
        key: &str,
    ) -> Result<Option<Vec<u8>>, ProjectStoreRefusal> {
        let failed =
            |doing: &'static str, why: &dyn std::fmt::Display| ProjectStoreRefusal::Failed {
                file: self.store_file.clone(),
                doing,
                why: why.to_string(),
            };
        let read_transaction = self
            .database
            .begin_read()
            .map_err(|why| failed("begin a read", &why))?;
        let opened = match read_transaction.open_table(table.definition()) {
            Ok(opened) => opened,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(why) => return Err(failed("open a table", &why)),
        };
        Ok(opened
            .get(key)
            .map_err(|why| failed("read a value", &why))?
            .map(|value| value.value().to_vec()))
    }

    fn decoded(&self, encoded: &[u8]) -> Result<JsonValue, ProjectStoreRefusal> {
        serde_json::from_slice(encoded).map_err(|why| ProjectStoreRefusal::Failed {
            file: self.store_file.clone(),
            doing: "decode a value",
            why: why.to_string(),
        })
    }
}

#[cfg(test)]
#[path = "project_store_tests.rs"]
mod project_store_tests;
