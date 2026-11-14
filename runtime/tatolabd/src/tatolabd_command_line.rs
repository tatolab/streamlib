// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `tatolabd` takes no arguments: streams are loaded into it over its local
//! API, never named at its start.

/// `tatolabd`, with only `--help` and `--version`.
#[derive(Debug, clap::Parser)]
#[command(
    name = "tatolabd",
    version,
    about = "The machine's runtime: hosts every stream loaded into it over its local API, and \
             re-loads the recorded ones at its start. Runs in the foreground until it is stopped by a \
             signal."
)]
pub(crate) struct TatolabdCommandLine {}
