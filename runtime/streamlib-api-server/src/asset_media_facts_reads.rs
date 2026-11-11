// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The reads of assets' media facts this runtime queues, one at a time off the async runtime:
//! each answered from its project's cache when kept there, queued on a miss, and announced on
//! the project's change feed when it finishes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use streamlib_asset_media_facts::{
    AssetMediaFactsCacheOfAProject, AssetMediaFactsCachedInTheProject,
};
use streamlib_project_repository::{
    ProjectAsset, ProjectAssetKind, ProjectRepositoriesOpenInThisRuntime,
};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    AssetMediaFactsReadOnTheWire, ProjectAssetOnTheWire, ProjectAssetWithItsMediaFactsOnTheWire,
};
use uuid::Uuid;

/// How many of a project's latest finished reads the runtime keeps for its change feed.
const FINISHED_MEDIA_FACTS_READS_KEPT_PER_PROJECT: usize = 100;

const MEDIA_FACTS_STATE_READ: &str = "read";
const MEDIA_FACTS_STATE_READING: &str = "reading";
const MEDIA_FACTS_STATE_UNREADABLE: &str = "unreadable";

/// One read of an asset's bytes the runtime queued.
struct AssetMediaFactsReadQueued {
    project: Uuid,
    project_directory: PathBuf,
    asset: Uuid,
    content_sha256: String,
    asset_file_path: PathBuf,
}

/// One finished read as a project's change feed holds it, with the second it finished at.
struct AssetMediaFactsReadFinished {
    read_at_unix_seconds: i64,
    asset: Uuid,
    content_sha256: String,
    media_facts_state: &'static str,
}

/// The reads of assets' media facts this runtime queued, and the latest finished on each
/// project since it started, held in memory only, since no history records a read.
pub(crate) struct AssetMediaFactsReadsInThisRuntime {
    project_repositories_open_in_this_runtime: Arc<ProjectRepositoriesOpenInThisRuntime>,
    reads_queued_or_under_way: Mutex<HashSet<(PathBuf, String)>>,
    finished_reads_oldest_first_by_project:
        Mutex<HashMap<Uuid, VecDeque<AssetMediaFactsReadFinished>>>,
    read_queue_to_the_reading_thread: OnceLock<Sender<AssetMediaFactsReadQueued>>,
}

/// The thread that reads assets' media facts did not start, so a read cannot be queued.
struct ReadingThreadNotRunning;

/// What the runtime has of one asset's media facts now.
pub(crate) enum AssetMediaFactsNow {
    /// A stream asset, whose graph `graph` serves; no facts are read for it.
    NotRead,
    /// Its bytes are being read.
    Reading,
    /// What was read from its bytes.
    Kept(AssetMediaFactsCachedInTheProject),
    /// No file is at its path, so nothing can be read.
    NoFileAtItsPath,
}

impl AssetMediaFactsReadsInThisRuntime {
    /// No read queued, each finished read announced on `project_repositories_open_in_this_runtime`'s
    /// change feed.
    pub(crate) fn announcing_on(
        project_repositories_open_in_this_runtime: Arc<ProjectRepositoriesOpenInThisRuntime>,
    ) -> Self {
        Self {
            project_repositories_open_in_this_runtime,
            reads_queued_or_under_way: Mutex::default(),
            finished_reads_oldest_first_by_project: Mutex::default(),
            read_queue_to_the_reading_thread: OnceLock::new(),
        }
    }

