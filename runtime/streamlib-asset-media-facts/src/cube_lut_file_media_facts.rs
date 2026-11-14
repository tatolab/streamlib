// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! `.cube` colour lookup tables (Adobe Cube LUT Specification 1.0): the table's size and domain
//! from the keyword lines before its first entry.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use streamlib_runtime_client_contract::local_api_wire_contract::{
    AssetLutFactsOnTheWire, AssetMediaFactsOnTheWire,
};

use crate::{AssetMediaFactsReadFromTheFile, AssetMediaFactsUnreadable};

/// How many lines before the first entry a header may take.
const LONGEST_CUBE_HEADER_LINES: usize = 1_000;

fn three_numbers_of(words: &[&str]) -> Option<[f64; 3]> {
    match words {
        [red, green, blue] => Some([red.parse().ok()?, green.parse().ok()?, blue.parse().ok()?]),
        _ => None,
    }
}

pub(crate) fn read_a_cube_lut_file(
    cube_file_path: &Path,
) -> Result<AssetMediaFactsReadFromTheFile, AssetMediaFactsUnreadable> {
    let file = File::open(cube_file_path).map_err(|why| {
        AssetMediaFactsUnreadable::because(format!("the file could not be opened: {why}"))
    })?;
    let mut lut = AssetLutFactsOnTheWire {
        lut_1d_size: None,
        lut_3d_size: None,
        domain_min: [0.0; 3],
        domain_max: [1.0; 3],
    };
    for line in BufReader::new(file).lines().take(LONGEST_CUBE_HEADER_LINES) {
        let line = line.map_err(|why| {
            AssetMediaFactsUnreadable::because(format!("the file is not text: {why}"))
        })?;
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            [] => {}
            [first, ..] if first.starts_with('#') => {}
            ["LUT_1D_SIZE", size] => lut.lut_1d_size = size.parse().ok(),
            ["LUT_3D_SIZE", size] => lut.lut_3d_size = size.parse().ok(),
            ["DOMAIN_MIN", rest @ ..] => {
                lut.domain_min = three_numbers_of(rest).unwrap_or(lut.domain_min)
            }
            ["DOMAIN_MAX", rest @ ..] => {
                lut.domain_max = three_numbers_of(rest).unwrap_or(lut.domain_max)
            }
            [first, ..] if first.parse::<f64>().is_ok() => break,
            _ => {}
        }
    }
    if lut.lut_1d_size.is_none() && lut.lut_3d_size.is_none() {
        return Err(AssetMediaFactsUnreadable::because(
            "it states neither `LUT_1D_SIZE` nor `LUT_3D_SIZE` before its first entry",
        ));
    }
    Ok(AssetMediaFactsReadFromTheFile {
        media_facts: AssetMediaFactsOnTheWire {
            lut: Some(lut),
            ..AssetMediaFactsOnTheWire::default()
        },
        preview_picture: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AssetMediaFactsReadersOfThisRuntime, read_the_media_facts_of_an_asset_file};

    #[test]
    fn a_cube_reads_its_size_and_domain_from_its_header() {
        let directory = tempfile::tempdir().unwrap();
        let cube = directory.path().join("film.cube");
        std::fs::write(
            &cube,
            "# Created by hand\nTITLE \"Film\"\nLUT_3D_SIZE 33\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n\n0.0 0.0 0.0\n",
        )
        .unwrap();
        let read = read_the_media_facts_of_an_asset_file(
            &cube,
            &AssetMediaFactsReadersOfThisRuntime::default(),
        )
        .unwrap();
        assert_eq!(
            read.media_facts.lut,
            Some(AssetLutFactsOnTheWire {
                lut_1d_size: None,
                lut_3d_size: Some(33),
                domain_min: [0.0; 3],
                domain_max: [1.0; 3],
            })
        );
    }

    #[test]
    fn a_cube_with_no_size_is_unreadable() {
        let directory = tempfile::tempdir().unwrap();
        let cube = directory.path().join("empty.cube");
        std::fs::write(&cube, "TITLE \"nothing\"\n0.1 0.2 0.3\n").unwrap();
        assert!(
            read_the_media_facts_of_an_asset_file(
                &cube,
                &AssetMediaFactsReadersOfThisRuntime::default()
            )
            .is_err()
        );
    }
}
