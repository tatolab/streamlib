// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Drift trip-wire for the vendored vulkanalia fork trees
//! (`vendor/tatolab-vulkanalia{,-sys,-vma}`).
//!
//! The vendored sources are a verbatim copy of a pinned fork rev plus a
//! short, documented local-patch list (see
//! `docs/architecture/vendored-vulkanalia.md`). Nothing in-tree may edit
//! them casually — the loudest failure mode being a routine workspace
//! `cargo fmt --all` sweep silently rewriting vendored files (`cargo fmt
//! --check` already disagrees with the vendored formatting, and no stable
//! rustfmt exclusion mechanism exists — `rustfmt.toml`'s `ignore` is
//! nightly-only). Prose alone can't stop that, so this check pins one
//! deterministic content hash per vendored crate dir, in the
//! `twin_drift_guard` trip-wire style: any byte change (edit, reformat,
//! added/removed/renamed file) fails CI and names the offending dir.
//!
//! When it trips on a DELIBERATE re-vendor or documented local patch:
//! follow the update recipe in `docs/architecture/vendored-vulkanalia.md`
//! and update the recorded hashes below in the same commit — the hash
//! change in the diff is the loud signal the vendored tree was touched.

use anyhow::{Context, Result};
use std::path::Path;

/// The vendored crate dirs this check pins, with their recorded tree hashes.
/// Updated only on a deliberate re-vendor / documented local patch, in the
/// same commit that changes the tree.
const VENDORED_TREES: &[(&str, u64)] = &[
    ("vendor/tatolab-vulkanalia", 0x7508_cfa2_9c2b_b9c7),
    ("vendor/tatolab-vulkanalia-sys", 0xef46_fa14_69b6_8757),
    ("vendor/tatolab-vulkanalia-vma", 0xac41_8fe4_7384_c0c9),
];

