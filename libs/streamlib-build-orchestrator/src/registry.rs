// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Programmatic toolchain-config for pointing a consumer at a single static
//! registry location — the library half of the `streamlib registry use` /
//! `streamlib registry serve` CLI verbs.

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

use streamlib_idents::{DEFAULT_REGISTRY_URL, RegistryConfig};
use thiserror::Error;

/// The named cargo registry the streamlib SDK crate chain + vulkanalia fork
/// resolve under (`registry = "tatolab"` in every published manifest).
pub const TATOLAB_REGISTRY_NAME: &str = "tatolab";

/// The replacement-source name the `[source]` stanza redirects `tatolab` to.
/// Matches the CI `cargo-fork-mirror` action so a consumer's config and CI
/// agree on the mirror source id.
pub const TATOLAB_REPLACEMENT_SOURCE_NAME: &str = "tatolab-local-registry";

/// Reshape-script basename inside a streamlib `scripts/registry/` dir — the
/// single canonical sparse→local-registry reshape reused by `registry use`.
const RESHAPE_SCRIPT_NAME: &str = "emit-cargo-local-registry.sh";

/// Default localhost port for [`serve_registry`] when the caller pins none —
/// matches `scripts/registry/serve-static-registry.sh` so a written `.npmrc`
/// scope is stable across serve sessions.
pub const DEFAULT_SERVE_PORT: u16 = 8799;

/// Errors from [`use_registry`] and the cargo `[source]`-replacement emission.
#[derive(Debug, Error)]
pub enum RegistryUseError {
    /// The named registry tree directory does not exist / is not a directory.
    #[error("registry tree {path} is not a directory")]
    TreeNotADirectory { path: PathBuf },
    /// A local tree is missing the `cargo/` subtree the reshape needs.
    #[error("no cargo/ subtree under registry tree {tree_root} (run `static-registry emit` first)")]
    CargoSubtreeMissing { tree_root: PathBuf },
    /// A local-tree reshape was requested but the reshape script could not be
    /// located; the caller must supply the streamlib `scripts/registry/` dir.
    #[error(
        "cargo local-registry reshape script `{RESHAPE_SCRIPT_NAME}` not found (searched: {searched:?}); run from a streamlib checkout or pass a scripts dir"
    )]
    ReshapeScriptNotFound { searched: Vec<PathBuf> },
    /// The reshape script ran but exited non-zero.
    #[error("cargo local-registry reshape failed: {detail}")]
    ReshapeFailed { detail: String },
    /// Canonicalizing / resolving the tree directory failed.
    #[error("resolve registry tree {path}: {detail}")]
    TreeResolve { path: PathBuf, detail: String },
    /// Reading the existing cargo config to merge into failed.
    #[error("read {path}: {detail}")]
    CargoConfigRead { path: PathBuf, detail: String },
    /// The existing cargo config is not valid TOML.
    #[error("parse {path}: {detail}")]
    CargoConfigParse { path: PathBuf, detail: String },
    /// A config key exists but is not a table, so the stanza can't be merged.
    #[error("cargo config key `{key}` exists but is not a table")]
    CargoConfigMalformed { key: String },
    /// Writing the merged cargo config back failed.
    #[error("write {path}: {detail}")]
    CargoConfigWrite { path: PathBuf, detail: String },
    /// Spawning the reshape script (`bash`) failed.
    #[error("spawn reshape script: {detail}")]
    Spawn { detail: String },
}

/// Errors from [`serve_registry`].
#[derive(Debug, Error)]
pub enum RegistryServeError {
    /// The named registry tree directory does not exist / is not a directory.
    #[error("registry tree {path} is not a directory")]
    TreeNotADirectory { path: PathBuf },
    /// Binding an ephemeral localhost port to serve on failed.
    #[error("bind localhost serve port: {detail}")]
    BindPort { detail: String },
    /// Spawning `python3 -m http.server` failed.
    #[error("spawn static server (is python3 installed?): {detail}")]
    Spawn { detail: String },
    /// The spawned server never began accepting connections.
    #[error("static server did not come up on 127.0.0.1:{port}")]
    ServerDidNotStart { port: u16 },
    /// Writing the `.npmrc` scope line back failed.
    #[error("write {path}: {detail}")]
    NpmrcWrite { path: PathBuf, detail: String },
}

