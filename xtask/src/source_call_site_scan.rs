// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Finding calls in Rust source by a cheap text scan, for the gates that refuse
//! a call by what its arguments carry.

/// One call found in source.
#[derive(Debug, PartialEq, Eq)]
pub struct ScannedCallSite<'code> {
    /// The 1-based line the call starts on.
    pub line: usize,
    /// Everything between the call's own parentheses.
    pub argument_text: &'code str,
    /// The whole call, with each run of whitespace collapsed to one space.
    pub collapsed_call_text: String,
}

/// Every call in `code` spelled `call_prefix` — which ends at its open
/// parenthesis — in source order. A call nested inside another's arguments is
/// not reported, and a call whose parentheses never close ends the scan.
pub fn call_sites_of<'code>(code: &'code str, call_prefix: &str) -> Vec<ScannedCallSite<'code>> {
    let mut call_sites = Vec::new();
    let mut search_from = 0usize;
    while let Some(offset) = code[search_from..].find(call_prefix) {
        let call_start = search_from + offset;
        let open_paren = call_start + call_prefix.len() - 1;
        let Some(close_paren) = matching_close_paren(code, open_paren) else {
            break;
        };
        call_sites.push(ScannedCallSite {
            line: code[..call_start].matches('\n').count() + 1,
            argument_text: &code[open_paren + 1..close_paren],
            collapsed_call_text: code[call_start..=close_paren]
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        });
        search_from = close_paren + 1;
    }
    call_sites
}

/// Blank every line `is_exempt` accepts while keeping the line count, so a
/// reported line number still points at the source.
pub fn blank_out_lines(body: &str, is_exempt: impl Fn(&str) -> bool) -> String {
    body.lines()
        .map(|line| if is_exempt(line) { "" } else { line })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether `line` is a comment and nothing else.
pub fn is_a_whole_line_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

fn matching_close_paren(code: &str, open_paren: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, byte) in code.as_bytes().iter().enumerate().skip(open_paren) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(offset);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_split_across_lines_is_reported_at_its_first_line_with_its_arguments() {
        let code = "fn f() {\n    let x = libc::pipe2(\n        fds.as_mut_ptr(),\n        flags,\n    );\n}\n";
        let call_sites = call_sites_of(code, "libc::pipe2(");
        assert_eq!(call_sites.len(), 1);
        assert_eq!(call_sites[0].line, 2);
        assert!(call_sites[0].argument_text.contains("flags"));
        assert_eq!(
            call_sites[0].collapsed_call_text,
            "libc::pipe2( fds.as_mut_ptr(), flags, )"
        );
    }

    #[test]
    fn a_call_whose_parentheses_never_close_ends_the_scan() {
        assert!(call_sites_of("libc::dup(fd", "libc::dup(").is_empty());
    }

    #[test]
    fn a_blanked_line_keeps_every_later_line_number() {
        let body = "// libc::dup(fd)\nlibc::dup(fd);\n";
        let code = blank_out_lines(body, is_a_whole_line_comment);
        let call_sites = call_sites_of(&code, "libc::dup(");
        assert_eq!(call_sites.len(), 1);
        assert_eq!(call_sites[0].line, 2);
    }
}
