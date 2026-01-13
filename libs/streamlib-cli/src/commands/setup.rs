// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Setup commands for configuring the shell environment.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// Get the streamlib home directory (~/.streamlib).
fn streamlib_home() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".streamlib"))
}

/// Configure shell to add streamlib to PATH.
pub fn shell(shell_type: Option<&str>) -> Result<()> {
    let home = streamlib_home()?;
    let bin_dir = home.join("bin");
    let env_file = home.join("env");

    // Create directories if needed
    fs::create_dir_all(&bin_dir).context("Failed to create ~/.streamlib/bin")?;

    // Detect shell if not specified
    let shell = shell_type
        .map(String::from)
        .or_else(|| std::env::var("SHELL").ok())
        .unwrap_or_else(|| "bash".to_string());

    let shell_name = if shell.contains("zsh") {
        "zsh"
    } else if shell.contains("fish") {
        "fish"
    } else {
        "bash"
    };

    // Generate env file content
    let env_content = if shell_name == "fish" {
        format!(
            r#"# StreamLib environment
# Add this to your config.fish:
#   source {}

set -gx PATH "{}" $PATH
"#,
            env_file.display(),
            bin_dir.display()
        )
    } else {
        format!(
            r#"# StreamLib environment
# Add this to your shell rc file:
#   . "{}"

export PATH="{}:$PATH"
"#,
            env_file.display(),
            bin_dir.display()
        )
    };

    // Write env file
    fs::write(&env_file, &env_content).context("Failed to write env file")?;

    println!("Created {}", env_file.display());
    println!();

    // Show instructions based on shell
    match shell_name {
        "zsh" => {
            println!("Add this line to your ~/.zshrc:");
            println!();
            println!("  . \"{}\"", env_file.display());
            println!();
            println!("Then reload your shell:");
            println!();
            println!("  source ~/.zshrc");
        }
        "fish" => {
            println!("Add this line to your ~/.config/fish/config.fish:");
            println!();
            println!("  source {}", env_file.display());
            println!();
            println!("Then reload your shell:");
            println!();
            println!("  source ~/.config/fish/config.fish");
        }
        _ => {
            println!("Add this line to your ~/.bashrc or ~/.bash_profile:");
            println!();
            println!("  . \"{}\"", env_file.display());
            println!();
            println!("Then reload your shell:");
            println!();
            println!("  source ~/.bashrc");
        }
    }

    Ok(())
}