/// FNV-1a 64 — deterministic (platform/version-stable), matching the
/// engine's `twin_drift_guard` trip-wire style.
fn fnv1a_bytes(h: u64, bytes: &[u8]) -> u64 {
    let mut h = h;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// Hash a directory tree deterministically: walk every file, sort by
/// `/`-separated relative path, fold in each path + a `0x00` separator +
/// the file bytes + a `0xFF` separator. Renames, adds, removals, and
/// content edits all change the hash.
pub fn hash_dir_tree(dir: &Path) -> Result<u64> {
    let mut files: Vec<(String, std::path::PathBuf)> = Vec::new();
    for entry in walkdir::WalkDir::new(dir).sort_by_file_name() {
        let entry = entry.with_context(|| format!("walk {}", dir.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(dir)
            .expect("walkdir yields paths under its root")
            .to_string_lossy()
            .replace('\\', "/");
        files.push((rel, entry.path().to_path_buf()));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut h = FNV_OFFSET_BASIS;
    for (rel, path) in files {
        h = fnv1a_bytes(h, rel.as_bytes());
        h = fnv1a_bytes(h, &[0x00]);
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        h = fnv1a_bytes(h, &bytes);
        h = fnv1a_bytes(h, &[0xFF]);
    }
    Ok(h)
}

/// One drifted vendored tree: `(dir, expected, actual)`.
pub struct VendoredTreeDrift {
    pub dir: &'static str,
    pub expected: u64,
    pub actual: u64,
}

/// Compare each recorded vendored tree against its on-disk hash.
pub fn check_trees(
    project_root: &Path,
    expected: &[(&'static str, u64)],
) -> Result<Vec<VendoredTreeDrift>> {
    let mut drifted = Vec::new();
    for &(dir, want) in expected {
        let got = hash_dir_tree(&project_root.join(dir))?;
        if got != want {
            drifted.push(VendoredTreeDrift {
                dir,
                expected: want,
                actual: got,
            });
        }
    }
    Ok(drifted)
}

pub fn run(project_root: &Path) -> Result<()> {
    let drifted = check_trees(project_root, VENDORED_TREES)?;
    if drifted.is_empty() {
        tracing::info!(
            trees = VENDORED_TREES.len(),
            "check-vendored-vulkanalia: all vendored trees match their recorded hashes"
        );
        return Ok(());
    }
    let mut msg = String::from(
        "check-vendored-vulkanalia: vendored vulkanalia tree(s) DRIFTED from the recorded \
         hash — the vendored fork sources are verbatim-by-contract and must not be edited \
         or reformatted in place (a workspace `cargo fmt --all` sweep is the classic \
         accidental cause; fmt sweeps must exclude vendor/tatolab-vulkanalia*).\n",
    );
    for d in &drifted {
        msg.push_str(&format!(
            "  {}: expected {:#018x}, found {:#018x}\n",
            d.dir, d.expected, d.actual
        ));
    }
    msg.push_str(
        "If this change is a DELIBERATE re-vendor or a documented local patch, follow the \
         update recipe in docs/architecture/vendored-vulkanalia.md and update the recorded \
         hashes in xtask/src/check_vendored_vulkanalia.rs (VENDORED_TREES) in the SAME \
         commit, using the `found` values above. Otherwise revert the vendored-tree edit.",
    );
    anyhow::bail!(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, contents: &[u8]) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn tree_hash_is_stable_across_recomputation() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "Cargo.toml", b"[package]\nname = \"x\"\n");
        write(tmp.path(), "src/lib.rs", b"pub fn f() {}\n");
        let a = hash_dir_tree(tmp.path()).unwrap();
        let b = hash_dir_tree(tmp.path()).unwrap();
        assert_eq!(a, b, "same tree must hash identically");
    }

    #[test]
    fn mutated_byte_trips_the_check() {
        let tmp = tempfile::tempdir().unwrap();
        let crate_dir = "vendor/tatolab-vulkanalia";
        write(
            &tmp.path().join(crate_dir),
            "src/lib.rs",
            b"pub fn f() {}\n",
        );
        let clean = hash_dir_tree(&tmp.path().join(crate_dir)).unwrap();
        let expected = [(crate_dir, clean)];
        assert!(
            check_trees(tmp.path(), &expected).unwrap().is_empty(),
            "unmutated tree must pass against its own hash"
        );

        // One mutated byte — the exact shape a fmt sweep or stray edit takes.
        write(
            &tmp.path().join(crate_dir),
            "src/lib.rs",
            b"pub fn f()  {}\n",
        );
        let drifted = check_trees(tmp.path(), &expected).unwrap();
        assert_eq!(drifted.len(), 1, "mutated byte must trip the check");
        assert_eq!(drifted[0].dir, crate_dir, "drift must name the crate dir");
        assert_ne!(drifted[0].actual, drifted[0].expected);
    }

    #[test]
    fn renamed_file_changes_the_hash() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "src/a.rs", b"x");
        let before = hash_dir_tree(tmp.path()).unwrap();
        std::fs::rename(tmp.path().join("src/a.rs"), tmp.path().join("src/b.rs")).unwrap();
        let after = hash_dir_tree(tmp.path()).unwrap();
        assert_ne!(before, after, "paths are folded into the hash");
    }

    /// The real guard: the repo's vendored trees match the recorded hashes.
    /// Mirrors `twin_drift_guard` — there is no fixture to update; making
    /// this pass after a vendored-tree change requires updating
    /// `VENDORED_TREES` in the same commit (per the provenance doc's recipe).
    #[test]
    fn repo_vendored_trees_match_recorded_hashes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let drifted = check_trees(root, VENDORED_TREES).unwrap();
        assert!(
            drifted.is_empty(),
            "vendored tree(s) drifted: {}",
            drifted
                .iter()
                .map(|d| format!(
                    "{} expected {:#018x} found {:#018x}",
                    d.dir, d.expected, d.actual
                ))
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
}
