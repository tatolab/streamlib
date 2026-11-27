// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The line a GPU fixture writes to the file its `output_path` names: `OK…` or `ERR:<why>`,
//! which an integration test reads.

use streamlib::sdk::error::{Error, Result};

/// The line a fixture wrote to the file it names: the computed value a fixture builds from its
/// `output_path`, so a new path writes the line again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultWrittenToItsFile {
    /// The file the line was written to.
    pub output_path: String,
    /// The line written.
    pub line: String,
}

/// Write `outcome` to `output_path` as `OK…` (the `Ok` value after `OK`) or `ERR:<why>`, for the
/// fixture `node`.
pub fn the_result_written_to_its_file(
    node: &str,
    output_path: &str,
    outcome: &Result<String>,
) -> Result<ResultWrittenToItsFile> {
    let line = match outcome {
        Ok(after_ok) => format!("OK{after_ok}"),
        Err(why) => format!("ERR:{why}"),
    };
    std::fs::write(output_path, &line)
        .map_err(|why| Error::Runtime(format!("{node}: write {output_path}: {why}")))?;
    Ok(ResultWrittenToItsFile {
        output_path: output_path.to_owned(),
        line,
    })
}
