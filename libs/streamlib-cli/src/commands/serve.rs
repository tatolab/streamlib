// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Locate the `streamlib-runtime` binary.
///
/// Search order:
/// 1. `STREAMLIB_RUNTIME_BIN` env var
/// 2. Same directory as the current executable
/// 3. `~/.streamlib/bin/streamlib-runtime`
/// 4. `streamlib-runtime` in PATH
fn find_runtime_binary() -> Result<PathBuf> {
    // 1. Env var override (used in dev mode)
    if let Ok(path) = std::env::var("STREAMLIB_RUNTIME_BIN") {
        let p = PathBuf::from(&path);
        if p.exists() {
            return Ok(p);
        }
        // Env var set but file doesn't exist — warn but continue searching
        tracing::warn!(
            "STREAMLIB_RUNTIME_BIN={} does not exist, searching other locations",
            path
        );
    }

    // 2. Same directory as the current executable
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("streamlib-runtime");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    // 3. ~/.streamlib/bin/streamlib-runtime
    if let Some(home) = dirs::home_dir() {
        let candidate = home
            .join(".streamlib")
            .join("bin")
            .join("streamlib-runtime");
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    // 4. Search PATH
    if let Ok(output) = Command::new("which").arg("streamlib-runtime").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(PathBuf::from(path));
            }
        }
    }

    bail!(
        "Could not find 'streamlib-runtime' binary.\n\
         \n\
         Searched:\n\
         1. STREAMLIB_RUNTIME_BIN env var\n\
         2. Same directory as this executable\n\
         3. ~/.streamlib/bin/streamlib-runtime\n\
         4. PATH\n\
         \n\
         Install with: cargo install --path libs/streamlib-runtime"
    )
}

/// Spawn a `streamlib-runtime` process.
pub fn run(
    host: String,
    port: u16,
    graph_file: Option<PathBuf>,
    plugins: Vec<PathBuf>,
    plugin_dir: Option<PathBuf>,
    name: Option<String>,
    daemon: bool,
) -> Result<()> {
    let runtime_bin = find_runtime_binary()?;

    let mut cmd = Command::new(&runtime_bin);

    // Forward arguments
    cmd.arg("--host").arg(&host);
    cmd.arg("--port").arg(port.to_string());

    if let Some(ref n) = name {
        cmd.arg("--name").arg(n);
    }

    if let Some(ref path) = graph_file {
        cmd.arg("--graph-file").arg(path);
    }

    for plugin in &plugins {
        cmd.arg("--plugin").arg(plugin);
    }

    if let Some(ref dir) = plugin_dir {
        cmd.arg("--plugin-dir").arg(dir);
    }

    if daemon {
        cmd.arg("--daemon");
    }

    // Forward relevant env vars to child process
    for var in &[
        "STREAMLIB_HOME",
        "STREAMLIB_BROKER_PORT",
        "STREAMLIB_XPC_SERVICE_NAME",
        "STREAMLIB_DEV_MODE",
        "RUST_LOG",
    ] {
        if let Ok(val) = std::env::var(var) {
            cmd.env(var, val);
        }
    }

    if daemon {
        // Daemon mode: spawn detached and poll /health for readiness
        let mut child = cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| {
                format!("Failed to spawn runtime binary: {}", runtime_bin.display())
            })?;

        // Poll /health endpoint for readiness.
        // The runtime binary daemonizes by forking — the parent exits with
        // code 0 while the child continues.  So an exit code of 0 from our
        // spawned process is expected and means the daemon forked successfully.
        let health_url = format!("http://{}:{}/health", host, port);
        let mut ready = false;

        for _ in 0..30 {
            std::thread::sleep(std::time::Duration::from_millis(500));

            // Check if child exited early
            if let Some(status) = child.try_wait()? {
                if !status.success() {
                    bail!("Runtime process exited immediately with status: {}", status);
                }
                // Exit code 0 is expected (daemonize parent exits after fork).
                // Continue polling health to confirm the daemon child started.
            }

            // Try health check
            if let Ok(output) = Command::new("curl")
                .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", &health_url])
                .output()
            {
                let code = String::from_utf8_lossy(&output.stdout);
                if code.trim() == "200" {
                    ready = true;
                    break;
                }
            }
        }

        if ready {
            let display_name = name.unwrap_or_else(|| "runtime".to_string());
            println!("runtime/{} started (pid {})", display_name, child.id());
            println!("  API: http://{}:{}", host, port);
            println!();
            println!("Next steps:");
            println!("  streamlib logs -r {} -f", display_name);
            println!("  streamlib runtimes list");
        } else {
            println!(
                "Runtime spawned (pid {}) but health check not responding after 15s",
                child.id()
            );
            println!("  Check: http://{}:{}/health", host, port);
        }
    } else {
        // Foreground mode: inherit stdio and wait for exit
        let status = cmd
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()
            .with_context(|| {
                format!("Failed to spawn runtime binary: {}", runtime_bin.display())
            })?;

        if !status.success() {
            bail!("Runtime process exited with status: {}", status);
        }
    }

    Ok(())
}
