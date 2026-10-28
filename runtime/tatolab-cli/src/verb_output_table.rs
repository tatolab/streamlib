// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The tables `streams` and `relays` print.

/// `table_rows` one line each, every column but the last padded to its widest cell and two
/// spaces apart.
pub(crate) fn rendered_padded_table<const COLUMNS: usize>(
    table_rows: &[[String; COLUMNS]],
) -> String {
    let column_widths: Vec<usize> = (0..COLUMNS)
        .map(|column_index| {
            table_rows
                .iter()
                .map(|table_row| table_row[column_index].chars().count())
                .max()
                .unwrap_or_default()
        })
        .collect();
    table_rows
        .iter()
        .map(|table_row| {
            let mut line = String::new();
            for (column_index, cell) in table_row.iter().enumerate() {
                if column_index + 1 == COLUMNS {
                    line.push_str(cell);
                } else {
                    let width = column_widths[column_index];
                    line.push_str(&format!("{cell:<width$}  "));
                }
            }
            format!("{}\n", line.trim_end())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_column_but_the_last_is_padded_to_its_widest_cell() {
        let rows = [
            ["NAME", "STATE", "PROJECT"].map(str::to_owned),
            ["camera", "failed", "/srv/cam"].map(str::to_owned),
            ["a", "", ""].map(str::to_owned),
        ];
        assert_eq!(
            rendered_padded_table(&rows),
            "NAME    STATE   PROJECT\ncamera  failed  /srv/cam\na\n"
        );
    }
}