/// Where the `[source.tatolab-local-registry]` block points — the two
/// serverless-vs-served cargo resolution shapes a consumer resolves through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CargoReplacementSource {
    /// A serverless cargo `local-registry` dir reshaped from a local `file://`
    /// tree's sparse `cargo/` subtree. Resolves `--offline` with no server.
    LocalRegistry(PathBuf),
    /// A served `sparse+http(s)://…/cargo/` mount (the tree is already an HTTP
    /// registry mount). No reshape.
    SparseMirror(String),
}

/// Options for [`use_registry`].
#[derive(Debug, Default)]
pub struct UseRegistryOptions {
    /// For a local `file://` tree, the dir the cargo `local-registry` mirror is
    /// reshaped into. Default: `<consumer_root>/.streamlib/cargo-local-registry`.
    pub cargo_local_registry_dir: Option<PathBuf>,
    /// The `scripts/registry/` dir holding `emit-cargo-local-registry.sh`,
    /// required to reshape a local tree's sparse cargo subtree. `None` errors
    /// with [`RegistryUseError::ReshapeScriptNotFound`] when a reshape is needed.
    pub reshape_scripts_dir: Option<PathBuf>,
}

/// What [`use_registry`] configured, for the CLI to present.
#[derive(Debug)]
pub struct UseRegistryReport {
    /// The resolved single registry location every channel derives from.
    pub registry: RegistryConfig,
    /// Path the cargo `[source]`-replacement stanza was merged into.
    pub cargo_config_path: PathBuf,
    /// The cargo replacement source that was emitted.
    pub cargo_replacement: CargoReplacementSource,
    /// The `UV_INDEX` value pypi resolution uses (derived, `file://`/http).
    pub pypi_index_url: String,
    /// The `@tatolab:registry=` value npm resolution needs.
    pub npm_registry_url: String,
    /// True when the tree is a local `file://` folder, so npm needs
    /// [`serve_registry`] before it is reachable (npm has no `file://` story).
    pub npm_needs_serve: bool,
}

/// Point a consumer at a single registry location `tree_ref` (a local folder
/// path, a `file://` URL, or an `http(s)://` mount), writing the cargo
/// `[source]`-replacement into `<consumer_root>/.cargo/config.toml` and
/// deriving the pypi / npm channels. A local folder is reshaped into a
/// serverless cargo `local-registry` mirror (no running server); an HTTP mount
/// is pointed at directly. Source replacement keeps the canonical `tatolab`
/// source id in every `Cargo.lock`.
#[tracing::instrument(skip(opts), fields(consumer_root = %consumer_root.display(), tree_ref = %tree_ref))]
pub fn use_registry(
    consumer_root: &Path,
    tree_ref: &str,
    opts: &UseRegistryOptions,
) -> Result<UseRegistryReport, RegistryUseError> {
    let registry = resolve_registry_ref(tree_ref)?;

    let cargo_replacement = if let Some(tree_root) = registry.local_tree_root() {
        // Local `file://` tree → serverless local-registry reshape.
        if !tree_root.join("cargo").is_dir() {
            return Err(RegistryUseError::CargoSubtreeMissing { tree_root });
        }
        let lr_dir = opts.cargo_local_registry_dir.clone().unwrap_or_else(|| {
            consumer_root
                .join(".streamlib")
                .join("cargo-local-registry")
        });
        let scripts_dir = match &opts.reshape_scripts_dir {
            Some(dir) => dir.clone(),
            None => locate_reshape_scripts_dir()?,
        };
        reshape_sparse_cargo_to_local_registry(&tree_root, &lr_dir, &scripts_dir)?;
        // The stanza records the absolute mirror dir so cargo resolves it from
        // any working directory under the consumer root.
        let abs_lr = lr_dir.canonicalize().unwrap_or(lr_dir);
        CargoReplacementSource::LocalRegistry(abs_lr)
    } else {
        // Served `http(s)://` mount → point the replacement at its sparse index.
        CargoReplacementSource::SparseMirror(registry.cargo_sparse_index_url())
    };

    let cargo_config_path = write_cargo_source_replacement(consumer_root, &cargo_replacement)?;

    Ok(UseRegistryReport {
        pypi_index_url: registry.pypi_simple_index_url(),
        npm_registry_url: registry.npm_registry_url(),
        npm_needs_serve: registry.local_tree_root().is_some(),
        registry,
        cargo_config_path,
        cargo_replacement,
    })
}