    /// What the runtime has of `asset`'s media facts, in the project `project` at
    /// `project_directory`; a miss queues a read, so a later call answers it.
    pub(crate) fn the_media_facts_of(
        self: &Arc<Self>,
        project: Uuid,
        project_directory: &Path,
        asset: &ProjectAsset,
    ) -> AssetMediaFactsNow {
        if asset.kind == ProjectAssetKind::Stream {
            return AssetMediaFactsNow::NotRead;
        }
        let asset_file_path = asset.path.on_disk_under(project_directory);
        if !asset_file_path.is_file() {
            return AssetMediaFactsNow::NoFileAtItsPath;
        }
        if let Some(kept) = AssetMediaFactsCacheOfAProject::of_the_project_at(project_directory)
            .cached(&asset.content_sha256, &asset_file_path)
        {
            return AssetMediaFactsNow::Kept(kept);
        }
        match self.queue_a_read(AssetMediaFactsReadQueued {
            project,
            project_directory: project_directory.to_path_buf(),
            asset: asset.asset,
            content_sha256: asset.content_sha256.clone(),
            asset_file_path,
        }) {
            Ok(()) => AssetMediaFactsNow::Reading,
            Err(ReadingThreadNotRunning) => {
                AssetMediaFactsNow::Kept(AssetMediaFactsCachedInTheProject::Unreadable {
                    media_facts_unreadable_because:
                        "the runtime's thread that reads assets' media facts is not running"
                            .to_owned(),
                })
            }
        }
    }

    /// `asset` as `list_assets` and `describe_asset` answer it, with its media facts now.
    pub(crate) fn the_asset_with_its_media_facts(
        self: &Arc<Self>,
        project: Uuid,
        project_directory: &Path,
        asset: &ProjectAsset,
        asset_on_the_wire: ProjectAssetOnTheWire,
    ) -> ProjectAssetWithItsMediaFactsOnTheWire {
        let without_facts = |media_facts_state: Option<&str>, because: Option<String>| {
            ProjectAssetWithItsMediaFactsOnTheWire {
                asset: asset_on_the_wire.clone(),
                media_facts_state: media_facts_state.map(str::to_owned),
                media_facts_unreadable_because: because,
                media_facts: None,
                preview: None,
            }
        };
        match self.the_media_facts_of(project, project_directory, asset) {
            AssetMediaFactsNow::NotRead => without_facts(None, None),
            AssetMediaFactsNow::Reading => without_facts(Some(MEDIA_FACTS_STATE_READING), None),
            AssetMediaFactsNow::NoFileAtItsPath => without_facts(
                Some(MEDIA_FACTS_STATE_UNREADABLE),
                Some("no file is at the asset's path".to_owned()),
            ),
            AssetMediaFactsNow::Kept(AssetMediaFactsCachedInTheProject::Unreadable {
                media_facts_unreadable_because,
            }) => without_facts(
                Some(MEDIA_FACTS_STATE_UNREADABLE),
                Some(media_facts_unreadable_because),
            ),
            AssetMediaFactsNow::Kept(AssetMediaFactsCachedInTheProject::Read {
                media_facts,
                preview,
            }) => ProjectAssetWithItsMediaFactsOnTheWire {
                asset: asset_on_the_wire,
                media_facts_state: Some(MEDIA_FACTS_STATE_READ.to_owned()),
                media_facts_unreadable_because: None,
                media_facts: Some(*media_facts),
                preview,
            },
        }
    }

    fn queue_a_read(
        self: &Arc<Self>,
        read: AssetMediaFactsReadQueued,
    ) -> Result<(), ReadingThreadNotRunning> {
        let read_key = (read.asset_file_path.clone(), read.content_sha256.clone());
        if !self
            .reads_queued_or_under_way
            .lock()
            .insert(read_key.clone())
        {
            return Ok(());
        }
        let read_queue = self.read_queue_to_the_reading_thread.get_or_init(|| {
            let (read_queue, reads_queued) = channel::<AssetMediaFactsReadQueued>();
            let reads = Arc::downgrade(self);
            let started = std::thread::Builder::new()
                .name("asset-media-facts-reads".to_owned())
                .spawn(move || {
                    for queued in reads_queued {
                        let Some(reads) = reads.upgrade() else {
                            return;
                        };
                        reads.read_and_announce(queued);
                    }
                });
            if let Err(why) = started {
                tracing::warn!(%why, "the thread that reads assets' media facts did not start; no facts are read");
            }
            read_queue
        });
        read_queue.send(read).map_err(|_| {
            self.reads_queued_or_under_way.lock().remove(&read_key);
            ReadingThreadNotRunning
        })
    }

