// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolabd`'s three flags.

use std::path::PathBuf;

/// `tatolabd --stream-graph <file> --project <dir> --interpreter <path>`.
#[derive(Debug, clap::Parser)]
#[command(
    name = "tatolabd",
    about = "Host one stream in the foreground: the engine, its built-ins and the local API."
)]
pub(crate) struct TatolabdCommandLine {
    /// The stream's graph, as JSON: what `compile_stream_to_graph` returns.
    #[arg(long = "stream-graph", value_name = "FILE")]
    pub(crate) stream_graph: PathBuf,

    /// The stream's project directory: its Python nodes import from here, and
    /// every processor interpreter runs here.
    #[arg(long = "project", value_name = "DIR")]
    pub(crate) project: PathBuf,

    /// The project's venv interpreter, which every processor interpreter is an
    /// exec of.
    #[arg(long = "interpreter", value_name = "PATH")]
    pub(crate) interpreter: PathBuf,
}