/// Resolve a `tree_ref` (local dir path | `file://` | `http(s)://`) into a
/// [`RegistryConfig`]. A bare path is canonicalized to a `file://` tree.
fn resolve_registry_ref(tree_ref: &str) -> Result<RegistryConfig, RegistryUseError> {
    if tree_ref.starts_with("http://")
        || tree_ref.starts_with("https://")
        || tree_ref.starts_with("file://")
    {
        return Ok(RegistryConfig {
            base_url: tree_ref.trim_end_matches('/').to_string(),
        });
    }
    let dir = Path::new(tree_ref);
    if !dir.is_dir() {
        return Err(RegistryUseError::TreeNotADirectory {
            path: dir.to_path_buf(),
        });
    }
    RegistryConfig::for_local_tree(dir).map_err(|e| RegistryUseError::TreeResolve {
        path: dir.to_path_buf(),
        detail: e.to_string(),
    })
}

/// Render the cargo `[source]`-replacement stanza that redirects the canonical
/// `tatolab` registry at `replacement`, keeping the canonical source id in
/// every `Cargo.lock`. Pure — the string the CLI prints; [`use_registry`]
/// merges the same three tables into a consumer's `.cargo/config.toml`.
pub fn emit_cargo_source_replacement_stanza(replacement: &CargoReplacementSource) -> String {
    let canonical = format!("sparse+{DEFAULT_REGISTRY_URL}/cargo/");
    let replacement_body = match replacement {
        CargoReplacementSource::LocalRegistry(dir) => {
            format!("local-registry = \"{}\"", dir.display())
        }
        CargoReplacementSource::SparseMirror(index) => format!("registry = \"{index}\""),
    };
    format!(
        "[registries.{name}]\n\
         index = \"{canonical}\"\n\
         \n\
         [source.{name}]\n\
         registry = \"{canonical}\"\n\
         replace-with = \"{repl}\"\n\
         \n\
         [source.{repl}]\n\
         {replacement_body}\n",
        name = TATOLAB_REGISTRY_NAME,
        repl = TATOLAB_REPLACEMENT_SOURCE_NAME,
    )
}

