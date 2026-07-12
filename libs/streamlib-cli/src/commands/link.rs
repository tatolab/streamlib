// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib link` / `streamlib unlink` — whole-tree local dev override.
//!
//! Points the entire streamlib surface a consumer resolves (all SDK cargo
//! crates + the Python SDK + the Deno SDK) at a local streamlib checkout via
//! language-native overrides, so an edit in the checkout is picked up by the
//! consumer's next build with no publish step. `unlink` restores every touched
//! manifest byte-identically. Emission is whole-tree and transactional: either
//! all overrides land or none do.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Consumer-root-relative directory holding all link state.
const LINK_STATE_DIR: &str = ".streamlib";
/// Manifest recording the active link (checkout + every touched file).
const LINK_MANIFEST_FILE: &str = "link.json";
/// Directory under [`LINK_STATE_DIR`] mirroring pre-edit backups by relative path.
const LINK_BACKUP_DIR: &str = "link-backup";
/// Human-facing greppability marker on the emitted cargo `[patch]` block.
const CARGO_PATCH_MARKER: &str =
    "# streamlib-link — managed by `streamlib link`; removed by `streamlib unlink`";
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

    /// No `[registries.gitea].index` is discoverable from the consumer's cargo config.
    #[error(
        "no `[registries.gitea]` registry index found in this consumer's cargo config (looked in \
         `.cargo/config.toml` walking up from the consumer root and in `~/.cargo/config.toml`); a \
         streamlib consumer must configure the gitea registry before linking"
    )]
    RegistryIndexNotConfigured,

    /// `cargo metadata` on the checkout failed or produced no linkable crates.
    #[error("could not derive the linkable crate set from `{checkout}`: {detail}")]
    CrateSetDerivation { checkout: PathBuf, detail: String },

    /// A manifest we planned to edit could not be parsed.
    #[error("failed to parse `{path}`: {detail}")]
    ManifestParse { path: PathBuf, detail: String },

    /// The recorded link manifest is corrupt / unreadable.
    #[error("link state at `{path}` is corrupt: {detail}")]
    CorruptLinkState { path: PathBuf, detail: String },

    /// A pack/publish was attempted while a link is active.
    #[error(
        "this package cannot be packed or published while a streamlib link is active (marker: \
         {marker}). Local link overrides are dev-only and must not leak into a distributed \
         artifact — run `streamlib unlink` first"
    )]
    PackRefusedWhileLinked { marker: PathBuf },

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

/// One manifest file the active link overrode, recorded for byte-clean teardown.
#[derive(Debug, Serialize, Deserialize)]
struct TouchedFile {
    /// Path relative to the consumer root.
    path: PathBuf,
    /// Whether the file existed before linking. `false` ⇒ unlink deletes it.
    existed_before: bool,
    /// Hex SHA-256 of the pre-edit content (empty when `!existed_before`).
    pre_edit_sha256: String,
}

