// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CI gate for the "ABI bump ⇒ coordinated republish" CD step.
//!
//! `STREAMLIB_ABI_VERSION` (in `streamlib-plugin-abi`) is the C-ABI contract a
//! `dlopen`-loaded package cdylib and the source-built host must agree on. A
//! package resolves the *published* `streamlib` SDK by version; the host builds
//! from source. If the ABI constant moves without a coordinated SDK republish
//! (a new workspace version + every package pin bumped so the published SDK is
//! at the new ABI), a cdylib built against the old SDK is correctly refused at
//! load with `PluginAbiVersionMismatch` — the handshake working as designed on
//! a genuine version skew.
//!
//! This lint fails a PR that changes the ABI constant without also changing the
//! `[workspace.package]` version — the first, mechanical half of that
//! republish. It compares the two values at the merge-base against the working
//! tree; it is registry-free (a `git` diff, no network).

use anyhow::Result;
use std::path::Path;
use std::process::Command;

/// The file carrying the ABI-version constant.
const ABI_FILE: &str = "runtime/streamlib-plugin-abi/src/lib.rs";
/// The manifest carrying `[workspace.package] version`.
const CARGO_TOML: &str = "Cargo.toml";

/// Parse the `STREAMLIB_ABI_VERSION` constant definition out of the ABI source.
///
/// Matches only the `const STREAMLIB_ABI_VERSION` definition line — not doc
/// comments, `$crate::STREAMLIB_ABI_VERSION` macro uses, or `assert_eq!`
/// references that mention the name without `const`.
pub fn extract_abi_version(src: &str) -> Option<u32> {
    for line in src.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        if !trimmed.contains("const STREAMLIB_ABI_VERSION") {
            continue;
        }
        // Value is the digit run after `=`, up to the terminating `;` — so a
        // trailing `// was 3`-style comment can't leak its digits in.
        let after_eq = line.split('=').nth(1)?;
        let value = after_eq.split(';').next()?.trim();
        return value.parse().ok();
    }
    None
}