/// Merge the cargo `[source]`-replacement (and the `[registries.tatolab]`
/// declaration it replaces) into `<consumer_root>/.cargo/config.toml`,
/// preserving any other config the consumer already has. Idempotent — a
/// repeated `use` overwrites only the three `tatolab` tables. Returns the
/// config path written.
fn write_cargo_source_replacement(
    consumer_root: &Path,
    replacement: &CargoReplacementSource,
) -> Result<PathBuf, RegistryUseError> {
    let cargo_dir = consumer_root.join(".cargo");
    let config_path = cargo_dir.join("config.toml");

    let existing = match std::fs::read_to_string(&config_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(RegistryUseError::CargoConfigRead {
                path: config_path,
                detail: e.to_string(),
            });
        }
    };
    let mut doc: toml_edit::DocumentMut =
        existing.parse().map_err(
            |e: toml_edit::TomlError| RegistryUseError::CargoConfigParse {
                path: config_path.clone(),
                detail: e.to_string(),
            },
        )?;

    let canonical = format!("sparse+{DEFAULT_REGISTRY_URL}/cargo/");

    // [registries.tatolab] index = <canonical>
    {
        let registries = ensure_implicit_table(doc.as_table_mut(), "registries")?;
        let tatolab = ensure_table(registries, TATOLAB_REGISTRY_NAME)?;
        tatolab.insert("index", toml_edit::value(canonical.clone()));
    }
    // [source.tatolab] registry = <canonical>, replace-with = <repl>
    // [source.<repl>] local-registry|registry = ...
    {
        let source = ensure_implicit_table(doc.as_table_mut(), "source")?;
        {
            let tatolab_src = ensure_table(source, TATOLAB_REGISTRY_NAME)?;
            tatolab_src.insert("registry", toml_edit::value(canonical.clone()));
            tatolab_src.insert(
                "replace-with",
                toml_edit::value(TATOLAB_REPLACEMENT_SOURCE_NAME),
            );
        }
        {
            let repl_src = ensure_table(source, TATOLAB_REPLACEMENT_SOURCE_NAME)?;
            match replacement {
                CargoReplacementSource::LocalRegistry(dir) => {
                    repl_src.remove("registry");
                    repl_src.insert(
                        "local-registry",
                        toml_edit::value(dir.display().to_string()),
                    );
                }
                CargoReplacementSource::SparseMirror(index) => {
                    repl_src.remove("local-registry");
                    repl_src.insert("registry", toml_edit::value(index.clone()));
                }
            }
        }
    }

    std::fs::create_dir_all(&cargo_dir).map_err(|e| RegistryUseError::CargoConfigWrite {
        path: config_path.clone(),
        detail: e.to_string(),
    })?;
    std::fs::write(&config_path, doc.to_string()).map_err(|e| {
        RegistryUseError::CargoConfigWrite {
            path: config_path.clone(),
            detail: e.to_string(),
        }
    })?;
    Ok(config_path)
}

/// Get-or-create `key` as a table; error (never panic) when a non-table value
/// already occupies the key.
fn ensure_table<'a>(
    table: &'a mut toml_edit::Table,
    key: &str,
) -> Result<&'a mut toml_edit::Table, RegistryUseError> {
    if !table.contains_key(key) {
        table.insert(key, toml_edit::Item::Table(toml_edit::Table::new()));
    }
    table[key]
        .as_table_mut()
        .ok_or_else(|| RegistryUseError::CargoConfigMalformed {
            key: key.to_string(),
        })
}

/// [`ensure_table`] plus mark the parent implicit so only the child header
/// (`[registries.tatolab]`, not a bare `[registries]`) is rendered.
fn ensure_implicit_table<'a>(
    table: &'a mut toml_edit::Table,
    key: &str,
) -> Result<&'a mut toml_edit::Table, RegistryUseError> {
    let t = ensure_table(table, key)?;
    t.set_implicit(true);
    Ok(t)
}

