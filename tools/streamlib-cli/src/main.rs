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
    /// Stream a runtime's on-disk JSONL log file in pretty format, or — with
    /// `--url` — collect a bounded sample of a running node's live event stream
    /// via its control plane.
    Logs {
        /// Runtime ID to read logs for. Omit when using `--list` or `--url`.
        #[arg(value_name = "RUNTIME_ID", required_unless_present_any = ["list", "url"])]
        runtime_id: Option<String>,

        /// Control-plane URL: collect a bounded sample of the running node's
        /// live event stream (all topics) via its `POST /mcp` instead of
        /// reading an on-disk JSONL log file.
        #[arg(long, value_name = "URL", conflicts_with_all = ["runtime_id", "list", "follow", "processor", "pipeline", "rhi", "level", "source", "intercepted_only", "since"])]
        url: Option<String>,

        /// (control-plane mode) Max events to collect before returning.
        #[arg(long, value_name = "COUNT", requires = "url")]
        count: Option<usize>,

        /// Enumerate available runtime log files instead of streaming one.
        #[arg(long, conflicts_with_all = ["runtime_id", "follow"])]
        list: bool,

        /// Follow the log file as new records land (like `tail -F`).
        #[arg(short = 'f', long)]
        follow: bool,

        /// Filter by processor ID.
        #[arg(long, value_name = "ID")]
        processor: Option<String>,

        /// Filter by pipeline ID.
        #[arg(long, value_name = "ID")]
        pipeline: Option<String>,

        /// Show only RHI operations (records with `rhi_op` set).
        #[arg(long)]
        rhi: bool,

        /// Filter by minimum severity level.
        #[arg(long, value_name = "LEVEL", value_parser = ["trace", "debug", "info", "warn", "error"])]
        level: Option<String>,

        /// Filter by source runtime.
        #[arg(long, value_name = "SOURCE", value_parser = ["rust", "python", "deno"])]
        source: Option<String>,

        /// Show only intercepted records (captured stdout/stderr/print/console.log).
        #[arg(long = "intercepted-only")]
        intercepted_only: bool,

        /// (Orchestrator-only) Show logs since a duration ago. Not supported in offline mode.
        #[arg(long, value_name = "DURATION")]
        since: Option<String>,
    },

    /// Speak the Model Context Protocol over stdio so any MCP host can spawn
    /// StreamLib's agent-operable tools (`claude mcp add streamlib -- streamlib
    /// mcp`).
    ///
    /// Default (no `--attach`): host a fresh in-process runtime and serve its
    /// MCP over stdio — zero-config; the host gets an operable runtime, torn
    /// down when it closes stdin. `--attach <url>` instead bridges stdio to a
    /// running runtime's `POST {url}/mcp` to operate an existing live pipeline.
    /// Exposes the same 7 tools as `POST /mcp`: graph / submit_processor /
    /// replace_processor / remove_processor / connect / tap / logs. Auth is off
    /// by construction on the in-process path; `--attach` forwards a token from
    /// `STREAMLIB_MCP_TOKEN` when set.
    Mcp {
        /// Bridge stdio to this running runtime's `POST {url}/mcp` instead of
        /// hosting an in-process runtime.
        #[arg(long, value_name = "URL")]
        attach: Option<String>,
    },

    /// Export a running node's live graph (processors, links, states, metrics)
    /// as JSON via its control plane.
    Graph {
        /// Control-plane base URL of the target node (its `POST /mcp` host).
        #[arg(long, value_name = "URL")]
        url: String,
    },

    /// Author a processor from source and submit it into a running node's graph.
    ///
    /// Transactional: registers the source, instantiates the first discovered
    /// processor, and optionally wires it to existing graph ports; a failed
    /// wiring rolls the whole submit back.
    Submit {
        /// Control-plane base URL of the target node (its `POST /mcp` host).
        #[arg(long, value_name = "URL")]
        url: String,

        /// Source language. `rust` is rejected for live submit — it's a full
        /// cargo build, not a live graph mutation.
        #[arg(long, value_parser = ["python", "typescript", "deno"])]
        language: String,

        /// Processor module source: `@<file>` or a plain path reads the file;
        /// `-` or an omitted flag reads stdin.
        #[arg(long, value_name = "SOURCE")]
        source: Option<String>,

        /// The `@session/<name>` segment to mint under (derived from the
        /// processor type name if omitted).
        #[arg(long, value_name = "NAME")]
        requested_name: Option<String>,

        /// The PascalCase processor type the source defines (derived from the
        /// requested name if omitted).
        #[arg(long, value_name = "TYPE")]
        processor_type_name: Option<String>,

        /// Config JSON applied when the processor is instantiated (default `{}`).
        #[arg(long, value_name = "JSON")]
        config: Option<String>,

        /// Wire the new processor after instantiation. Repeatable:
        /// `local_port:role:peer_processor:peer_port` (role ∈ `output`|`input`).
        #[arg(long, value_name = "SPEC")]
        connect: Vec<String>,
    },

    /// Swap a running node's `@session/<name>` source registration for a
    /// replacement.
    ///
    /// Type-level: this swaps the SOURCE REGISTRATION only — already-running
    /// instances are NOT swapped in place; they keep running the prior source
    /// until removed and re-instantiated. Transactional: a failed replacement
    /// restores the prior registration.
    Replace {
        /// Control-plane base URL of the target node (its `POST /mcp` host).
        #[arg(long, value_name = "URL")]
        url: String,

        /// The `@session/<name>@<range>` module to replace, e.g.
        /// `@session/widget@*`.
        #[arg(long, value_name = "MODULE")]
        target_session_module: String,

        /// Replacement source language. `rust` is rejected for live submit.
        #[arg(long, value_parser = ["python", "typescript", "deno"])]
        language: String,

        /// Replacement source: `@<file>` or a plain path reads the file; `-` or
        /// an omitted flag reads stdin.
        #[arg(long, value_name = "SOURCE")]
        source: Option<String>,

        /// The `@session/<name>` segment to mint under.
        #[arg(long, value_name = "NAME")]
        requested_name: Option<String>,

        /// The PascalCase processor type the replacement source defines.
        #[arg(long, value_name = "TYPE")]
        processor_type_name: Option<String>,
    },

    /// Connect an output port to an input port between two running processors.
    Connect {
        /// Control-plane base URL of the target node (its `POST /mcp` host).
        #[arg(long, value_name = "URL")]
        url: String,

        #[arg(long, value_name = "PROCESSOR")]
        from_processor: String,

        #[arg(long, value_name = "PORT")]
        from_port: String,

        #[arg(long, value_name = "PROCESSOR")]
        to_processor: String,

        #[arg(long, value_name = "PORT")]
        to_port: String,
    },

    /// Attach a read-only tap to a running node's channel and collect a bounded
    /// sample of raw bags (a hex preview plus byte length per bag).
    Tap {
        /// Control-plane base URL of the target node (its `POST /mcp` host).
        #[arg(long, value_name = "URL")]
        url: String,

        /// Channel data-service name, e.g. `{source_processor}/{source_output_port}`.
        #[arg(value_name = "CHANNEL")]
        channel: String,

        /// Number of bags to collect before returning.
        #[arg(long, value_name = "COUNT")]
        count: Option<usize>,
    },

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
    ///   the schema-tier `cargo add`. `pkg build` reconciles it against code.
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

    /// Remove a package from this app's streamlib_modules/ folder, or — with
    /// `--url` — remove a processor instance from a running node's graph via
    /// its control plane.
    ///
    /// Local mode deletes `streamlib_modules/@org/name/` and drops the
    /// package's entry from the app's `streamlib.lock`. Control-plane mode
    /// (`--url` + `--processor-id`) removes a live processor instance by id.
    Remove {
        /// Canonical `@org/name` reference to remove (local mode). Omit with
        /// `--url`.
        #[arg(required_unless_present = "url", conflicts_with = "url")]
        name: Option<String>,

        /// App root to anchor streamlib_modules/ + streamlib.lock at
        /// (default: current working directory, no walk-up).
        #[arg(long)]
        dir: Option<PathBuf>,

        /// Control-plane URL: remove a processor instance from the running node
        /// at this URL (its `POST /mcp` host) instead of a package.
        #[arg(long, value_name = "URL")]
        url: Option<String>,

        /// Processor instance id to remove (control-plane mode; requires `--url`).
        #[arg(long, value_name = "ID", requires = "url")]
        processor_id: Option<String>,
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
    /// Build THIS package into a source-only `.slpkg` (run inside the package).
    ///
    /// Bundles source only — no compilation, no prebuilt cdylib, nothing
    /// path-related. The consumer builds it from source on their host
    /// (`streamlib add` / runtime registry resolution), pulling every dep
    /// from the registry. The artifact is for hand-off (email it, hand it to
    /// a runtime); `publish` repacks independently.
    Build {
        /// Output file path (default: {name}-{version}.slpkg in the package dir)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Publish THIS package to the registry (run inside the package).
    ///
    /// Always repacks a fresh source-only `.slpkg` (never trusts an existing
    /// artifact) and writes it into the static registry tree. The registry
    /// tree root comes from `STREAMLIB_REGISTRY_URL` and must be a `file://`
    /// tree — publishing writes files (a static HTTP mount is read-only);
    /// reads are tokenless. Publishing many packages is a script over this
    /// single-package command.
    Publish,
    /// Remove THIS package's build/pack artifacts (run inside the package):
    /// any `*.slpkg`, the prebuilt `lib/` dir, and generated `_generated_/` trees.
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
    /// Inspect a .slpkg package (show manifest without installing)
    Inspect {
        /// Path to .slpkg file
        path: PathBuf,
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

/// The short-lived-CLI logging config for `command`. Every subcommand mirrors
/// pretty logs to stdout except `mcp`, which speaks a byte protocol on stdout
/// and so routes its mirror to stderr to keep fd 1 carrying only MCP JSON-RPC
/// frames.
fn logging_config_for(command: &Option<Commands>) -> streamlib::sdk::logging::StreamlibLoggingConfig {
    if matches!(command, Some(Commands::Mcp { .. })) {
        streamlib::sdk::logging::StreamlibLoggingConfig::for_stdio_protocol("streamlib-cli")
    } else {
        streamlib::sdk::logging::StreamlibLoggingConfig::for_cli("streamlib-cli")
    }
}

async fn async_main(cli: Cli) -> Result<()> {
    let _logging_guard = streamlib::sdk::logging::init(logging_config_for(&cli.command))?;

    match cli.command {
        Some(Commands::Logs {
            runtime_id,
            url,
            count,
            list,
            follow,
            processor,
            pipeline,
            rhi,
            level,
            source,
            intercepted_only,
            since,
        }) => {
            if let Some(url) = url {
                return commands::control::logs(&url, count);
            }
            commands::logs::run(commands::logs::LogsArgs {
                runtime_id: runtime_id.as_deref(),
                list,
                follow,
                processor: processor.as_deref(),
                pipeline: pipeline.as_deref(),
                rhi,
                level: level.as_deref(),
                source: source.as_deref(),
                intercepted_only,
                since: since.as_deref(),
            })
            .await?
        }
        Some(Commands::Mcp { attach }) => commands::mcp::run(attach).await?,
        Some(Commands::Graph { url }) => commands::control::graph(&url)?,
        Some(Commands::Submit {
            url,
            language,
            source,
            requested_name,
            processor_type_name,
            config,
            connect,
        }) => commands::control::submit(commands::control::SubmitArgs {
            url,
            language,
            source,
            requested_name,
            processor_type_name,
            config,
            connect,
        })?,
        Some(Commands::Replace {
            url,
            target_session_module,
            language,
            source,
            requested_name,
            processor_type_name,
        }) => commands::control::replace(commands::control::ReplaceArgs {
            url,
            target_session_module,
            language,
            source,
            requested_name,
            processor_type_name,
        })?,
        Some(Commands::Connect {
            url,
            from_processor,
            from_port,
            to_processor,
            to_port,
        }) => {
            commands::control::connect(&url, &from_processor, &from_port, &to_processor, &to_port)?
        }
        Some(Commands::Tap {
            url,
            channel,
            count,
        }) => commands::control::tap(&url, &channel, count)?,
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
        Some(Commands::Remove {
            name,
            dir,
            url,
            processor_id,
        }) => match url {
            Some(url) => {
                let processor_id = processor_id.ok_or_else(|| {
                    anyhow::anyhow!("`remove --url <url>` requires `--processor-id <id>`")
                })?;
                commands::control::remove(&url, &processor_id)?
            }
            None => {
                let name = name.ok_or_else(|| {
                    anyhow::anyhow!("`remove` requires either `<name>` (local mode) or `--url` (control-plane mode)")
                })?;
                commands::add::remove(&name, dir.as_deref())?
            }
        },
        Some(Commands::Pkg { action }) => match action {
            PkgCommands::Build { output } => commands::pkg::build(output.as_deref())?,
            PkgCommands::Publish => commands::pkg::publish()?,
            PkgCommands::Clean => commands::pkg::clean()?,
            PkgCommands::CacheGc { dir } => commands::pkg::cache_gc(dir.as_deref())?,
            PkgCommands::Inspect { path } => commands::pkg::inspect(&path)?,
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
    use streamlib::sdk::logging::PrettyMirrorStream;

    /// The `mcp` subcommand's stdio transport requires fd 1 carry only MCP
    /// JSON-RPC frames — its pretty log mirror MUST route to stderr. Reverting
    /// the mcp branch of [`logging_config_for`] (mirror back to stdout) goes red
    /// here, catching a regression that would re-pollute the protocol stream.
    #[test]
    fn mcp_subcommand_mirrors_logs_to_stderr_not_stdout() {
        let mcp = logging_config_for(&Some(Commands::Mcp { attach: None }));
        assert_eq!(
            mcp.pretty_mirror_stream,
            PrettyMirrorStream::Stderr,
            "mcp stdout is a protocol channel — the pretty mirror must go to stderr"
        );
    }

    /// Every path other than `mcp` keeps the human-facing stdout mirror.
    #[test]
    fn non_mcp_invocation_mirrors_logs_to_stdout() {
        assert_eq!(
            logging_config_for(&None).pretty_mirror_stream,
            PrettyMirrorStream::Stdout
        );
    }
}