/// Parse `version = "X"` from the `[workspace.package]` section of a Cargo
/// manifest. Ignores `version` keys in any other section (e.g. a dependency's).
pub fn extract_workspace_package_version(cargo_toml: &str) -> Option<String> {
    let mut in_workspace_package = false;
    for line in cargo_toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_workspace_package = trimmed == "[workspace.package]";
            continue;
        }
        if !in_workspace_package {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("version") {
            if let Some(value) = rest.trim_start().strip_prefix('=') {
                return Some(value.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

/// The decision: an ABI-version change unaccompanied by a workspace-version
/// change is the CD violation this gate exists to catch.
pub fn evaluate(abi_changed: bool, workspace_version_changed: bool) -> Result<()> {
    if abi_changed && !workspace_version_changed {
        Err(anyhow::anyhow!(
            "check-abi-republish: STREAMLIB_ABI_VERSION changed but the \
             [workspace.package] version in Cargo.toml did not. An ABI bump is \
             a breaking plugin-ABI change and requires a coordinated SDK \
             republish: bump the workspace version, bump every package's \
             streamlib* pin to match, and republish the SDK — otherwise a \
             cdylib built against the old SDK is refused at load with \
             PluginAbiVersionMismatch. See the \"ABI-version bump\" section of \
             docs/architecture/static-registry.md."
        ))
    } else {
        Ok(())
    }
}

pub fn run(project_root: &Path) -> Result<()> {
    let base = match resolve_merge_base(project_root) {
        Some(base) => base,
        None => {
            // No base ref to diff against (e.g. a shallow clone that didn't
            // fetch the target branch). Can't make a determination; don't
            // block. The workflow fetches the base (actions/checkout with
            // fetch-depth: 0) so the real check runs in CI.
            println!(
                "check-abi-republish: no merge-base against the target branch \
                 could be resolved; skipping. Ensure the base ref is fetched \
                 (actions/checkout with fetch-depth: 0)."
            );
            return Ok(());
        }
    };
    run_against_base(project_root, &base)
}

fn run_against_base(project_root: &Path, base: &str) -> Result<()> {
    let abi_before = abi_version_at_rev(project_root, base);
    let abi_after = std::fs::read_to_string(project_root.join(ABI_FILE))
        .ok()
        .as_deref()
        .and_then(extract_abi_version);
    let version_before = git_show(project_root, &base, CARGO_TOML)
        .as_deref()
        .and_then(extract_workspace_package_version);
    let version_after = std::fs::read_to_string(project_root.join(CARGO_TOML))
        .ok()
        .as_deref()
        .and_then(extract_workspace_package_version);

    let abi_changed = abi_before != abi_after;
    let workspace_version_changed = version_before != version_after;

    if abi_changed {
        println!(
            "check-abi-republish: STREAMLIB_ABI_VERSION {:?} -> {:?}; \
             [workspace.package] version {:?} -> {:?} (merge-base {})",
            abi_before, abi_after, version_before, version_after, base,
        );
    } else {
        println!(
            "check-abi-republish: STREAMLIB_ABI_VERSION unchanged ({:?}); no \
             coordinated republish required (merge-base {})",
            abi_after, base,
        );
    }

    evaluate(abi_changed, workspace_version_changed)
}

/// Resolve the merge-base commit of HEAD against the PR's target branch.
///
/// Prefers `origin/$GITHUB_BASE_REF` (the PR target on GitHub Actions), then a
/// bare `$GITHUB_BASE_REF`, then `origin/main` / `main`.
fn resolve_merge_base(project_root: &Path) -> Option<String> {
    let base_ref = std::env::var("GITHUB_BASE_REF")
        .ok()
        .filter(|s| !s.is_empty());
    let candidates: Vec<String> = match base_ref {
        Some(branch) => vec![format!("origin/{branch}"), branch],
        None => vec!["origin/main".to_string(), "main".to_string()],
    };
    candidates
        .iter()
        .find_map(|candidate| merge_base(project_root, candidate))
}

fn merge_base(project_root: &Path, base: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["merge-base", base, "HEAD"])
        .current_dir(project_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}

/// The ABI version at `rev`: primary lookup at [`ABI_FILE`], falling back to
/// searching `rev`'s tree for the `const STREAMLIB_ABI_VERSION` definition
/// when the file is absent at that path (the file moved between `rev` and the
/// working tree — a pure rename must not read as "ABI version appeared").
fn abi_version_at_rev(project_root: &Path, rev: &str) -> Option<u32> {
    if let Some(content) = git_show(project_root, rev, ABI_FILE) {
        return extract_abi_version(&content);
    }
    for path in git_grep_rs_files(project_root, rev, "const STREAMLIB_ABI_VERSION") {
        if let Some(version) = git_show(project_root, rev, &path)
            .as_deref()
            .and_then(extract_abi_version)
        {
            return Some(version);
        }
    }
    None
}

/// `git grep -l -F <pattern> <rev> -- '*.rs'` — the `.rs` paths in `rev`'s
/// tree containing `pattern`, or empty on any failure (no match, no git).
fn git_grep_rs_files(project_root: &Path, rev: &str, pattern: &str) -> Vec<String> {
    let Ok(output) = Command::new("git")
        .args(["grep", "-l", "-F", pattern, rev, "--", "*.rs"])
        .current_dir(project_root)
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        // Output lines are `<rev>:<path>`; a sha/refname carries no `:`.
        .filter_map(|line| line.split_once(':').map(|(_, path)| path.to_string()))
        .collect()
}

/// `git show <rev>:<path>` — the file's contents at `rev`, or `None` if absent.
fn git_show(project_root: &Path, rev: &str, path: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("show")
        .arg(format!("{rev}:{path}"))
        .current_dir(project_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_abi_version_parses_the_const_definition() {
        assert_eq!(
            extract_abi_version("pub const STREAMLIB_ABI_VERSION: u32 = 5;"),
            Some(5)
        );
    }

    #[test]
    fn extract_abi_version_ignores_docs_macros_and_asserts() {
        // Only the `const` definition should match — not the doc comment, the
        // `$crate::` macro use, or the `assert_eq!` reference.
        let src = "//! pin the same [`STREAMLIB_ABI_VERSION`].\n\
                   pub const STREAMLIB_ABI_VERSION: u32 = 7;\n\
                   // abi_version: $crate::STREAMLIB_ABI_VERSION,\n\
                   assert_eq!(STREAMLIB_ABI_VERSION, 7);\n";
        assert_eq!(extract_abi_version(src), Some(7));
    }

    #[test]
    fn extract_abi_version_ignores_trailing_comment_digits() {
        assert_eq!(
            extract_abi_version("pub const STREAMLIB_ABI_VERSION: u32 = 5; // was 3"),
            Some(5)
        );
    }

    #[test]
    fn extract_abi_version_none_when_absent() {
        assert_eq!(extract_abi_version("fn main() {}\n"), None);
    }

    #[test]
    fn extract_workspace_version_reads_the_workspace_package_section() {
        let toml = "[package]\nversion = \"9.9.9\"\n\n\
                    [workspace.package]\nversion = \"0.6.0\"\nedition = \"2024\"\n";
        assert_eq!(
            extract_workspace_package_version(toml).as_deref(),
            Some("0.6.0")
        );
    }

    #[test]
    fn extract_workspace_version_ignores_dependency_versions() {
        // A `version` inside [workspace.dependencies] must not be picked up.
        let toml = "[workspace.package]\nedition = \"2024\"\n\n\
                    [workspace.dependencies]\nserde = { version = \"1\" }\n";
        assert_eq!(extract_workspace_package_version(toml), None);
    }

    // The gate's decision, exercised through the real extractors on synthetic
    // before/after snapshots. Mentally revert the `&& !workspace_version_changed`
    // guard in `evaluate` and `abi_bump_with_version_bump_passes` fails — the
    // test locks the version-coordination requirement, not just "ABI changed".

    #[test]
    fn abi_bump_without_version_bump_is_rejected() {
        let abi_before = extract_abi_version("pub const STREAMLIB_ABI_VERSION: u32 = 5;");
        let abi_after = extract_abi_version("pub const STREAMLIB_ABI_VERSION: u32 = 6;");
        // Version constant across before/after — the missing coordinated bump.
        let ver_before =
            extract_workspace_package_version("[workspace.package]\nversion = \"0.6.0\"\n");
        let ver_after =
            extract_workspace_package_version("[workspace.package]\nversion = \"0.6.0\"\n");
        let abi_changed = abi_before != abi_after;
        let version_changed = ver_before != ver_after;
        assert!(abi_changed, "ABI 5->6 must register as changed");
        assert!(!version_changed, "version held constant");
        assert!(evaluate(abi_changed, version_changed).is_err());
    }

    #[test]
    fn abi_bump_with_version_bump_passes() {
        let abi_before = extract_abi_version("pub const STREAMLIB_ABI_VERSION: u32 = 5;");
        let abi_after = extract_abi_version("pub const STREAMLIB_ABI_VERSION: u32 = 6;");
        let ver_before =
            extract_workspace_package_version("[workspace.package]\nversion = \"0.6.0\"\n");
        let ver_after =
            extract_workspace_package_version("[workspace.package]\nversion = \"0.7.0\"\n");
        let abi_changed = abi_before != abi_after;
        let version_changed = ver_before != ver_after;
        assert!(abi_changed && version_changed);
        assert!(evaluate(abi_changed, version_changed).is_ok());
    }

    #[test]
    fn version_only_change_passes() {
        // This very PR: workspace version moved, ABI constant did not.
        assert!(evaluate(false, true).is_ok());
    }

    #[test]
    fn no_change_passes() {
        assert!(evaluate(false, false).is_ok());
    }

    // ----- rename-aware base-side lookup (fixture git repos) -----

    /// Old on-disk location of the ABI file — what a pre-restructure base
    /// commit carries while the working tree carries [`ABI_FILE`].
    const OLD_ABI_FILE: &str = "libs/streamlib-plugin-abi/src/lib.rs";

    fn git_in(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write_file(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// Fixture repo whose base commit carries the ABI const at the OLD path
    /// plus a workspace Cargo.toml; the working tree then carries the file at
    /// the NEW path ([`ABI_FILE`]) with `working_tree_abi_version`. Returns
    /// `(tempdir, base_sha)`.
    fn fixture_repo_with_moved_abi_file(
        working_tree_abi_version: u32,
    ) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git_in(root, &["init", "-q"]);
        git_in(root, &["config", "user.email", "test@example.com"]);
        git_in(root, &["config", "user.name", "Test"]);
        git_in(root, &["config", "commit.gpgsign", "false"]);
        write_file(
            root,
            OLD_ABI_FILE,
            "pub const STREAMLIB_ABI_VERSION: u32 = 5;\n",
        );
        write_file(
            root,
            "Cargo.toml",
            "[workspace.package]\nversion = \"0.6.0\"\n",
        );
        git_in(root, &["add", "-A"]);
        git_in(root, &["commit", "-q", "-m", "base"]);
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root)
            .output()
            .expect("rev-parse");
        let base = String::from_utf8_lossy(&output.stdout).trim().to_string();
        // Working tree: the file moved to the new zone path (uncommitted, the
        // shape run() sees mid-PR). Same workspace version — no bump.
        std::fs::remove_file(root.join(OLD_ABI_FILE)).unwrap();
        write_file(
            root,
            ABI_FILE,
            &format!("pub const STREAMLIB_ABI_VERSION: u32 = {working_tree_abi_version};\n"),
        );
        (dir, base)
    }

    #[test]
    fn moved_abi_file_with_unchanged_version_passes() {
        // A pure file move (value 5 at the base's old path, 5 at the working
        // tree's new path, no workspace bump) must NOT read as an ABI change.
        // Mentally revert `abi_version_at_rev`'s fallback: the base-side
        // lookup returns None, None != Some(5) reads as "changed", and the
        // gate fails — this test locks the rename-aware fallback.
        let (dir, base) = fixture_repo_with_moved_abi_file(5);
        assert_eq!(abi_version_at_rev(dir.path(), &base), Some(5));
        run_against_base(dir.path(), &base)
            .expect("pure ABI-file move with unchanged version must pass");
    }

    #[test]
    fn moved_abi_file_with_genuine_bump_and_no_workspace_bump_fails() {
        // The fallback must not weaken the gate: a genuine 5 -> 6 bump across
        // the same file move, with no workspace-version change, still fails.
        // This also guards against a lazier "skip when the base-side lookup
        // misses" fix, which would let the bump through silently.
        let (dir, base) = fixture_repo_with_moved_abi_file(6);
        assert_eq!(abi_version_at_rev(dir.path(), &base), Some(5));
        run_against_base(dir.path(), &base)
            .expect_err("ABI bump without workspace-version bump must fail across a file move");
    }
}
