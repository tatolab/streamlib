// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib link` / `streamlib unlink` — whole-tree local checkout override.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use streamlib_idents::link_marker::{
    LINK_BACKUP_DIR, LINK_MANIFEST_FILE, LINK_STATE_DIR, LinkManifest, LinkMarkerError,
    LinkTransactionState, LinkedManifestFile,
};

/// Human-facing greppability marker on the emitted cargo `[patch]` block.
const CARGO_PATCH_MARKER: &str =
    "# streamlib-link — managed by `streamlib link`; removed by `streamlib unlink`";
/// The cargo `[patch.<source>]` source the link overrides. Internal cross-crate
/// deps + an out-of-tree consumer's `streamlib` deps resolve from crates.io (by
/// bare `version`), so the link patches `crates-io`. `[patch.crates-io]` is a
/// source replacement — cargo uses the patch's `path` and never queries
/// crates.io, so it works even though the SDK isn't published there yet.
const CARGO_PATCH_SOURCE: &str = "crates-io";
/// Checkout-relative path of the Python SDK (uv editable source target).
const PYTHON_SDK_REL: &str = "libs/streamlib-python";
/// Checkout-relative path of the Deno SDK entrypoint module.
const DENO_SDK_ENTRYPOINT_REL: &str = "libs/streamlib-deno/mod.ts";

/// Failure modes of `streamlib link` / `streamlib unlink`.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    /// The checkout path does not exist or is not a streamlib workspace.
    #[error("`{0}` is not a streamlib checkout (no Cargo.toml workspace at that path)")]
    NotAStreamlibCheckout(PathBuf),

    /// A link to a different checkout is already active; refuse to clobber it.
    #[error(
        "a link to `{active}` is already active in this consumer; run `streamlib unlink` before \
         linking `{requested}`"
    )]
    AlreadyLinkedElsewhere { active: PathBuf, requested: PathBuf },

    /// A previous link run crashed mid-apply and left torn state behind.
    #[error(
        "a previous `streamlib link` was interrupted mid-apply (marker: {path}); run \
         `streamlib unlink` to restore before linking again"
    )]
    TornLinkState { path: PathBuf },

    /// The link marker already exists at creation time (concurrent link run).
    #[error(
        "link state already exists at `{path}` — another `streamlib link` may be running \
         concurrently, or a previous link was interrupted; run `streamlib unlink` first"
    )]
    LinkMarkerAlreadyExists { path: PathBuf },

    /// `cargo metadata` on the checkout failed or produced no linkable crates.
    #[error("could not derive the linkable crate set from `{checkout}`: {detail}")]
    CrateSetDerivation { checkout: PathBuf, detail: String },

    /// A manifest we planned to edit could not be parsed.
    #[error("failed to parse `{path}`: {detail}")]
    ManifestParse { path: PathBuf, detail: String },

    /// The recorded link manifest / a backup is corrupt.
    #[error("link state at `{path}` is corrupt: {detail}")]
    CorruptLinkState { path: PathBuf, detail: String },

    /// Unlink found a file the user modified while the link was active.
    #[error(
        "`{path}` was modified while the link was active; refusing to clobber it — re-apply your \
         edit after unlinking, or run `streamlib unlink --force` to discard it and restore the \
         pre-link original"
    )]
    UnlinkRefusedModifiedFile { path: PathBuf },

    /// A rollback after a failed link could not restore every file.
    #[error(
        "link failed and rollback could not restore every file; originals are preserved under \
         `{backup_dir}` — resolve the filesystem issue and run `streamlib unlink` to finish \
         restoring. {detail}"
    )]
    RollbackIncomplete { backup_dir: PathBuf, detail: String },

    /// Post-link cargo resolution check failed; the link was rolled back. Only
    /// reached after the stale-lock cause has been ruled out (a transparent
    /// re-lock was attempted when a `Cargo.lock` was present), so the remaining
    /// cause is a genuine version-requirement mismatch or an unresolvable graph.
    #[error(
        "post-link verification failed and the link was rolled back: {detail}. Adjust the \
         consumer's `streamlib*` version requirements to admit the checkout's versions, or \
         re-run with `--skip-verify` to keep the link unverified"
    )]
    LinkVerificationFailed { detail: String },

    /// A pre-existing consumer `Cargo.lock` pins the streamlib crates to
    /// registry versions — cargo honors an existing lockfile over a newly-added
    /// `[patch]`, so the link's patch is silently ignored — and the transparent
    /// re-lock could not run. The link is left applied (the patch IS on disk) so
    /// the named command finishes it directly.
    #[error(
        "the consumer's Cargo.lock (`{lockfile}`) pins {crates} to registry versions, which \
         defeats the link's `[patch]` — cargo honors an existing lockfile over a newly-added \
         `[patch]`. The link is applied but unverified because the automatic re-lock could not \
         run ({detail}). Finish it by re-locking the streamlib crate set:\n\n    {command}\n\n\
         Or run `streamlib unlink` to remove the link"
    )]
    StaleConsumerLockRelockFailed {
        lockfile: PathBuf,
        crates: String,
        command: String,
        detail: String,
    },

    /// A filesystem operation failed.
    #[error("filesystem error at `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl LinkError {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        LinkError::Io {
            path: path.into(),
            source,
        }
    }
}

