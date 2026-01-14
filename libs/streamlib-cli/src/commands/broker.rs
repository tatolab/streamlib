// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Broker management and diagnostics commands.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use streamlib_broker::proto::broker_service_client::BrokerServiceClient;
use streamlib_broker::proto::{
    GetHealthRequest, GetVersionRequest, ListConnectionsRequest, ListProcessorsRequest,
    ListRuntimesRequest,
};
use streamlib_broker::GRPC_PORT;

/// Launchd service label.
const LAUNCHD_LABEL: &str = "com.tatolab.streamlib.broker";

/// Get the streamlib home directory (~/.streamlib).
fn streamlib_home() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".streamlib"))
}

/// Get the plist path for the launchd service.
fn plist_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", LAUNCHD_LABEL)))
}

/// Find the broker binary to install.
fn find_broker_binary(specified: Option<&Path>) -> Result<PathBuf> {
    // If explicitly specified, use that
    if let Some(path) = specified {
        if path.exists() {
            return Ok(path.to_path_buf());
        }
        bail!("Specified broker binary not found: {}", path.display());
    }

    // Try target/release first (workspace build)
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
    if let Some(dir) = manifest_dir {
        let release_path = PathBuf::from(&dir)
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("target").join("release").join("streamlib-broker"));
        if let Some(path) = release_path {
            if path.exists() {
                return Ok(path);
            }
        }
    }

    // Try common locations relative to current dir
    let candidates = [
        PathBuf::from("target/release/streamlib-broker"),
        PathBuf::from("../target/release/streamlib-broker"),
        PathBuf::from("../../target/release/streamlib-broker"),
    ];

    for candidate in &candidates {
        if candidate.exists() {
            return Ok(candidate.clone());
        }
    }

    // Try PATH
    if let Ok(output) = Command::new("which").arg("streamlib-broker").output() {
        if output.status.success() {
            let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path_str.is_empty() {
                return Ok(PathBuf::from(path_str));
            }
        }
    }

    bail!(
        "Could not find broker binary. Either:\n\
         1. Build it with: cargo build --release -p streamlib-broker\n\
         2. Specify the path with: --binary /path/to/streamlib-broker"
    )
}

/// Check if the broker service is running.
fn is_broker_running() -> bool {
    Command::new("launchctl")
        .args(["list", LAUNCHD_LABEL])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Stop the broker service.
fn stop_broker() -> Result<()> {
    let domain = format!("gui/{}", unsafe { libc::getuid() });

    // bootout is the modern way to stop a service
    let output = Command::new("launchctl")
        .args(["bootout", &format!("{}/{}", domain, LAUNCHD_LABEL)])
        .output()
        .context("Failed to run launchctl bootout")?;

    if !output.status.success() {
        // Service might not be running, which is fine
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("Could not find specified service") {
            tracing::warn!("launchctl bootout warning: {}", stderr);
        }
    }

    Ok(())
}

/// Start the broker service.
fn start_broker() -> Result<()> {
    let plist = plist_path()?;
    if !plist.exists() {
        bail!(
            "Plist not found at {}. Run 'streamlib broker install' first.",
            plist.display()
        );
    }

    let domain = format!("gui/{}", unsafe { libc::getuid() });

    let output = Command::new("launchctl")
        .args(["bootstrap", &domain, plist.to_str().unwrap()])
        .output()
        .context("Failed to run launchctl bootstrap")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // "already loaded" is fine
        if !stderr.contains("already loaded") {
            bail!("Failed to bootstrap service: {}", stderr);
        }
    }

    Ok(())
}

/// Generate the launchd plist content.
fn generate_plist(broker_path: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{broker_path}</string>
    </array>
    <key>MachServices</key>
    <dict>
        <key>com.tatolab.streamlib.runtime</key>
        <true/>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/streamlib-broker.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/streamlib-broker.log</string>
</dict>
</plist>
"#,
        label = LAUNCHD_LABEL,
        broker_path = broker_path.display()
    )
}