    fn read_and_announce(&self, queued: AssetMediaFactsReadQueued) {
        let cache = AssetMediaFactsCacheOfAProject::of_the_project_at(&queued.project_directory);
        let kept = cache
            .cached(&queued.content_sha256, &queued.asset_file_path)
            .unwrap_or_else(|| {
                cache.read_and_keep(&queued.content_sha256, &queued.asset_file_path)
            });
        let media_facts_state = match kept {
            AssetMediaFactsCachedInTheProject::Read { .. } => MEDIA_FACTS_STATE_READ,
            AssetMediaFactsCachedInTheProject::Unreadable { .. } => MEDIA_FACTS_STATE_UNREADABLE,
        };
        self.reads_queued_or_under_way
            .lock()
            .remove(&(queued.asset_file_path, queued.content_sha256.clone()));
        {
            let mut finished_reads_oldest_first_by_project =
                self.finished_reads_oldest_first_by_project.lock();
            let finished_reads_oldest_first = finished_reads_oldest_first_by_project
                .entry(queued.project)
                .or_default();
            if finished_reads_oldest_first.len() == FINISHED_MEDIA_FACTS_READS_KEPT_PER_PROJECT {
                finished_reads_oldest_first.pop_front();
            }
            finished_reads_oldest_first.push_back(AssetMediaFactsReadFinished {
                read_at_unix_seconds:
                    crate::mcp_project_change_feed::the_wall_clock_in_unix_seconds(),
                asset: queued.asset,
                content_sha256: queued.content_sha256,
                media_facts_state,
            });
        }
        self.project_repositories_open_in_this_runtime
            .announce_media_facts_read(queued.project);
    }

    /// `project`'s kept finished reads, newest first, each with the second it finished at.
    pub(crate) fn the_finished_reads_newest_first(
        &self,
        project: Uuid,
    ) -> Vec<(i64, AssetMediaFactsReadOnTheWire)> {
        self.finished_reads_oldest_first_by_project
            .lock()
            .get(&project)
            .map(|finished_reads_oldest_first| {
                finished_reads_oldest_first
                    .iter()
                    .rev()
                    .map(|finished| {
                        (
                            finished.read_at_unix_seconds,
                            AssetMediaFactsReadOnTheWire {
                                asset: finished.asset.to_string(),
                                content_sha256: finished.content_sha256.clone(),
                                media_facts_state: finished.media_facts_state.to_owned(),
                                read_at:
                                    crate::mcp_project_change_feed::rfc_3339_utc_of_unix_seconds(
                                        finished.read_at_unix_seconds,
                                    ),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use streamlib_project_repository::ProjectRelativeAssetPath;

    use super::*;

    #[test]
    fn an_asset_is_unreadable_in_words_when_the_reading_thread_is_not_running() {
        let project_directory = tempfile::tempdir().unwrap();
        std::fs::write(project_directory.path().join("cover.png"), b"png bytes").unwrap();
        let asset = ProjectAsset {
            asset: Uuid::new_v4(),
            kind: ProjectAssetKind::Media,
            path: ProjectRelativeAssetPath::parse("cover.png").unwrap(),
            content_sha256: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
                .to_owned(),
            content_byte_len: 9,
            import_settings: Default::default(),
        };
        let reads = Arc::new(AssetMediaFactsReadsInThisRuntime::announcing_on(Arc::new(
            ProjectRepositoriesOpenInThisRuntime::default(),
        )));
        let (read_queue_to_no_thread, no_thread) = channel();
        drop(no_thread);
        assert!(
            reads
                .read_queue_to_the_reading_thread
                .set(read_queue_to_no_thread)
                .is_ok()
        );
        let AssetMediaFactsNow::Kept(AssetMediaFactsCachedInTheProject::Unreadable {
            media_facts_unreadable_because,
        }) = reads.the_media_facts_of(Uuid::new_v4(), project_directory.path(), &asset)
        else {
            panic!("the asset is not unreadable");
        };
        assert_eq!(
            media_facts_unreadable_because,
            "the runtime's thread that reads assets' media facts is not running"
        );
        assert!(reads.reads_queued_or_under_way.lock().is_empty());
    }
}
