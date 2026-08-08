// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! StreamLib CLI
//!
//! Command-line interface for spawning runtimes and managing local artifacts.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use streamlib_jtd_codegen::RuntimeTarget;

mod commands;

#[derive(Parser)]
#[command(name = "streamlib")]
#[command(author, version, about = "StreamLib runtime CLI", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Setup commands
    Setup {
        #[command(subcommand)]
        action: SetupCommands,
    },

    /// Schema management
    Schemas {
        #[command(subcommand)]
        action: SchemasCommands,
    },

    /// Reproduce this app's streamlib_modules/ folder from its committed
    /// streamlib.lock — exactly, hash-verified, and offline.
    ///
    /// Install is the container/CI preinstall seam: `add`/`link` decide what's
    /// in the environment and record it in `streamlib.lock`; `install`
    /// reproduces that decision elsewhere (a fresh checkout, an image build)
    /// with no resolution decisions. Each byte-source entry (folder / archive /
    /// URL) is re-materialized and re-verified against its recorded content
    /// hash; a linked entry's symlink is re-created (a gone checkout target is
    /// an error — a dev link isn't reproducible on another machine). Never
    /// builds.
    Install {
        /// App root to anchor streamlib_modules/ + streamlib.lock at
        /// (default: current working directory, no walk-up).
        #[arg(long)]
        dir: Option<PathBuf>,

        /// Reproduce only — skip the on-the-box compile of each materialized
        /// slot (for toolchain-free machines).
        #[arg(long)]
        no_build: bool,
    },

    /// Record a dependency (in a package dir) or adopt a package (in an app).
    ///
    /// Context-sensitive on the anchor directory:
    ///
    /// - In a **package-authoring dir** (a `streamlib.yaml` with a `package:`
    ///   block), `streamlib add @org/name@<version>` records a caret
    ///   dependency (`^<version>`) into that package's own `dependencies:` —
    ///   the schema-tier `cargo add`. `pkg publish` reconciles it against code.
    /// - In a **consumer / app dir**, takes a byte source — a package folder,
    ///   an archive (`.slpkg` / `.zip` / `.tar.gz`), or a `file://` / HTTP(S)
    ///   URL — materializes it into `streamlib_modules/@org/name/` beside the
    ///   app, and records identity, source, and content hash in the app's
    ///   `streamlib.lock`. Identity comes from the package's own manifest;
    ///   re-adding replaces cleanly. Never builds.
    Add {
        /// Package dir: `@org/name@<version>`. App dir: package folder |
        /// archive (`.slpkg`/`.zip`/`.tar.gz`) | URL.
        spec: String,

        /// Anchor dir — a package dir to record a dependency in, or an app root
        /// to materialize into (default: current working directory, no walk-up).
        #[arg(long)]
        dir: Option<PathBuf>,

        /// Expected SHA-256 of the archive bytes (hex, optional `sha256:`
        /// prefix). A mismatch fails the add with nothing materialized.
        #[arg(long)]
        expect_sha256: Option<String>,

        /// Place only — skip the on-the-box compile of the added slot (for
        /// toolchain-free machines).
        #[arg(long)]
        no_build: bool,
    },

    /// Remove a package from this app's streamlib_modules/ folder.
    ///
    /// Deletes `streamlib_modules/@org/name/` and drops the package's entry
    /// from the app's `streamlib.lock`.
    Remove {
        /// Canonical `@org/name` reference to remove.
        name: String,

        /// App root to anchor streamlib_modules/ + streamlib.lock at
        /// (default: current working directory, no walk-up).
        #[arg(long)]
        dir: Option<PathBuf>,
    },

    /// Manage installed packages
    Pkg {
        #[command(subcommand)]
        action: PkgCommands,
    },

    /// Link a local package checkout into this app's streamlib_modules/ — npm
    /// link for streamlib packages.
    ///
    /// `link <path>` is `add` with a symlink instead of a copy: it symlinks the
    /// checkout into `streamlib_modules/@org/name` (identity read from the
    /// checkout's own manifest), so edits in the checkout are live on the next
    /// run with no re-add. `unlink <name>` reverts it.
    ///
    /// `--engine <checkout>` is the rare engine-developer verb: it points this
    /// consumer's ENTIRE streamlib SDK surface at a local engine checkout via
    /// whole-tree cargo `[patch]` / uv / Deno import-map overrides. Omit the
    /// path with `--engine` to print engine-link status. App developers never
    /// need `--engine`.
    Link {
        /// Package checkout to symlink (default), or the engine checkout with
        /// `--engine`. With `--engine`, omit to print engine-link status.
        path: Option<PathBuf>,

        /// Engine-developer mode: whole-tree SDK override pointing at <path>.
        #[arg(long)]
        engine: bool,

        /// (package link) App root to anchor streamlib_modules/ at
        /// (default: current working directory, no walk-up).
        #[arg(long)]
        dir: Option<PathBuf>,

        /// (engine link) Skip the post-link cargo resolution verification.
        #[arg(long)]
        skip_verify: bool,
    },

    /// Reverse a `streamlib link`.
    ///
    /// `unlink <name>` removes a package's `streamlib_modules/@org/name` symlink
    /// and its `streamlib.lock` entry (the linked checkout is untouched).
    /// `--engine` removes the whole-tree engine link, restoring every manifest
    /// byte-identically.
    Unlink {
        /// Canonical `@org/name` package to unlink (omit with `--engine`).
        name: Option<String>,

        /// Engine-developer mode: remove the active whole-tree engine link.
        #[arg(long)]
        engine: bool,

        /// (package unlink) App root to anchor streamlib_modules/ at
        /// (default: current working directory, no walk-up).
        #[arg(long)]
        dir: Option<PathBuf>,

        /// (engine unlink) Discard files modified while the link was active
        /// instead of refusing.
        #[arg(long)]
        force: bool,
    },

    /// Generate typed bindings from JTD schemas via the JTD-codegen pipeline.
    ///
    /// Same pipeline contributors run as `cargo xtask generate-schemas`,
    /// reachable here without rustup.
    Generate {
        /// Target language (default: rust)
        #[arg(long, default_value = "rust")]
        runtime: RuntimeTarget,

        /// Output directory (required)
        #[arg(long)]
        output: PathBuf,

        /// `streamlib.yaml`-driven mode: directory containing the manifest.
        /// The resolver walks declared dependencies and codegen ingests the
        /// resulting set, writing `streamlib-codegen.lock` next to the
        /// manifest.
        #[arg(long, group = "input")]
        project_dir: Option<PathBuf>,

        /// Process a single schema file
        #[arg(long, group = "input")]
        schema_file: Option<PathBuf>,

        /// Process all .yaml files in a directory
        #[arg(long, group = "input")]
        schema_dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum SetupCommands {
    /// Configure shell to add streamlib to PATH
    Shell {
        /// Shell type (bash, zsh, fish). Auto-detected if not specified.
        #[arg(long)]
        shell: Option<String>,
    },
}

#[derive(Subcommand)]
enum SchemasCommands {
    /// Validate a processor YAML schema file
    ValidateProcessor {
        /// Path to the processor YAML file
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum PkgCommands {
    /// Publish THIS package to the package source (run inside the package).
    ///
    /// Always repacks a fresh source-only `.slpkg` (never trusts an existing
    /// artifact) and writes it into the package source (a static `.slpkg`
    /// tree) generic store. The package source tree root comes from
    /// `STREAMLIB_PACKAGE_SOURCE` and must be a `file://`
    /// tree — publishing writes files (a static HTTP mount is read-only);
    /// reads are tokenless. Publishing many packages is a script over this
    /// single-package command.
    Publish,
    /// Remove THIS package's build artifacts (run inside the package): the
    /// prebuilt `lib/` dir and generated `_generated_/` trees. Also sweeps any
    /// hand-made `*.slpkg` left in the package dir — `publish` packs to a
    /// tempfile, so nothing streamlib runs writes one here any more.
    Clean,
    /// Reclaim on-the-box build scratch across every materialized package slot,
    /// keeping the loadable artifact. Reclaims each slot's `target/` plus
    /// orphaned staging residue across the app's co-located
    /// `streamlib_modules/`. Unlike `clean` (this package's source dir), this
    /// is a whole-cache reclaim.
    CacheGc {
        /// App root whose `streamlib_modules/` is reclaimed (default: CWD).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// List installed packages (the app's `streamlib_modules/` folder)
    List {
        /// App root whose installed packages are listed (default: CWD).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main(cli))
}

async fn async_main(cli: Cli) -> Result<()> {
    let _logging_guard = streamlib::sdk::logging::init(
        streamlib::sdk::logging::StreamlibLoggingConfig::for_cli("streamlib-cli"),
    )?;

    match cli.command {
        Some(Commands::Setup { action }) => match action {
            SetupCommands::Shell { shell } => commands::setup::shell(shell.as_deref())?,
        },
        Some(Commands::Schemas { action }) => match action {
            SchemasCommands::ValidateProcessor { path } => {
                commands::schema::validate_processor(&path)?
            }
        },
        Some(Commands::Install { dir, no_build }) => {
            commands::install::install(dir.as_deref(), no_build)?
        }
        Some(Commands::Add {
            spec,
            dir,
            expect_sha256,
            no_build,
        }) => commands::add::add(&spec, dir.as_deref(), expect_sha256.as_deref(), no_build)?,
        Some(Commands::Remove { name, dir }) => commands::add::remove(&name, dir.as_deref())?,
        Some(Commands::Pkg { action }) => match action {
            PkgCommands::Publish => commands::pkg::publish()?,
            PkgCommands::Clean => commands::pkg::clean()?,
            PkgCommands::CacheGc { dir } => commands::pkg::cache_gc(dir.as_deref())?,
            PkgCommands::List { dir } => commands::pkg::list(dir.as_deref())?,
        },
        Some(Commands::Link {
            path,
            engine,
            dir,
            skip_verify,
        }) => {
            if engine {
                if dir.is_some() {
                    anyhow::bail!("--dir applies only to a package link, not `link --engine`");
                }
                let consumer_root = std::env::current_dir()?;
                match path {
                    Some(checkout) => commands::link::link(&consumer_root, &checkout, skip_verify)?,
                    None => commands::link::status(&consumer_root)?,
                }
            } else {
                if skip_verify {
                    anyhow::bail!("--skip-verify applies only to `streamlib link --engine`");
                }
                let path = path.ok_or_else(|| {
                    anyhow::anyhow!(
                        "streamlib link needs a package checkout path (or `--engine <checkout>` \
                         for the whole-tree engine link)"
                    )
                })?;
                commands::add::link(&path, dir.as_deref())?;
            }
        }
        Some(Commands::Unlink {
            name,
            engine,
            dir,
            force,
        }) => {
            if engine {
                if name.is_some() {
                    anyhow::bail!(
                        "`unlink --engine` takes no package name (it removes the whole-tree \
                         engine link); drop the name or drop --engine"
                    );
                }
                if dir.is_some() {
                    anyhow::bail!("--dir applies only to a package unlink, not `unlink --engine`");
                }
                let consumer_root = std::env::current_dir()?;
                commands::link::unlink(&consumer_root, force)?;
            } else {
                if force {
                    anyhow::bail!("--force applies only to `streamlib unlink --engine`");
                }
                let name = name.ok_or_else(|| {
                    anyhow::anyhow!(
                        "streamlib unlink needs a package `@org/name` (or `--engine` for the \
                         whole-tree engine link)"
                    )
                })?;
                commands::add::unlink(&name, dir.as_deref())?;
            }
        }
        Some(Commands::Generate {
            runtime,
            output,
            project_dir,
            schema_file,
            schema_dir,
        }) => commands::generate::run(runtime, output, project_dir, schema_file, schema_dir)?,
        None => {
            Cli::parse_from(["streamlib", "--help"]);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The observation verbs live in the wheel's console script now. A Rust
    /// `graph` / `tap` / `logs` / `nodes` / `mcp` reappearing here is a second
    /// client answering to the same name against the same control plane.
    #[test]
    fn the_rust_cli_owns_no_observation_verb() {
        for verb in ["nodes", "graph", "tap", "logs", "mcp", "shutdown"] {
            assert!(
                Cli::try_parse_from(["streamlib", verb]).is_err(),
                "`streamlib {verb}` must not be a Rust CLI subcommand"
            );
        }
    }

    /// The app-launch verbs live in the wheel's console script, which is the
    /// only host with an interpreter to run `app.py` in. A Rust `run` / `dev`
    /// reappearing here is a second launcher answering to the same name.
    #[test]
    fn the_rust_cli_owns_no_app_launch_verb() {
        for verb in ["run", "dev"] {
            assert!(
                Cli::try_parse_from(["streamlib", verb]).is_err(),
                "`streamlib {verb}` belongs to the wheel's console script, not this binary"
            );
        }
    }
}