/// Install the broker service.
pub async fn install(force: bool, binary_path: Option<&Path>) -> Result<()> {
    let broker_version = streamlib_broker::VERSION;
    let home = streamlib_home()?;

    // Check if already installed
    let version_dir = home.join("versions").join(broker_version);
    let installed_binary = version_dir.join("streamlib-broker");
    let bin_symlink = home.join("bin").join("streamlib-broker");

    if installed_binary.exists() && !force {
        println!("Broker v{} is already installed.", broker_version);
        println!("Use --force to reinstall.");

        // Make sure service is running
        if !is_broker_running() {
            println!("Starting broker service...");
            start_broker()?;
            println!("Broker service started.");
        }

        return Ok(());
    }

    // Find source binary
    let source_binary = find_broker_binary(binary_path)?;
    println!("Using broker binary: {}", source_binary.display());

    // Stop existing broker if running
    if is_broker_running() {
        println!("Stopping existing broker...");
        stop_broker()?;
        // Give it a moment to clean up
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    // Create directories
    fs::create_dir_all(&version_dir).context("Failed to create version directory")?;
    fs::create_dir_all(home.join("bin")).context("Failed to create bin directory")?;

    // Copy binary
    println!(
        "Installing broker v{} to {}",
        broker_version,
        installed_binary.display()
    );
    fs::copy(&source_binary, &installed_binary).context("Failed to copy broker binary")?;

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&installed_binary)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&installed_binary, perms)?;
    }

    // Create/update symlink
    if bin_symlink.exists() || bin_symlink.is_symlink() {
        fs::remove_file(&bin_symlink).context("Failed to remove old symlink")?;
    }
    symlink(&installed_binary, &bin_symlink).context("Failed to create symlink")?;
    println!(
        "Created symlink: {} -> {}",
        bin_symlink.display(),
        installed_binary.display()
    );

    // Generate and write plist
    let plist = plist_path()?;
    let plist_content = generate_plist(&bin_symlink);
    fs::write(&plist, plist_content).context("Failed to write plist")?;
    println!("Created launchd plist: {}", plist.display());

    // Start service
    println!("Starting broker service...");
    start_broker()?;

    // Wait for service to be ready
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // Verify it's running
    if is_broker_running() {
        println!();
        println!("Broker v{} installed successfully!", broker_version);
        println!();
        println!("The broker will start automatically on login.");
        println!("Check status with: streamlib broker status");
    } else {
        println!();
        println!("Warning: Broker was installed but may not be running.");
        println!("Check logs at: /tmp/streamlib-broker.log");
    }

    Ok(())
}