impl From<LinkMarkerError> for LinkError {
    fn from(e: LinkMarkerError) -> Self {
        match e {
            LinkMarkerError::CorruptLinkState { path, detail } => {
                LinkError::CorruptLinkState { path, detail }
            }
            LinkMarkerError::Io { path, source } => LinkError::Io { path, source },
            LinkMarkerError::PackRefusedWhileLinked { marker } => {
                // Not produced by link/unlink flows; map to corrupt-state shape
                // for completeness.
                LinkError::CorruptLinkState {
                    path: marker,
                    detail: "unexpected pack refusal in link flow".to_string(),
                }
            }
        }
    }
}

/// A computed manifest edit, not yet applied.
#[derive(Debug)]
struct PlannedEdit {
    /// Path relative to the consumer root (e.g. `.cargo/config.toml`).
    rel_path: PathBuf,
    /// Absolute path of the file to write.
    abs_path: PathBuf,
    /// The full post-edit content to write.
    new_content: String,
    /// Pre-edit bytes when the file already existed, else `None`.
    original: Option<Vec<u8>>,
}

/// Establish (or refresh) a whole-tree link from `consumer_root` to `checkout`.
#[tracing::instrument(skip_all, fields(consumer_root = %consumer_root.display(), checkout = %checkout.display(), skip_verify))]
pub fn link(consumer_root: &Path, checkout: &Path, skip_verify: bool) -> Result<(), LinkError> {
    let checkout = canonicalize_checkout(checkout)?;
    let consumer_root = consumer_root
        .canonicalize()
        .map_err(|e| LinkError::io(consumer_root, e))?;

    // Idempotency gates: different checkout ⇒ refuse; torn state ⇒ point at
    // `unlink`; identical active checkout ⇒ refresh (below).
    let existing = load_active_manifest(&consumer_root)?;
    if let Some(m) = &existing {
        if m.checkout != checkout {
            return Err(LinkError::AlreadyLinkedElsewhere {
                active: m.checkout.clone(),
                requested: checkout,
            });
        }
        if m.state == LinkTransactionState::Applying {
            return Err(LinkError::TornLinkState {
                path: consumer_root.join(LINK_STATE_DIR).join(LINK_MANIFEST_FILE),
            });
        }
    }

    // Derive everything failure-prone BEFORE tearing down an existing link, so
    // a failed derivation preserves the working link on a refresh.
    let crates = derive_linkable_crates(&checkout)?;
    if crates.is_empty() {
        return Err(LinkError::CrateSetDerivation {
            checkout: checkout.clone(),
            detail: "no workspace members matched the streamlib linkable set".into(),
        });
    }

    if existing.is_some() {
        tracing::info!("refreshing existing link (same checkout) — re-deriving crate set");
        println!(
            "Refreshing active link to {} (re-deriving overrides).",
            checkout.display()
        );
        unlink(&consumer_root, false)?;
    }

    // Snapshot the consumer's pre-link `Cargo.lock` — taken *after* a refresh's
    // unlink has restored the tree, so it is the true pre-link state. A
    // transparent re-lock during verification (below) mutates this file; the
    // snapshot is what `unlink` restores it to, keeping teardown byte-clean.
    let original_cargo_lock = read_optional_bytes(&consumer_root.join("Cargo.lock"))?;

    let edits = establish_link(&consumer_root, &checkout, &crates)?;

    println!("Linked streamlib → {}", checkout.display());
    println!("  Cargo crates redirected: {}", crates.len());
    for edit in &edits {
        println!("  Overrode: {}", edit.rel_path.display());
    }

    // Post-link verification: prove the [patch] actually took effect in the
    // consumer's cargo resolution. Two ways it can fail to redirect:
    //   1. A pre-existing `Cargo.lock` pins the streamlib crates to registry
    //      versions — cargo honors an existing lock over a newly-added [patch],
    //      so the patch is silently ignored. The fix is a re-lock, not a
    //      version-requirement change. `link` owns that step transparently.
    //   2. A semver-incompatible consumer requirement genuinely doesn't admit
    //      the checkout's versions. Roll the whole link back.
    if skip_verify {
        println!("  Verification skipped (--skip-verify).");
    } else if consumer_root.join("Cargo.toml").is_file() {
        match verify_and_remedy_patch_resolution(
            &consumer_root,
            &checkout,
            &crates,
            original_cargo_lock,
        )? {
            VerifyOutcome::Verified { relocked: 0 } => {
                println!(
                    "  Verified: streamlib crates resolve to the checkout via cargo metadata."
                );
            }
            VerifyOutcome::Verified { relocked } => {
                println!(
                    "  Re-locked the consumer's Cargo.lock ({relocked} crate(s)) so the \
                     [patch] takes effect; verified: streamlib crates resolve to the checkout."
                );
            }
            VerifyOutcome::RollBack { detail } => {
                unlink(&consumer_root, false)?;
                return Err(LinkError::LinkVerificationFailed { detail });
            }
            VerifyOutcome::RelockUnavailable {
                lockfile,
                crates,
                command,
                detail,
            } => {
                // Leave the link applied (the patch IS on disk) — the named
                // command finishes the re-lock directly. `Cargo.lock` was not
                // mutated, so teardown stays byte-clean.
                return Err(LinkError::StaleConsumerLockRelockFailed {
                    lockfile,
                    crates,
                    command,
                    detail,
                });
            }
        }
    }

    println!("Run `streamlib unlink` to restore.");
    Ok(())
}