/// Persisted record of the active whole-tree link.
#[derive(Debug, Serialize, Deserialize)]
struct LinkManifest {
    /// Canonicalized path of the linked streamlib checkout.
    checkout: PathBuf,
    /// RFC-3339 timestamp of when the link was established.
    linked_at: String,
    /// Number of cargo crates redirected to the checkout.
    linked_crate_count: usize,
    /// Every manifest file the link touched (in apply order).
    files: Vec<TouchedFile>,
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
#[tracing::instrument(skip_all, fields(consumer_root = %consumer_root.display(), checkout = %checkout.display()))]
pub fn link(consumer_root: &Path, checkout: &Path) -> Result<(), LinkError> {
    let checkout = canonicalize_checkout(checkout)?;
    let consumer_root = consumer_root
        .canonicalize()
        .map_err(|e| LinkError::io(consumer_root, e))?;

    // Idempotency: identical checkout ⇒ refresh; different checkout ⇒ refuse.
    if let Some(existing) = load_active_manifest(&consumer_root)? {
        if existing.checkout == checkout {
            tracing::info!("refreshing existing link (same checkout) — re-deriving crate set");
            println!(
                "Refreshing active link to {} (re-deriving overrides).",
                checkout.display()
            );
            unlink(&consumer_root)?;
        } else {
            return Err(LinkError::AlreadyLinkedElsewhere {
                active: existing.checkout,
                requested: checkout,
            });
        }
    }

    let index_url = discover_registry_index(&consumer_root)?;
    let crates = derive_linkable_crates(&checkout)?;
    if crates.is_empty() {
        return Err(LinkError::CrateSetDerivation {
            checkout: checkout.clone(),
            detail: "no workspace members matched the streamlib linkable set".into(),
        });
    }

    let edits = plan_edits(&consumer_root, &checkout, &index_url, &crates)?;
    let touched = apply_transaction(&consumer_root, &edits)?;

    write_manifest(
        &consumer_root,
        &LinkManifest {
            checkout: checkout.clone(),
            linked_at: chrono::Utc::now().to_rfc3339(),
            linked_crate_count: crates.len(),
            files: touched,
        },
    )?;

    println!("Linked streamlib → {}", checkout.display());
    println!("  Cargo crates redirected: {}", crates.len());
    for edit in &edits {
        println!("  Overrode: {}", edit.rel_path.display());
    }
    println!("Run `streamlib unlink` to restore.");
    Ok(())
}

/// Tear down the active link, restoring every touched manifest byte-identically.
#[tracing::instrument(skip_all, fields(consumer_root = %consumer_root.display()))]
pub fn unlink(consumer_root: &Path) -> Result<(), LinkError> {
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

    // Restore in reverse apply order so partial states unwind predictably.
    for tf in manifest.files.iter().rev() {
        let abs = consumer_root.join(&tf.path);
        if tf.existed_before {
            let backup = backup_dir.join(&tf.path);
            let bytes = std::fs::read(&backup).map_err(|e| LinkError::io(&backup, e))?;
            std::fs::write(&abs, &bytes).map_err(|e| LinkError::io(&abs, e))?;
        } else {
            // We created this file — remove it, then prune any now-empty parent
            // dirs we introduced (e.g. `.cargo/`), up to the consumer root.
            if abs.exists() {
                std::fs::remove_file(&abs).map_err(|e| LinkError::io(&abs, e))?;
            }
            prune_empty_parents(&abs, &consumer_root);
        }
    }

    std::fs::remove_dir_all(&state_dir).map_err(|e| LinkError::io(&state_dir, e))?;
    println!("Unlinked streamlib — {} file(s) restored.", manifest.files.len());
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
            println!("  Cargo crates redirected: {}", m.linked_crate_count);
            for tf in &m.files {
                println!("  Overrode: {}", tf.path.display());
            }
        }
    }
    Ok(())
}

/// Walk up from `start` to the filesystem root looking for an active link
/// marker (`.streamlib/link.json`). Returns the marker path when found.
pub fn find_active_link_marker(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let marker = d.join(LINK_STATE_DIR).join(LINK_MANIFEST_FILE);
        if marker.is_file() {
            return Some(marker);
        }
        dir = d.parent();
    }
    None
}