/// Uninstall the broker service.
pub async fn uninstall() -> Result<()> {
    let home = streamlib_home()?;
    let plist = plist_path()?;

    // Stop service if running
    if is_broker_running() {
        println!("Stopping broker service...");
        stop_broker()?;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    // Remove plist
    if plist.exists() {
        fs::remove_file(&plist).context("Failed to remove plist")?;
        println!("Removed {}", plist.display());
    }

    // Remove symlink
    let bin_symlink = home.join("bin").join("streamlib-broker");
    if bin_symlink.exists() || bin_symlink.is_symlink() {
        fs::remove_file(&bin_symlink).context("Failed to remove symlink")?;
        println!("Removed {}", bin_symlink.display());
    }

    // Remove versions directory
    let versions_dir = home.join("versions");
    if versions_dir.exists() {
        fs::remove_dir_all(&versions_dir).context("Failed to remove versions directory")?;
        println!("Removed {}", versions_dir.display());
    }

    println!();
    println!("Broker uninstalled successfully.");
    println!();
    println!("Note: The ~/.streamlib directory was not removed.");
    println!("To remove it completely: rm -rf ~/.streamlib");

    Ok(())
}

/// Get the broker gRPC endpoint.
/// Reads from STREAMLIB_BROKER_PORT env var, falls back to default GRPC_PORT.
fn broker_endpoint() -> String {
    let port = std::env::var("STREAMLIB_BROKER_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(GRPC_PORT);
    format!("http://127.0.0.1:{}", port)
}

/// Show broker health and version status.
pub async fn status() -> Result<()> {
    let endpoint = broker_endpoint();
    let mut client = BrokerServiceClient::connect(endpoint.clone())
        .await
        .context("Failed to connect to broker. Is the broker running?")?;

    // Get health
    let health = client
        .get_health(GetHealthRequest {})
        .await
        .context("Failed to get broker health")?
        .into_inner();

    // Get version
    let version = client
        .get_version(GetVersionRequest {})
        .await
        .context("Failed to get broker version")?
        .into_inner();

    println!("Broker Status");
    println!("─────────────────────────────────");
    println!("  Endpoint:   {}", endpoint);
    println!(
        "  Health:     {}",
        if health.healthy {
            "healthy"
        } else {
            "unhealthy"
        }
    );
    println!("  Status:     {}", health.status);
    println!("  Uptime:     {}s", health.uptime_secs);
    println!();
    println!("Version Info");
    println!("─────────────────────────────────");
    println!("  Version:    {}", version.version);
    println!("  Git Commit: {}", version.git_commit);
    println!("  Build Date: {}", version.build_date);

    Ok(())
}

/// List registered runtimes.
pub async fn runtimes() -> Result<()> {
    let endpoint = broker_endpoint();
    let mut client = BrokerServiceClient::connect(endpoint)
        .await
        .context("Failed to connect to broker. Is the broker running?")?;

    let response = client
        .list_runtimes(ListRuntimesRequest {})
        .await
        .context("Failed to list runtimes")?
        .into_inner();

    if response.runtimes.is_empty() {
        println!("No runtimes registered.");
        return Ok(());
    }

    println!("Registered Runtimes ({}):", response.runtimes.len());
    println!("─────────────────────────────────────────────────────────");

    for runtime in &response.runtimes {
        println!("  Runtime: {}", runtime.runtime_id);
        println!("    Processors:  {}", runtime.processor_count);
        println!("    Connections: {}", runtime.connection_count);
        println!("    Age:         {}ms", runtime.registered_at_unix_ms);
        println!();
    }

    Ok(())
}

/// List registered processors.
pub async fn processors(runtime_id: Option<&str>) -> Result<()> {
    let endpoint = broker_endpoint();
    let mut client = BrokerServiceClient::connect(endpoint)
        .await
        .context("Failed to connect to broker. Is the broker running?")?;

    let response = client
        .list_processors(ListProcessorsRequest {
            runtime_id: runtime_id.unwrap_or("").to_string(),
        })
        .await
        .context("Failed to list processors")?
        .into_inner();

    if response.processors.is_empty() {
        if let Some(id) = runtime_id {
            println!("No processors registered for runtime '{}'.", id);
        } else {
            println!("No processors registered.");
        }
        return Ok(());
    }

    let title = if let Some(id) = runtime_id {
        format!(
            "Processors for Runtime '{}' ({}):",
            id,
            response.processors.len()
        )
    } else {
        format!("All Processors ({}):", response.processors.len())
    };

    println!("{}", title);
    println!("─────────────────────────────────────────────────────────");

    for proc in &response.processors {
        println!("  Processor: {}", proc.processor_id);
        println!("    Runtime: {}", proc.runtime_id);
        println!("    Type:    {}", proc.processor_type);
        println!("    State:   {}", proc.bridge_state);
        println!("    Age:     {}ms", proc.registered_at_unix_ms);
        println!();
    }

    Ok(())
}

/// List active connections.
pub async fn connections(runtime_id: Option<&str>) -> Result<()> {
    let endpoint = broker_endpoint();
    let mut client = BrokerServiceClient::connect(endpoint)
        .await
        .context("Failed to connect to broker. Is the broker running?")?;

    let response = client
        .list_connections(ListConnectionsRequest {
            runtime_id: runtime_id.unwrap_or("").to_string(),
        })
        .await
        .context("Failed to list connections")?
        .into_inner();

    if response.connections.is_empty() {
        if let Some(id) = runtime_id {
            println!("No connections for runtime '{}'.", id);
        } else {
            println!("No active connections.");
        }
        return Ok(());
    }

    let title = if let Some(id) = runtime_id {
        format!(
            "Connections for Runtime '{}' ({}):",
            id,
            response.connections.len()
        )
    } else {
        format!("All Connections ({}):", response.connections.len())
    };

    println!("{}", title);
    println!("─────────────────────────────────────────────────────────");

    for conn in &response.connections {
        println!("  Connection: {}", conn.connection_id);
        println!("    Runtime:   {}", conn.runtime_id);
        println!("    Processor: {}", conn.processor_id);
        println!("    Role:      {}", conn.role);
        println!("    Age:       {}ms", conn.established_at_unix_ms);
        if conn.frames_transferred > 0 || conn.bytes_transferred > 0 {
            println!("    Frames:    {}", conn.frames_transferred);
            println!("    Bytes:     {}", conn.bytes_transferred);
        }
        println!();
    }

    Ok(())
}