/// The teardown action pass 1 of `unlink` decides for one touched file.
enum RestoreAction {
    /// Live content already matches the pre-link original — nothing to do.
    Skip,
    /// Write the (hash-verified) backup bytes back over the live file.
    RestoreOriginal(Vec<u8>),
    /// Delete a file the link created, then prune empty parents.
    RemoveCreated,
}

/// Tear down the active link, restoring every touched manifest byte-identically.
///
/// Recovers torn (`applying`) state from a crashed link run. Refuses to
/// clobber a file the user modified while the link was active unless `force`.
#[tracing::instrument(skip_all, fields(consumer_root = %consumer_root.display(), force))]
pub fn unlink(consumer_root: &Path, force: bool) -> Result<(), LinkError> {
    let consumer_root = consumer_root
        .canonicalize()
        .map_err(|e| LinkError::io(consumer_root, e))?;

    let manifest = match load_active_manifest(&consumer_root)? {
        Some(m) => m,
        None => {
            println!("No active streamlib link — nothing to do.");
            return Ok(());
        }
    };

    let state_dir = consumer_root.join(LINK_STATE_DIR);
    let backup_dir = state_dir.join(LINK_BACKUP_DIR);

    // Pass 1 — classify every file (tri-state per file) and verify every
    // needed backup BEFORE mutating anything, so a refusal or a corrupt
    // backup leaves the tree untouched.
    let mut actions: Vec<(&LinkedManifestFile, RestoreAction)> = Vec::new();
    for tf in manifest.files.iter().rev() {
        let abs = consumer_root.join(&tf.path);
        let live = read_optional_bytes(&abs)?;
        let live_hash = live.as_deref().map(hex_sha256);

        let action = if tf.existed_before {
            if live_hash.as_deref() == Some(tf.pre_edit_sha256.as_str()) {
                // Already the original (e.g. crash before this edit applied).
                RestoreAction::Skip
            } else {
                let is_linked_content = live_hash.as_deref() == Some(tf.post_edit_sha256.as_str());
                if !(live.is_none() || is_linked_content || force) {
                    return Err(LinkError::UnlinkRefusedModifiedFile { path: abs.clone() });
                }
                let backup = backup_dir.join(&tf.path);
                let bytes = std::fs::read(&backup).map_err(|e| LinkError::io(&backup, e))?;
                // Integrity guard: the backup must still hash to the value
                // recorded at link time — never restore corrupted bytes.
                if hex_sha256(&bytes) != tf.pre_edit_sha256 {
                    return Err(LinkError::CorruptLinkState {
                        path: backup.clone(),
                        detail: format!(
                            "backup content hash does not match the pre-edit hash recorded for \
                             `{}`; refusing to restore a corrupted backup",
                            tf.path.display()
                        ),
                    });
                }
                RestoreAction::RestoreOriginal(bytes)
            }
        } else {
            match live_hash.as_deref() {
                None => RestoreAction::Skip,
                Some(h) if h == tf.post_edit_sha256 => RestoreAction::RemoveCreated,
                Some(_) if force => RestoreAction::RemoveCreated,
                Some(_) => {
                    return Err(LinkError::UnlinkRefusedModifiedFile { path: abs.clone() });
                }
            }
        };
        actions.push((tf, action));
    }

    // Pass 2 — apply the restores (already in reverse apply order).
    let mut restored = 0usize;
    for (tf, action) in actions {
        let abs = consumer_root.join(&tf.path);
        match action {
            RestoreAction::Skip => {}
            RestoreAction::RestoreOriginal(bytes) => {
                std::fs::write(&abs, &bytes).map_err(|e| LinkError::io(&abs, e))?;
                restored += 1;
            }
            RestoreAction::RemoveCreated => {
                std::fs::remove_file(&abs).map_err(|e| LinkError::io(&abs, e))?;
                prune_empty_parents(&abs, &consumer_root);
                restored += 1;
            }
        }
    }

    std::fs::remove_dir_all(&state_dir).map_err(|e| LinkError::io(&state_dir, e))?;
    println!("Unlinked streamlib — {restored} file(s) restored.");
    Ok(())
}