/// Reshape a local registry tree's sparse `cargo/` subtree into a serverless
/// cargo `local-registry` mirror at `lr_out`, via the single canonical reshape
/// script in `scripts_dir`. Deterministic file copies — no server, no cargo.
pub fn reshape_sparse_cargo_to_local_registry(
    tree_root: &Path,
    lr_out: &Path,
    scripts_dir: &Path,
) -> Result<(), RegistryUseError> {
    let script = scripts_dir.join(RESHAPE_SCRIPT_NAME);
    if !script.is_file() {
        return Err(RegistryUseError::ReshapeScriptNotFound {
            searched: vec![script],
        });
    }
    if let Some(parent) = lr_out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| RegistryUseError::ReshapeFailed {
            detail: format!("create mirror parent {}: {e}", parent.display()),
        })?;
    }
    let output = Command::new("bash")
        .arg(&script)
        .arg(tree_root)
        .arg(lr_out)
        .output()
        .map_err(|e| RegistryUseError::Spawn {
            detail: e.to_string(),
        })?;
    if !output.status.success() {
        return Err(RegistryUseError::ReshapeFailed {
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(())
}

/// Walk up from the current working directory and the running executable
/// looking for a streamlib `scripts/registry/` dir holding the reshape script.
fn locate_reshape_scripts_dir() -> Result<PathBuf, RegistryUseError> {
    let mut searched = Vec::new();
    let mut roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        roots.push(parent.to_path_buf());
    }
    for root in roots {
        for ancestor in root.ancestors() {
            let candidate = ancestor.join("scripts").join("registry");
            if candidate.join(RESHAPE_SCRIPT_NAME).is_file() {
                return Ok(candidate);
            }
            searched.push(candidate);
        }
    }
    Err(RegistryUseError::ReshapeScriptNotFound { searched })
}

/// Options for [`serve_registry`].
#[derive(Debug, Default)]
pub struct ServeRegistryOptions {
    /// Localhost port to serve on. `None` → [`DEFAULT_SERVE_PORT`].
    pub port: Option<u16>,
}

/// A running localhost static server for a registry tree — kills the child on
/// drop. Only npm needs this seam (it has no `file://` story); cargo resolves
/// serverless via the `local-registry` mirror and pypi/`.slpkg` read `file://`.
#[derive(Debug)]
pub struct RegistryServeHandle {
    child: Child,
    /// The port the tree is served on.
    pub port: u16,
    /// The `.npmrc` scope line consumers point npm at.
    pub npm_scope_line: String,
    /// The localhost base URL the tree is served at.
    pub base_url: String,
}

impl RegistryServeHandle {
    /// Block until the server child exits (e.g. on Ctrl-C, which the child
    /// receives with the foreground process group).
    pub fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait()
    }
}

impl Drop for RegistryServeHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Serve a local registry `tree_dir` over a dumb localhost static HTTP mount so
/// npm can resolve `@tatolab` (the one ecosystem with no `file://` registry
/// story). Returns a handle carrying the `.npmrc` scope; the caller writes /
/// prints it and holds the handle for the serve session's lifetime.
#[tracing::instrument(skip(opts), fields(tree_dir = %tree_dir.display()))]
pub fn serve_registry(
    tree_dir: &Path,
    opts: &ServeRegistryOptions,
) -> Result<RegistryServeHandle, RegistryServeError> {
    if !tree_dir.is_dir() {
        return Err(RegistryServeError::TreeNotADirectory {
            path: tree_dir.to_path_buf(),
        });
    }
    let port = match opts.port {
        Some(p) => p,
        None => DEFAULT_SERVE_PORT,
    };
    // Fail early with a typed error if the port is already taken, rather than
    // letting python fail opaquely.
    match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => drop(listener),
        Err(e) => {
            return Err(RegistryServeError::BindPort {
                detail: format!("127.0.0.1:{port}: {e}"),
            });
        }
    }

    let mut cmd = Command::new("python3");
    cmd.args([
        "-m",
        "http.server",
        &port.to_string(),
        "--bind",
        "127.0.0.1",
        "--directory",
    ])
    .arg(tree_dir)
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null());
    // Kill the server child when the parent (CLI / embedding host) dies, so a
    // killed parent never orphans the static server — the Drop impl only covers
    // a normal handle drop. `PR_SET_PDEATHSIG` is delivered on parent death and
    // `prctl` is async-signal-safe, so it's legal from `pre_exec`; it survives
    // the child's (non-setuid) `exec` of python.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let child = cmd.spawn().map_err(|e| RegistryServeError::Spawn {
        detail: e.to_string(),
    })?;

    let base_url = format!("http://127.0.0.1:{port}");
    let npm_scope_line = format!("@{TATOLAB_REGISTRY_NAME}:registry={base_url}/npm/");
    let mut handle = RegistryServeHandle {
        child,
        port,
        npm_scope_line,
        base_url,
    };

    // Wait for the server to accept connections (its Drop kills the child if we
    // give up).
    for _ in 0..50 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok(handle);
        }
        // If the child already died (e.g. python3 missing), surface it.
        if let Ok(Some(_)) = handle.child.try_wait() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(RegistryServeError::ServerDidNotStart { port })
}

