// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a GPU fixture writes to the file its `output_path` names: `OK` and the lines after it,
//! one a line, or `ERR:<why>`, which an integration test reads.

use streamlib::sdk::error::{Error, Result};

/// What a fixture wrote to the file it names: the computed value a fixture builds from its
/// `output_path`, so a new path writes the line again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultWrittenToItsFile {
    /// The file it was written to.
    output_path: String,
    /// What was written.
    written: String,
}

/// Write `outcome` to `output_path` as `OK` and each line of the `Ok` value after it, one a line,
/// or as `ERR:<why>`, for the fixture `node`.
pub fn the_result_written_to_its_file(
    node: &str,
    output_path: &str,
    outcome: &Result<Vec<String>>,
) -> Result<ResultWrittenToItsFile> {
    let written = match outcome {
        Ok(lines_after_ok) => std::iter::once("OK")
            .chain(lines_after_ok.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Err(why) => format!("ERR:{why}"),
    };
    std::fs::write(output_path, &written)
        .map_err(|why| Error::Runtime(format!("{node}: write {output_path}: {why}")))?;
    Ok(ResultWrittenToItsFile {
        output_path: output_path.to_owned(),
        written,
    })
}