/// Refuse a pack/publish while a link is active anywhere above `package_dir`.
pub fn ensure_no_active_link_for_pack(package_dir: &Path) -> Result<(), LinkError> {
    if let Some(marker) = find_active_link_marker(package_dir) {
        return Err(LinkError::PackRefusedWhileLinked { marker });
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

/// Load the active link manifest from `consumer_root/.streamlib/link.json`.
fn load_active_manifest(consumer_root: &Path) -> Result<Option<LinkManifest>, LinkError> {
    let path = consumer_root
        .join(LINK_STATE_DIR)
        .join(LINK_MANIFEST_FILE);
    if !path.is_file() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(&path).map_err(|e| LinkError::io(&path, e))?;
    let manifest: LinkManifest = serde_json::from_str(&body).map_err(|e| {
        LinkError::CorruptLinkState {
            path: path.clone(),
            detail: e.to_string(),
        }
    })?;
    Ok(Some(manifest))
}

fn write_manifest(consumer_root: &Path, manifest: &LinkManifest) -> Result<(), LinkError> {
    let state_dir = consumer_root.join(LINK_STATE_DIR);
    std::fs::create_dir_all(&state_dir).map_err(|e| LinkError::io(&state_dir, e))?;
    let path = state_dir.join(LINK_MANIFEST_FILE);
    let body = serde_json::to_string_pretty(manifest).map_err(|e| LinkError::CorruptLinkState {
        path: path.clone(),
        detail: e.to_string(),
    })?;
    std::fs::write(&path, body).map_err(|e| LinkError::io(&path, e))
}

/// Discover the consumer's effective `[registries.gitea].index` string the way
/// cargo does: closest `.cargo/config.toml` (walking up) wins, then
/// `~/.cargo/config.toml`.
fn discover_registry_index(consumer_root: &Path) -> Result<String, LinkError> {
    let mut dir = Some(consumer_root);
    while let Some(d) = dir {
        for name in [".cargo/config.toml", ".cargo/config"] {
            let candidate = d.join(name);
            if let Some(idx) = read_gitea_index(&candidate)? {
                return Ok(idx);
            }
        }
        dir = d.parent();
    }
    if let Some(home) = dirs::home_dir() {
        for name in [".cargo/config.toml", ".cargo/config"] {
            if let Some(idx) = read_gitea_index(&home.join(name))? {
                return Ok(idx);
            }
        }
    }
    Err(LinkError::RegistryIndexNotConfigured)
}

/// Read `registries.gitea.index` from a cargo config file, if present.
fn read_gitea_index(path: &Path) -> Result<Option<String>, LinkError> {
    if !path.is_file() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(path).map_err(|e| LinkError::io(path, e))?;
    let doc: toml::Value = toml::from_str(&body).map_err(|e| LinkError::ManifestParse {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;
    Ok(doc
        .get("registries")
        .and_then(|r| r.get("gitea"))
        .and_then(|g| g.get("index"))
        .and_then(|i| i.as_str())
        .map(|s| s.to_string()))
}

/// Derive the linkable crate set (`name` → checkout-relative member dir) from
/// the checkout live via `cargo metadata` — same selection as the publish
/// closure with `STREAMLIB_PUBLISH_ALL_LIBS=1`: every workspace member whose
/// name starts with `streamlib` (or is `vulkan-jpeg`), that produces a library
/// target and is publishable.
fn derive_linkable_crates(checkout: &Path) -> Result<BTreeMap<String, PathBuf>, LinkError> {
    let manifest_path = checkout.join("Cargo.toml");
    let output = std::process::Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps", "--manifest-path"])
        .arg(&manifest_path)
        .output()
        .map_err(|e| LinkError::CrateSetDerivation {
            checkout: checkout.to_path_buf(),
            detail: format!("failed to run cargo metadata: {e}"),
        })?;
    if !output.status.success() {
        return Err(LinkError::CrateSetDerivation {
            checkout: checkout.to_path_buf(),
            detail: format!(
                "cargo metadata failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    let md: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| LinkError::CrateSetDerivation {
            checkout: checkout.to_path_buf(),
            detail: format!("cargo metadata is not valid JSON: {e}"),
        })?;

    let members: std::collections::HashSet<&str> = md
        .get("workspace_members")
        .and_then(|m| m.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let mut crates = BTreeMap::new();
    let empty = Vec::new();
    let packages = md
        .get("packages")
        .and_then(|p| p.as_array())
        .unwrap_or(&empty);
    for pkg in packages {
        let id = pkg.get("id").and_then(|v| v.as_str()).unwrap_or_default();
        if !members.contains(id) {
            continue;
        }
        let name = pkg.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        if !(name.starts_with("streamlib") || name == "vulkan-jpeg") {
            continue;
        }
        // `publish == []` ⇒ publish = false ⇒ not a linkable SDK crate.
        if pkg.get("publish").and_then(|v| v.as_array()).is_some_and(|a| a.is_empty()) {
            continue;
        }
        if !has_library_target(pkg) {
            continue;
        }
        let manifest = pkg
            .get("manifest_path")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if let Some(dir) = Path::new(manifest).parent() {
            crates.insert(name.to_string(), dir.to_path_buf());
        }
    }
    Ok(crates)
}

fn has_library_target(pkg: &serde_json::Value) -> bool {
    const LIB_KINDS: &[&str] = &["lib", "rlib", "cdylib", "proc-macro", "dylib", "staticlib"];
    pkg.get("targets")
        .and_then(|t| t.as_array())
        .is_some_and(|targets| {
            targets.iter().any(|t| {
                t.get("kind")
                    .and_then(|k| k.as_array())
                    .is_some_and(|kinds| {
                        kinds
                            .iter()
                            .filter_map(|k| k.as_str())
                            .any(|k| LIB_KINDS.contains(&k))
                    })
            })
        })
}

/// Compute every planned edit (cargo config always; pyproject / deno.json only
/// when the consumer has one).
fn plan_edits(
    consumer_root: &Path,
    checkout: &Path,
    index_url: &str,
    crates: &BTreeMap<String, PathBuf>,
) -> Result<Vec<PlannedEdit>, LinkError> {
    let mut edits = Vec::new();

    // 1. Cargo config `[patch."<index>"]` — always emitted.
    edits.push(plan_cargo_config_edit(consumer_root, index_url, crates)?);

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
    index_url: &str,
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
        existing.parse().map_err(|e: toml_edit::TomlError| LinkError::ManifestParse {
            path: abs_path.clone(),
            detail: e.to_string(),
        })?;

    // Build `[patch."<index>"]` with one path entry per crate. `patch` is an
    // implicit parent table so only the `[patch."url"]` header is emitted.
    let mut patch_target = toml_edit::Table::new();
    for (name, dir) in crates {
        let mut entry = toml_edit::InlineTable::new();
        entry.insert("path", toml_edit::Value::from(dir.to_string_lossy().into_owned()));
        patch_target.insert(name, toml_edit::Item::Value(toml_edit::Value::InlineTable(entry)));
    }
    patch_target
        .decor_mut()
        .set_prefix(format!("\n{CARGO_PATCH_MARKER}\n"));

    if !doc.contains_key("patch") {
        let mut patch = toml_edit::Table::new();
        patch.set_implicit(true);
        doc.insert("patch", toml_edit::Item::Table(patch));
    }
    doc["patch"][index_url] = toml_edit::Item::Table(patch_target);

    Ok(PlannedEdit {
        rel_path,
        abs_path,
        new_content: doc.to_string(),
        original,
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
        existing.parse().map_err(|e: toml_edit::TomlError| LinkError::ManifestParse {
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

    ensure_table(&mut doc, "tool");
    ensure_subtable(doc["tool"].as_table_mut().unwrap(), "uv");
    let uv = doc["tool"]["uv"].as_table_mut().unwrap();
    ensure_subtable(uv, "sources");
    uv["sources"]["streamlib"] = toml_edit::Item::Value(toml_edit::Value::InlineTable(source));

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

    let mut new_content = serde_json::to_string_pretty(&value).map_err(|e| {
        LinkError::ManifestParse {
            path: deno.to_path_buf(),
            detail: e.to_string(),
        }
    })?;
    new_content.push('\n');

    Ok(PlannedEdit {
        rel_path: rel_of(deno, consumer_root),
        abs_path: deno.to_path_buf(),
        new_content,
        original,
    })
}

/// Apply every planned edit transactionally: back up pre-existing files, write
/// the new content, and roll back all applied edits on any failure.
fn apply_transaction(
    consumer_root: &Path,
    edits: &[PlannedEdit],
) -> Result<Vec<TouchedFile>, LinkError> {
    let backup_dir = consumer_root.join(LINK_STATE_DIR).join(LINK_BACKUP_DIR);
    std::fs::create_dir_all(&backup_dir).map_err(|e| LinkError::io(&backup_dir, e))?;

    let mut applied: Vec<&PlannedEdit> = Vec::new();
    let mut touched: Vec<TouchedFile> = Vec::new();

    for edit in edits {
        let result = (|| -> Result<(), LinkError> {
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
                .map_err(|e| LinkError::io(&edit.abs_path, e))
        })();

        if let Err(e) = result {
            rollback(&applied);
            let state_dir = consumer_root.join(LINK_STATE_DIR);
            let _ = std::fs::remove_dir_all(&state_dir);
            return Err(e);
        }

        touched.push(TouchedFile {
            path: edit.rel_path.clone(),
            existed_before: edit.original.is_some(),
            pre_edit_sha256: edit
                .original
                .as_ref()
                .map(|b| hex_sha256(b))
                .unwrap_or_default(),
        });
        applied.push(edit);
    }

    Ok(touched)
}

/// Undo applied edits in reverse order: restore originals, delete created files.
fn rollback(applied: &[&PlannedEdit]) {
    for edit in applied.iter().rev() {
        match &edit.original {
            Some(orig) => {
                let _ = std::fs::write(&edit.abs_path, orig);
            }
            None => {
                let _ = std::fs::remove_file(&edit.abs_path);
            }
        }
    }
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

fn ensure_table(doc: &mut toml_edit::DocumentMut, key: &str) {
    if !doc.contains_key(key) {
        doc.insert(key, toml_edit::Item::Table(toml_edit::Table::new()));
    }
}

fn ensure_subtable(table: &mut toml_edit::Table, key: &str) {
    if !table.contains_key(key) {
        table.insert(key, toml_edit::Item::Table(toml_edit::Table::new()));
    }
}

#[cfg(test)]
mod tests;