/// Merge the `@tatolab:registry=<npm-url>` scope line into `<consumer_root>/
/// .npmrc`, replacing any prior `@tatolab:registry=` line and preserving the
/// rest. Returns the `.npmrc` path written. This is what lets `registry serve`
/// leave a consumer's npm config set without hand-editing.
pub fn write_npmrc_scope(
    consumer_root: &Path,
    npm_scope_line: &str,
) -> Result<PathBuf, RegistryServeError> {
    let npmrc = consumer_root.join(".npmrc");
    let existing = match std::fs::read_to_string(&npmrc) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(RegistryServeError::NpmrcWrite {
                path: npmrc,
                detail: e.to_string(),
            });
        }
    };
    let prefix = format!("@{TATOLAB_REGISTRY_NAME}:registry=");
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|l| !l.trim_start().starts_with(&prefix))
        .map(|l| l.to_string())
        .collect();
    lines.push(npm_scope_line.to_string());
    let mut body = lines.join("\n");
    body.push('\n');
    std::fs::write(&npmrc, body).map_err(|e| RegistryServeError::NpmrcWrite {
        path: npmrc.clone(),
        detail: e.to_string(),
    })?;
    Ok(npmrc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stanza_local_registry_emits_source_replacement_block() {
        let repl = CargoReplacementSource::LocalRegistry(PathBuf::from("/home/u/.streamlib/clr"));
        let stanza = emit_cargo_source_replacement_stanza(&repl);
        // Canonical registry declared + replaced; the mirror is the local dir.
        assert!(stanza.contains("[registries.tatolab]"), "{stanza}");
        assert!(
            stanza.contains("index = \"sparse+https://registry.tatolab.com/cargo/\""),
            "{stanza}"
        );
        assert!(stanza.contains("[source.tatolab]"), "{stanza}");
        assert!(
            stanza.contains("registry = \"sparse+https://registry.tatolab.com/cargo/\""),
            "{stanza}"
        );
        assert!(
            stanza.contains("replace-with = \"tatolab-local-registry\""),
            "{stanza}"
        );
        assert!(
            stanza.contains("[source.tatolab-local-registry]"),
            "{stanza}"
        );
        assert!(
            stanza.contains("local-registry = \"/home/u/.streamlib/clr\""),
            "{stanza}"
        );
        // Serverless mirror never emits a sparse+http replacement (the #1245 poison).
        assert!(!stanza.contains("sparse+http://"), "{stanza}");
    }

    #[test]
    fn stanza_sparse_mirror_points_replacement_at_served_index() {
        let repl =
            CargoReplacementSource::SparseMirror("sparse+http://127.0.0.1:8799/cargo/".to_string());
        let stanza = emit_cargo_source_replacement_stanza(&repl);
        // Canonical id preserved on the replaced source, replacement is the mount.
        assert!(
            stanza.contains(
                "[source.tatolab]\nregistry = \"sparse+https://registry.tatolab.com/cargo/\""
            ),
            "{stanza}"
        );
        assert!(
            stanza.contains("[source.tatolab-local-registry]\nregistry = \"sparse+http://127.0.0.1:8799/cargo/\""),
            "{stanza}"
        );
        // A served mirror must NOT be written as a local-registry.
        assert!(!stanza.contains("local-registry ="), "{stanza}");
    }

    #[test]
    fn write_cargo_source_replacement_creates_and_merges_config() {
        let consumer = tempfile::tempdir().unwrap();
        let repl = CargoReplacementSource::LocalRegistry(PathBuf::from("/mnt/mirror"));
        let path = write_cargo_source_replacement(consumer.path(), &repl).unwrap();
        assert_eq!(path, consumer.path().join(".cargo").join("config.toml"));

        let body = std::fs::read_to_string(&path).unwrap();
        // Reverting the local-registry insert would drop this line.
        assert!(body.contains("local-registry = \"/mnt/mirror\""), "{body}");
        assert!(
            body.contains("replace-with = \"tatolab-local-registry\""),
            "{body}"
        );

        // Parses back as valid TOML with the expected structure.
        let doc: toml_edit::DocumentMut = body.parse().unwrap();
        assert_eq!(
            doc["registries"]["tatolab"]["index"].as_str().unwrap(),
            "sparse+https://registry.tatolab.com/cargo/"
        );
        assert_eq!(
            doc["source"]["tatolab"]["replace-with"].as_str().unwrap(),
            "tatolab-local-registry"
        );
        assert_eq!(
            doc["source"]["tatolab-local-registry"]["local-registry"]
                .as_str()
                .unwrap(),
            "/mnt/mirror"
        );
    }

    #[test]
    fn write_cargo_source_replacement_preserves_unrelated_config() {
        let consumer = tempfile::tempdir().unwrap();
        let cargo_dir = consumer.path().join(".cargo");
        std::fs::create_dir_all(&cargo_dir).unwrap();
        std::fs::write(
            cargo_dir.join("config.toml"),
            "[build]\njobs = 4\n\n[net]\nretry = 3\n",
        )
        .unwrap();

        let repl = CargoReplacementSource::LocalRegistry(PathBuf::from("/mnt/mirror"));
        write_cargo_source_replacement(consumer.path(), &repl).unwrap();

        let body = std::fs::read_to_string(cargo_dir.join("config.toml")).unwrap();
        let doc: toml_edit::DocumentMut = body.parse().unwrap();
        // Pre-existing unrelated config survives the merge.
        assert_eq!(doc["build"]["jobs"].as_integer().unwrap(), 4);
        assert_eq!(doc["net"]["retry"].as_integer().unwrap(), 3);
        // And the tatolab replacement landed.
        assert_eq!(
            doc["source"]["tatolab-local-registry"]["local-registry"]
                .as_str()
                .unwrap(),
            "/mnt/mirror"
        );
    }

    #[test]
    fn write_cargo_source_replacement_is_idempotent() {
        let consumer = tempfile::tempdir().unwrap();
        let repl = CargoReplacementSource::LocalRegistry(PathBuf::from("/mnt/mirror"));
        write_cargo_source_replacement(consumer.path(), &repl).unwrap();
        write_cargo_source_replacement(consumer.path(), &repl).unwrap();
        let body =
            std::fs::read_to_string(consumer.path().join(".cargo").join("config.toml")).unwrap();
        // Exactly one replacement source table — a repeated `use` overwrites,
        // never duplicates.
        assert_eq!(
            body.matches("[source.tatolab-local-registry]").count(),
            1,
            "{body}"
        );
    }

    #[test]
    fn switching_from_sparse_mirror_to_local_registry_drops_stale_key() {
        let consumer = tempfile::tempdir().unwrap();
        // First point at a served mirror…
        write_cargo_source_replacement(
            consumer.path(),
            &CargoReplacementSource::SparseMirror("sparse+http://127.0.0.1:9/cargo/".to_string()),
        )
        .unwrap();
        // …then re-point at a serverless local-registry.
        write_cargo_source_replacement(
            consumer.path(),
            &CargoReplacementSource::LocalRegistry(PathBuf::from("/mnt/mirror")),
        )
        .unwrap();
        let body =
            std::fs::read_to_string(consumer.path().join(".cargo").join("config.toml")).unwrap();
        let doc: toml_edit::DocumentMut = body.parse().unwrap();
        let repl = &doc["source"]["tatolab-local-registry"];
        // The stale `registry = sparse+...` must be gone; only local-registry remains.
        assert!(repl.get("registry").is_none(), "{body}");
        assert!(repl.get("local-registry").is_some(), "{body}");
    }

    #[test]
    fn non_table_registries_key_is_a_typed_error_not_a_panic() {
        let consumer = tempfile::tempdir().unwrap();
        let cargo_dir = consumer.path().join(".cargo");
        std::fs::create_dir_all(&cargo_dir).unwrap();
        std::fs::write(cargo_dir.join("config.toml"), "registries = 3\n").unwrap();
        let err = write_cargo_source_replacement(
            consumer.path(),
            &CargoReplacementSource::LocalRegistry(PathBuf::from("/m")),
        )
        .expect_err("non-table registries must be a typed error");
        assert!(matches!(err, RegistryUseError::CargoConfigMalformed { .. }));
    }

    #[test]
    fn resolve_registry_ref_classifies_local_and_remote() {
        // A `file://` and a plain dir path both resolve to a local tree.
        let remote = resolve_registry_ref("https://registry.tatolab.com").unwrap();
        assert!(remote.local_tree_root().is_none());
        assert_eq!(
            remote.cargo_sparse_index_url(),
            "sparse+https://registry.tatolab.com/cargo/"
        );

        let tree = tempfile::tempdir().unwrap();
        let local = resolve_registry_ref(tree.path().to_str().unwrap()).unwrap();
        assert!(local.local_tree_root().is_some());
        // Trailing slash trimmed on URL forms.
        let file_url = resolve_registry_ref(&format!("file://{}/", tree.path().display())).unwrap();
        assert!(!file_url.base_url.ends_with('/'));
    }

    #[test]
    fn use_registry_missing_cargo_subtree_errors() {
        // A local tree with no cargo/ subtree cannot be reshaped.
        let consumer = tempfile::tempdir().unwrap();
        let tree = tempfile::tempdir().unwrap();
        let err = use_registry(
            consumer.path(),
            tree.path().to_str().unwrap(),
            &UseRegistryOptions::default(),
        )
        .expect_err("no cargo/ subtree must error");
        assert!(matches!(err, RegistryUseError::CargoSubtreeMissing { .. }));
    }

    #[test]
    fn reshape_missing_script_is_a_typed_error() {
        let tree = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tree.path().join("cargo")).unwrap();
        let out = tempfile::tempdir().unwrap();
        let empty_scripts = tempfile::tempdir().unwrap();
        let err = reshape_sparse_cargo_to_local_registry(
            tree.path(),
            &out.path().join("clr"),
            empty_scripts.path(),
        )
        .expect_err("missing reshape script must be a typed error");
        assert!(matches!(
            err,
            RegistryUseError::ReshapeScriptNotFound { .. }
        ));
    }

    #[test]
    fn write_npmrc_scope_replaces_prior_tatolab_line_only() {
        let consumer = tempfile::tempdir().unwrap();
        std::fs::write(
            consumer.path().join(".npmrc"),
            "@tatolab:registry=http://127.0.0.1:1111/npm/\nalways-auth=false\n",
        )
        .unwrap();
        let line = "@tatolab:registry=http://127.0.0.1:8799/npm/";
        write_npmrc_scope(consumer.path(), line).unwrap();
        let body = std::fs::read_to_string(consumer.path().join(".npmrc")).unwrap();
        // Old scope replaced, unrelated line kept, new scope present exactly once.
        assert!(!body.contains(":1111/"), "{body}");
        assert!(body.contains("always-auth=false"), "{body}");
        assert_eq!(body.matches("@tatolab:registry=").count(), 1, "{body}");
        assert!(body.contains(":8799/npm/"), "{body}");
    }
}