/// Print the active link status (or that none is active).
#[tracing::instrument(skip_all, fields(consumer_root = %consumer_root.display()))]
pub fn status(consumer_root: &Path) -> Result<(), LinkError> {
    let consumer_root = consumer_root
        .canonicalize()
        .map_err(|e| LinkError::io(consumer_root, e))?;
    match load_active_manifest(&consumer_root)? {
        None => println!("No active streamlib link."),
        Some(m) => {
            println!("streamlib linked → {}", m.checkout.display());
            println!("  Linked at: {}", m.linked_at);
            println!(
                "  State: {}",
                match m.state {
                    LinkTransactionState::Active => "active",
                    LinkTransactionState::Applying =>
                        "applying (torn — run `streamlib unlink` to restore)",
                }
            );
            println!("  Cargo crates redirected: {}", m.linked_crate_count);
            for tf in &m.files {
                println!("  Overrode: {}", tf.path.display());
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn canonicalize_checkout(checkout: &Path) -> Result<PathBuf, LinkError> {
    let canonical = checkout
        .canonicalize()
        .map_err(|_| LinkError::NotAStreamlibCheckout(checkout.to_path_buf()))?;
    if !canonical.join("Cargo.toml").is_file() {
        return Err(LinkError::NotAStreamlibCheckout(canonical));
    }
    Ok(canonical)
}

/// Load the link manifest from `consumer_root/.streamlib/link.json` (any state).
fn load_active_manifest(consumer_root: &Path) -> Result<Option<LinkManifest>, LinkError> {
    let path = consumer_root.join(LINK_STATE_DIR).join(LINK_MANIFEST_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    Ok(Some(streamlib_idents::link_marker::load_link_manifest(
        &path,
    )?))
}

fn manifest_json(manifest: &LinkManifest, path: &Path) -> Result<String, LinkError> {
    serde_json::to_string_pretty(manifest).map_err(|e| LinkError::CorruptLinkState {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })
}

/// Create `link.json` with O_EXCL semantics — fails if the marker already
/// exists, closing the concurrent-link race.
fn write_manifest_excl(consumer_root: &Path, manifest: &LinkManifest) -> Result<(), LinkError> {
    use std::io::Write;
    let state_dir = consumer_root.join(LINK_STATE_DIR);
    std::fs::create_dir_all(&state_dir).map_err(|e| LinkError::io(&state_dir, e))?;
    let path = state_dir.join(LINK_MANIFEST_FILE);
    let body = manifest_json(manifest, &path)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                LinkError::LinkMarkerAlreadyExists { path: path.clone() }
            } else {
                LinkError::io(&path, e)
            }
        })?;
    file.write_all(body.as_bytes())
        .map_err(|e| LinkError::io(&path, e))
}

/// Overwrite `link.json` in place (used for the `applying → active` flip).
fn overwrite_manifest(consumer_root: &Path, manifest: &LinkManifest) -> Result<(), LinkError> {
    let path = consumer_root.join(LINK_STATE_DIR).join(LINK_MANIFEST_FILE);
    let body = manifest_json(manifest, &path)?;
    std::fs::write(&path, body).map_err(|e| LinkError::io(&path, e))
}

/// Run `cargo metadata` and parse its JSON output.
fn run_cargo_metadata(args: &[&str], cwd: Option<&Path>) -> Result<serde_json::Value, String> {
    let mut cmd = std::process::Command::new("cargo");
    cmd.args(args);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let output = cmd
        .output()
        .map_err(|e| format!("failed to run cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("cargo metadata is not valid JSON: {e}"))
}

/// Run a `cargo` subcommand for its exit status only (no JSON), returning a
/// diagnostic string on failure.
fn run_cargo_status(args: &[&str], cwd: Option<&Path>) -> Result<(), String> {
    let mut cmd = std::process::Command::new("cargo");
    cmd.args(args);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let output = cmd
        .output()
        .map_err(|e| format!("failed to run cargo {}: {e}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "cargo {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Derive the linkable crate set (`name` → checkout member dir) from the
/// checkout via the single canonical release-closure definition
/// ([`streamlib_pack::compute_release_closure`]): every publishable workspace
/// library crate named `streamlib*` / `vulkan-jpeg`. This is the exact set a
/// release publishes, so a whole-tree link and a release always agree on the
/// crate set by construction.
fn derive_linkable_crates(checkout: &Path) -> Result<BTreeMap<String, PathBuf>, LinkError> {
    let closure = streamlib_pack::compute_release_closure(checkout).map_err(|e| {
        LinkError::CrateSetDerivation {
            checkout: checkout.to_path_buf(),
            detail: format!("{e}"),
        }
    })?;
    Ok(closure
        .crates
        .into_iter()
        .map(|c| (c.name, c.manifest_dir))
        .collect())
}

/// One streamlib crate that resolves from the registry instead of the checkout
/// — i.e. failed to redirect through the link's `[patch]`.
#[derive(Debug, Clone)]
struct UnpatchedCrate {
    name: String,
    version: String,
}

/// Verify the consumer's cargo resolution honors the link: every resolved
/// `streamlib*` / `vulkan-jpeg` package must be a path source under the
/// checkout. Returns the crates that still resolve from the registry (empty ⇒
/// fully patched); `Err(detail)` when cargo can't resolve the graph at all.
/// `Ok(vec![])` (vacuously) when the consumer has no `Cargo.toml`.
fn verify_cargo_patch_resolution(
    consumer_root: &Path,
    checkout: &Path,
    crates: &BTreeMap<String, PathBuf>,
) -> Result<Vec<UnpatchedCrate>, String> {
    if !consumer_root.join("Cargo.toml").is_file() {
        return Ok(Vec::new());
    }

    // Offline first (the whole point of link mode); fall back to online when
    // offline resolution fails for unrelated reasons (cold cache).
    let md = match run_cargo_metadata(
        &["metadata", "--format-version", "1", "--offline"],
        Some(consumer_root),
    ) {
        Ok(md) => md,
        Err(offline_err) => {
            run_cargo_metadata(&["metadata", "--format-version", "1"], Some(consumer_root))
                .map_err(|online_err| {
                    format!(
                        "cargo could not resolve the consumer's dependency graph — offline: \
                 {offline_err}; online: {online_err}"
                    )
                })?
        }
    };

    let empty = Vec::new();
    let packages = md
        .get("packages")
        .and_then(|p| p.as_array())
        .unwrap_or(&empty);
    let mut verified = 0usize;
    let mut not_patched: Vec<UnpatchedCrate> = Vec::new();
    for pkg in packages {
        let name = pkg.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        if !crates.contains_key(name) {
            continue;
        }
        let is_path_source = pkg.get("source").is_none_or(|s| s.is_null());
        let at_checkout = pkg
            .get("manifest_path")
            .and_then(|v| v.as_str())
            .is_some_and(|mp| Path::new(mp).starts_with(checkout));
        if is_path_source && at_checkout {
            verified += 1;
        } else {
            let version = pkg.get("version").and_then(|v| v.as_str()).unwrap_or("?");
            not_patched.push(UnpatchedCrate {
                name: name.to_string(),
                version: version.to_string(),
            });
        }
    }

    tracing::info!(
        verified,
        unpatched = not_patched.len(),
        "post-link cargo resolution checked"
    );
    Ok(not_patched)
}

/// Result of verifying (and, when a stale lock is at fault, remedying) that the
/// link's `[patch]` took effect in the consumer's cargo resolution.
enum VerifyOutcome {
    /// The `[patch]` resolves. `relocked` is the number of crates whose lock
    /// entries were transparently refreshed to make it take effect (0 ⇒ the
    /// patch was honored without touching the lock).
    Verified { relocked: usize },
    /// The `[patch]` is genuinely ignored (version-requirement mismatch) or the
    /// graph is unresolvable — the caller rolls the whole link back.
    RollBack { detail: String },
    /// A stale lock defeats the `[patch]` and the automatic re-lock could not
    /// run — the caller leaves the link applied and surfaces the exact command.
    RelockUnavailable {
        lockfile: PathBuf,
        crates: String,
        command: String,
        detail: String,
    },
}

/// Verify the link's `[patch]` resolves; if a pre-existing `Cargo.lock` defeats
/// it, transparently re-lock exactly the crates that failed to redirect and
/// re-verify. The re-lock mutates `Cargo.lock`, which is recorded as a
/// link-managed file (`record_relocked_lockfile`) so `unlink` restores it
/// byte-identically. Returns `Err` only for a filesystem failure while
/// recording; every resolution outcome is a [`VerifyOutcome`].
fn verify_and_remedy_patch_resolution(
    consumer_root: &Path,
    checkout: &Path,
    crates: &BTreeMap<String, PathBuf>,
    original_cargo_lock: Option<Vec<u8>>,
) -> Result<VerifyOutcome, LinkError> {
    let unpatched = match verify_cargo_patch_resolution(consumer_root, checkout, crates) {
        Ok(u) => u,
        Err(detail) => return Ok(VerifyOutcome::RollBack { detail }),
    };
    if unpatched.is_empty() {
        return Ok(VerifyOutcome::Verified { relocked: 0 });
    }

    let lockfile = consumer_root.join("Cargo.lock");
    if !lockfile.is_file() {
        // No lock to blame — the checkout's versions don't satisfy the
        // consumer's requirements, so cargo ignored the [patch].
        return Ok(VerifyOutcome::RollBack {
            detail: format!(
                "these streamlib crates resolve from the registry, not the checkout — the \
                 checkout's versions don't satisfy the consumer's version requirements: {}",
                render_unpatched(&unpatched)
            ),
        });
    }

    // Stale lock: cargo honored the existing lock over the new [patch]. Refresh
    // exactly the crates that failed to redirect so the patch takes effect.
    let targets: Vec<&str> = unpatched.iter().map(|c| c.name.as_str()).collect();
    let command = relock_command_string(consumer_root, &targets);
    tracing::info!(
        crates = %targets.join(", "),
        "consumer Cargo.lock defeats the [patch]; re-locking to make it take effect"
    );
    if let Err(detail) = relock_streamlib_crates(consumer_root, &targets) {
        return Ok(VerifyOutcome::RelockUnavailable {
            lockfile,
            crates: targets.join(", "),
            command,
            detail,
        });
    }

    // Record the re-locked Cargo.lock before re-verifying, so a roll-back on the
    // (rare) still-unresolved path also restores the lock byte-identically.
    record_relocked_lockfile(consumer_root, &lockfile, original_cargo_lock)?;

    match verify_cargo_patch_resolution(consumer_root, checkout, crates) {
        Ok(still) if still.is_empty() => Ok(VerifyOutcome::Verified {
            relocked: targets.len(),
        }),
        Ok(still) => Ok(VerifyOutcome::RollBack {
            detail: format!(
                "after re-locking the consumer's Cargo.lock, these streamlib crates still \
                 resolve from the registry — the checkout's versions don't satisfy the \
                 consumer's version requirements: {}",
                render_unpatched(&still)
            ),
        }),
        Err(detail) => Ok(VerifyOutcome::RollBack { detail }),
    }
}

/// Render an unpatched-crate list as `name@version, name@version` for messages.
fn render_unpatched(crates: &[UnpatchedCrate]) -> String {
    crates
        .iter()
        .map(|c| format!("{}@{}", c.name, c.version))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The exact `cargo update` command that re-locks `targets` for the consumer —
/// surfaced to the operator when the automatic re-lock can't run.
fn relock_command_string(consumer_root: &Path, targets: &[&str]) -> String {
    let mut s = format!(
        "cargo update --manifest-path {}",
        consumer_root.join("Cargo.toml").display()
    );
    for t in targets {
        s.push_str(" -p ");
        s.push_str(t);
    }
    s
}

/// Re-lock exactly `targets` in the consumer's `Cargo.lock` so the link's
/// `[patch]` takes effect. `cargo update -p <name>` re-resolves that package,
/// which redirects it to the patched path source. Offline first (link mode is
/// offline-by-design); fall back to online when offline resolution can't run.
fn relock_streamlib_crates(consumer_root: &Path, targets: &[&str]) -> Result<(), String> {
    let mut base: Vec<&str> = vec!["update"];
    for t in targets {
        base.push("-p");
        base.push(t);
    }
    let mut offline = base.clone();
    offline.push("--offline");
    match run_cargo_status(&offline, Some(consumer_root)) {
        Ok(()) => Ok(()),
        Err(offline_err) => run_cargo_status(&base, Some(consumer_root))
            .map_err(|online_err| format!("offline: {offline_err}; online: {online_err}")),
    }
}

/// Record the transparently re-locked `Cargo.lock` as a link-managed file so
/// `unlink` restores it byte-identically. Backs the pre-link bytes up (when the
/// lock existed) and appends a [`LinkedManifestFile`] entry to the persisted
/// manifest. When the lock did not exist pre-link, the entry is `!existed_before`
/// so `unlink` removes the file the link created.
fn record_relocked_lockfile(
    consumer_root: &Path,
    lockfile: &Path,
    original: Option<Vec<u8>>,
) -> Result<(), LinkError> {
    let current = std::fs::read(lockfile).map_err(|e| LinkError::io(lockfile, e))?;
    let backup_dir = consumer_root.join(LINK_STATE_DIR).join(LINK_BACKUP_DIR);
    std::fs::create_dir_all(&backup_dir).map_err(|e| LinkError::io(&backup_dir, e))?;
    if let Some(orig) = &original {
        let backup = backup_dir.join("Cargo.lock");
        std::fs::write(&backup, orig).map_err(|e| LinkError::io(&backup, e))?;
    }

    let mut manifest =
        load_active_manifest(consumer_root)?.ok_or_else(|| LinkError::CorruptLinkState {
            path: consumer_root.join(LINK_STATE_DIR).join(LINK_MANIFEST_FILE),
            detail: "link manifest missing while recording the re-locked Cargo.lock".to_string(),
        })?;
    manifest.files.push(LinkedManifestFile {
        path: PathBuf::from("Cargo.lock"),
        existed_before: original.is_some(),
        pre_edit_sha256: original.as_deref().map(hex_sha256).unwrap_or_default(),
        post_edit_sha256: hex_sha256(&current),
    });
    overwrite_manifest(consumer_root, &manifest)
}

/// Run the full link transaction against a computed crate set: plan every
/// edit, persist the plan as `link.json` (state `applying`, O_EXCL), apply the
/// edits with backups, then flip the state to `active`. Returns the applied
/// edits for reporting.
fn establish_link(
    consumer_root: &Path,
    checkout: &Path,
    crates: &BTreeMap<String, PathBuf>,
) -> Result<Vec<PlannedEdit>, LinkError> {
    let edits = plan_edits(consumer_root, checkout, crates)?;
    let mut manifest = build_link_manifest(checkout, crates.len(), &edits);

    // Manifest-first: the full plan is on disk (O_EXCL) before any edit, so a
    // crash at any later point is recoverable by a plain `streamlib unlink`,
    // and a concurrent second `link` run fails the exclusive create.
    write_manifest_excl(consumer_root, &manifest)?;

    if let Err(e) = apply_transaction(consumer_root, &edits) {
        return Err(unwind_failed_transaction(consumer_root, &edits, e));
    }

    manifest.state = LinkTransactionState::Active;
    if let Err(e) = overwrite_manifest(consumer_root, &manifest) {
        return Err(unwind_failed_transaction(consumer_root, &edits, e));
    }

    Ok(edits)
}

/// Build the persisted link manifest (state `applying`) from the edit plan.
fn build_link_manifest(
    checkout: &Path,
    linked_crate_count: usize,
    edits: &[PlannedEdit],
) -> LinkManifest {
    let files: Vec<LinkedManifestFile> = edits
        .iter()
        .map(|edit| LinkedManifestFile {
            path: edit.rel_path.clone(),
            existed_before: edit.original.is_some(),
            pre_edit_sha256: edit.original.as_deref().map(hex_sha256).unwrap_or_default(),
            post_edit_sha256: hex_sha256(edit.new_content.as_bytes()),
        })
        .collect();
    LinkManifest {
        checkout: checkout.to_path_buf(),
        python_sdk_path: checkout.join(PYTHON_SDK_REL),
        deno_sdk_entrypoint_path: checkout.join(DENO_SDK_ENTRYPOINT_REL),
        linked_at: chrono::Utc::now().to_rfc3339(),
        linked_crate_count,
        state: LinkTransactionState::Applying,
        files,
    }
}

/// Roll back a failed link transaction. Removes the link state only when the
/// verified rollback restored every file; otherwise the state dir (manifest +
/// backups) is left intact for `streamlib unlink` recovery and the error says
/// so.
fn unwind_failed_transaction(
    consumer_root: &Path,
    edits: &[PlannedEdit],
    cause: LinkError,
) -> LinkError {
    let refs: Vec<&PlannedEdit> = edits.iter().collect();
    if rollback(&refs) {
        let _ = std::fs::remove_dir_all(consumer_root.join(LINK_STATE_DIR));
        cause
    } else {
        LinkError::RollbackIncomplete {
            backup_dir: consumer_root.join(LINK_STATE_DIR).join(LINK_BACKUP_DIR),
            detail: format!("original failure: {cause}"),
        }
    }
}

/// Compute every planned edit (cargo config always; pyproject / deno.json only
/// when the consumer has one).
fn plan_edits(
    consumer_root: &Path,
    checkout: &Path,
    crates: &BTreeMap<String, PathBuf>,
) -> Result<Vec<PlannedEdit>, LinkError> {
    let mut edits = Vec::new();

    // 1. Cargo config `[patch.crates-io]` — always emitted.
    edits.push(plan_cargo_config_edit(consumer_root, crates)?);

    // 2. Python SDK via `[tool.uv.sources]` — only when the consumer has a pyproject.
    let pyproject = consumer_root.join("pyproject.toml");
    if pyproject.is_file() {
        edits.push(plan_pyproject_edit(&pyproject, consumer_root, checkout)?);
    }

    // 3. Deno SDK import rewrite — only when the consumer has a deno.json(c).
    for name in ["deno.json", "deno.jsonc"] {
        let deno = consumer_root.join(name);
        if deno.is_file() {
            edits.push(plan_deno_edit(&deno, consumer_root, checkout)?);
            break;
        }
    }

    Ok(edits)
}

fn plan_cargo_config_edit(
    consumer_root: &Path,
    crates: &BTreeMap<String, PathBuf>,
) -> Result<PlannedEdit, LinkError> {
    let abs_path = consumer_root.join(".cargo").join("config.toml");
    let rel_path = PathBuf::from(".cargo").join("config.toml");
    let original = read_optional_bytes(&abs_path)?;

    let existing = match &original {
        Some(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        None => String::new(),
    };
    let mut doc: toml_edit::DocumentMut =
        existing
            .parse()
            .map_err(|e: toml_edit::TomlError| LinkError::ManifestParse {
                path: abs_path.clone(),
                detail: e.to_string(),
            })?;

    // Build `[patch.crates-io]` with one path entry per crate. `patch` is an
    // implicit parent table so only the `[patch.crates-io]` header is emitted.
    let mut patch_target = toml_edit::Table::new();
    for (name, dir) in crates {
        let mut entry = toml_edit::InlineTable::new();
        entry.insert(
            "path",
            toml_edit::Value::from(dir.to_string_lossy().into_owned()),
        );
        patch_target.insert(
            name,
            toml_edit::Item::Value(toml_edit::Value::InlineTable(entry)),
        );
    }
    patch_target
        .decor_mut()
        .set_prefix(format!("\n{CARGO_PATCH_MARKER}\n"));

    if !doc.contains_key("patch") {
        let mut patch = toml_edit::Table::new();
        patch.set_implicit(true);
        doc.insert("patch", toml_edit::Item::Table(patch));
    }
    let patch = doc["patch"]
        .as_table_mut()
        .ok_or_else(|| LinkError::ManifestParse {
            path: abs_path.clone(),
            detail: "`patch` exists but is not a table".to_string(),
        })?;
    patch.insert(CARGO_PATCH_SOURCE, toml_edit::Item::Table(patch_target));

    Ok(PlannedEdit {
        rel_path,
        abs_path,
        new_content: doc.to_string(),
        original,
    })
}

/// Get-or-create `key` as a table on `table`, with a typed error when an
/// existing non-table value occupies the key.
fn ensure_table_mut<'a>(
    table: &'a mut toml_edit::Table,
    key: &str,
    file: &Path,
) -> Result<&'a mut toml_edit::Table, LinkError> {
    if !table.contains_key(key) {
        table.insert(key, toml_edit::Item::Table(toml_edit::Table::new()));
    }
    table[key]
        .as_table_mut()
        .ok_or_else(|| LinkError::ManifestParse {
            path: file.to_path_buf(),
            detail: format!("`{key}` exists but is not a table"),
        })
}

fn plan_pyproject_edit(
    pyproject: &Path,
    consumer_root: &Path,
    checkout: &Path,
) -> Result<PlannedEdit, LinkError> {
    let original = read_optional_bytes(pyproject)?;
    let existing = match &original {
        Some(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        None => String::new(),
    };
    let mut doc: toml_edit::DocumentMut =
        existing
            .parse()
            .map_err(|e: toml_edit::TomlError| LinkError::ManifestParse {
                path: pyproject.to_path_buf(),
                detail: e.to_string(),
            })?;

    // [tool.uv.sources] streamlib = { path = "<checkout>/libs/streamlib-python", editable = true }
    let sdk_path = checkout.join(PYTHON_SDK_REL);
    let mut source = toml_edit::InlineTable::new();
    source.insert(
        "path",
        toml_edit::Value::from(sdk_path.to_string_lossy().into_owned()),
    );
    source.insert("editable", toml_edit::Value::from(true));

    let tool = ensure_table_mut(doc.as_table_mut(), "tool", pyproject)?;
    let uv = ensure_table_mut(tool, "uv", pyproject)?;
    let sources = ensure_table_mut(uv, "sources", pyproject)?;
    sources.insert(
        "streamlib",
        toml_edit::Item::Value(toml_edit::Value::InlineTable(source)),
    );

    Ok(PlannedEdit {
        rel_path: rel_of(pyproject, consumer_root),
        abs_path: pyproject.to_path_buf(),
        new_content: doc.to_string(),
        original,
    })
}

fn plan_deno_edit(
    deno: &Path,
    consumer_root: &Path,
    checkout: &Path,
) -> Result<PlannedEdit, LinkError> {
    let original = read_optional_bytes(deno)?;
    let body = match &original {
        Some(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        None => String::new(),
    };
    let mut value: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| LinkError::ManifestParse {
            path: deno.to_path_buf(),
            detail: format!(
                "{e} (deno.jsonc with comments is not supported for link override; use plain JSON)"
            ),
        })?;
    if !value.is_object() {
        value = serde_json::json!({});
    }
    let entry = checkout.join(DENO_SDK_ENTRYPOINT_REL);
    let obj = value.as_object_mut().unwrap();
    let imports = obj
        .entry("imports")
        .or_insert_with(|| serde_json::json!({}));
    if !imports.is_object() {
        *imports = serde_json::json!({});
    }
    imports.as_object_mut().unwrap().insert(
        "streamlib".to_string(),
        serde_json::Value::String(entry.to_string_lossy().into_owned()),
    );

    let mut new_content =
        serde_json::to_string_pretty(&value).map_err(|e| LinkError::ManifestParse {
            path: deno.to_path_buf(),
            detail: e.to_string(),
        })?;
    new_content.push('\n');

    Ok(PlannedEdit {
        rel_path: rel_of(deno, consumer_root),
        abs_path: deno.to_path_buf(),
        new_content,
        original,
    })
}

/// Apply every planned edit: back up pre-existing files, then write the new
/// content. The caller (`establish_link`) owns rollback on failure — the plan
/// is already persisted in `link.json` (state `applying`) before this runs.
fn apply_transaction(consumer_root: &Path, edits: &[PlannedEdit]) -> Result<(), LinkError> {
    let backup_dir = consumer_root.join(LINK_STATE_DIR).join(LINK_BACKUP_DIR);
    std::fs::create_dir_all(&backup_dir).map_err(|e| LinkError::io(&backup_dir, e))?;

    for edit in edits {
        if let Some(orig) = &edit.original {
            let backup = backup_dir.join(&edit.rel_path);
            if let Some(parent) = backup.parent() {
                std::fs::create_dir_all(parent).map_err(|e| LinkError::io(parent, e))?;
            }
            std::fs::write(&backup, orig).map_err(|e| LinkError::io(&backup, e))?;
        }
        if let Some(parent) = edit.abs_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LinkError::io(parent, e))?;
        }
        std::fs::write(&edit.abs_path, &edit.new_content)
            .map_err(|e| LinkError::io(&edit.abs_path, e))?;
    }
    Ok(())
}

/// Undo edits in reverse order and VERIFY each outcome: an `existed_before`
/// file must read back byte-identical to its original, a created file must be
/// gone. Returns `true` only when every file verifiably reached its pre-link
/// state.
fn rollback(applied: &[&PlannedEdit]) -> bool {
    let mut ok = true;
    for edit in applied.iter().rev() {
        match &edit.original {
            Some(orig) => {
                let _ = std::fs::write(&edit.abs_path, orig);
                ok &= std::fs::read(&edit.abs_path)
                    .map(|live| &live == orig)
                    .unwrap_or(false);
            }
            None => {
                let _ = std::fs::remove_file(&edit.abs_path);
                ok &= !edit.abs_path.exists();
            }
        }
    }
    ok
}

/// Remove now-empty parent directories of `removed_file` up to (but excluding)
/// `stop_at`. Best-effort — a non-empty dir halts the walk.
fn prune_empty_parents(removed_file: &Path, stop_at: &Path) {
    let mut dir = removed_file.parent();
    while let Some(d) = dir {
        if d == stop_at || !d.starts_with(stop_at) {
            break;
        }
        // `remove_dir` only succeeds on an empty directory.
        if std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

fn read_optional_bytes(path: &Path) -> Result<Option<Vec<u8>>, LinkError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(LinkError::io(path, e)),
    }
}

fn rel_of(abs: &Path, root: &Path) -> PathBuf {
    abs.strip_prefix(root).unwrap_or(abs).to_path_buf()
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests;
