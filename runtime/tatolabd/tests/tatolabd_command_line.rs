// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolabd` takes no arguments: every one is a usage error, refused before
//! the runtime touches the machine, and `--version` names the build.

use std::process::Command;

#[test]
fn any_argument_is_a_usage_error_naming_it() {
    for (arguments, named_argument) in [
        (&["--graph", "graph.json"][..], "--graph"),
        (&["--project", "."][..], "--project"),
        (&["--interpreter", "/bin/sh"][..], "--interpreter"),
        (&["stream.py"][..], "stream.py"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_tatolabd"))
            .args(arguments)
            .output()
            .expect("tatolabd runs");
        let standard_error = String::from_utf8_lossy(&output.stderr);

        assert_eq!(
            output.status.code(),
            Some(2),
            "{arguments:?}: {standard_error}"
        );
        assert!(
            standard_error.contains(named_argument),
            "{arguments:?}: {standard_error}"
        );
        assert!(output.stdout.is_empty(), "{arguments:?}");
    }
}

#[test]
fn version_names_the_build_and_exits_zero() {
    let output = Command::new(env!("CARGO_BIN_EXE_tatolabd"))
        .arg("--version")
        .output()
        .expect("tatolabd runs");

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("tatolabd {}\n", env!("CARGO_PKG_VERSION"))
    );
}
