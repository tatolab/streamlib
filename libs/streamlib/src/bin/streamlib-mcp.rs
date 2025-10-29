//! streamlib MCP Server Binary
//!
//! This binary starts an MCP server that exposes streamlib processors
//! to AI agents like Claude Code.
//!
//! Runs in **application mode** with a live StreamRuntime, enabling AI agents to:
//! - List running processors (list_processors)
//! - Remove processors dynamically (remove_processor)
//! - List connections (list_connections)
//! - Discover available processor types (discovery mode features)
//!
//! Usage:
//! ```bash
//! # Install the binary
//! cargo install streamlib --features mcp
//!
//! # Stdio transport (default, for Claude Desktop integration)
//! streamlib-mcp
//!
//! # HTTP transport (for remote MCP tools, defaults to port 3050)
//! streamlib-mcp --http
//! # Or specify a custom port:
//! streamlib-mcp --http --port 3060
//! ```

use streamlib::{global_registry, mcp::McpServer};
use streamlib::core::StreamRuntime;
use streamlib::{request_camera_permission, request_display_permission, request_audio_permission};
use clap::Parser;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

#[derive(Parser, Debug)]
#[command(name = "streamlib-mcp")]
#[command(about = "MCP server for streamlib processors")]
#[command(version)]
struct Args {
    /// Use HTTP transport instead of stdio
    #[arg(long)]
    http: bool,

    /// Port to bind to (HTTP mode only)
    #[arg(long, default_value = "3050")]
    port: u16,

    /// Host to bind to (HTTP mode only)
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Allow all permissions (camera, display, audio)
    #[arg(long)]
    allow_all: bool,

    /// Allow camera access (requests permission upfront)
    #[arg(long)]
    allow_camera: bool,

    /// Allow display window creation
    #[arg(long)]
    allow_display: bool,

    /// Allow audio input (microphone access)
    #[arg(long)]
    allow_audio: bool,
}

fn main() -> anyhow::Result<()> {
    // Parse CLI arguments on main thread
    let args = Args::parse();

    // Initialize tracing (logs to stderr so they don't interfere with stdio MCP protocol)
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!("Starting streamlib MCP server");

    // Request permissions on main thread BEFORE entering async runtime
    // This ensures platform permission dialogs appear correctly

    // --allow-all overrides specific flags
    let allow_camera = args.allow_all || args.allow_camera;
    let allow_display = args.allow_all || args.allow_display;
    let allow_audio = args.allow_all || args.allow_audio;

    let mut permissions = std::collections::HashSet::new();

    if allow_camera {
        tracing::info!("Requesting camera permission...");
        if request_camera_permission()? {
            tracing::info!("Camera permission granted");
            permissions.insert("camera".to_string());
        } else {
            anyhow::bail!("Camera permission denied by user");
        }
    }

    if allow_display {
        tracing::info!("Requesting display permission...");
        if request_display_permission()? {
            tracing::info!("Display permission granted");
            permissions.insert("display".to_string());
        } else {
            anyhow::bail!("Display permission denied by user");
        }
    }

    if allow_audio {
        tracing::info!("Requesting audio permission...");
        if request_audio_permission()? {
            tracing::info!("Audio permission granted");
            permissions.insert("audio".to_string());
        } else {
            anyhow::bail!("Audio permission denied by user");
        }
    }

    // Now enter async runtime with permissions already granted
    // IMPORTANT: Run tokio on a background thread and keep main thread for CFRunLoop
    // This allows GCD exec_sync() to work properly
    let (result_tx, result_rx) = std::sync::mpsc::channel();

    // Spawn tokio runtime on a background thread
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(async_main(args, permissions));
        result_tx.send(result).ok();
    });

    // Run CFRunLoop on main thread to process GCD dispatches
    tracing::info!("Main thread now running CFRunLoop for GCD dispatches");

    #[cfg(target_os = "macos")]
    {
        use core_foundation::runloop::{CFRunLoop, kCFRunLoopDefaultMode};
        use std::time::Duration;

        // Poll for completion while processing run loop
        loop {
            // Check if async_main completed
            if let Ok(result) = result_rx.try_recv() {
                return result;
            }

            // Process run loop events for a short time (100ms)
            // This allows GCD exec_sync() calls to complete
            unsafe {
                CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, Duration::from_millis(100), true);
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        // Non-macOS: just wait for result
        result_rx.recv().unwrap()
    }
}

async fn async_main(args: Args, permissions: std::collections::HashSet<String>) -> anyhow::Result<()> {
    tracing::info!("Entered async runtime");

    // Get the global processor registry
    // Built-in processors (CameraProcessor, DisplayProcessor) are automatically registered
    // via inventory at compile-time. Any additional processors submitted with inventory::submit!
    // will also be registered automatically.
    let registry = global_registry();

    tracing::info!("Auto-registered {} processors from inventory", {
        let reg = registry.lock().unwrap();
        reg.list().len()
    });

    // Create and start StreamRuntime (application mode)
    tracing::info!("Creating StreamRuntime at 60 FPS");
    let mut runtime = StreamRuntime::new(60.0);

    // Start the runtime
    tracing::info!("Starting StreamRuntime...");
    runtime.start().await?;
    tracing::info!("StreamRuntime started successfully");

    // Wrap runtime in Arc<TokioMutex<>> for MCP server
    let runtime = Arc::new(TokioMutex::new(runtime));

    // Create MCP server in APPLICATION MODE (with runtime control and permissions)
    let server = McpServer::with_runtime(registry.clone(), runtime.clone())
        .with_permissions(permissions);

    tracing::info!(
        "MCP server {} v{} ready (APPLICATION MODE - runtime control enabled)",
        server.name(),
        server.version()
    );

    // Run the server with the selected transport
    // The runtime will keep running in the background while the MCP server handles requests
    if args.http {
        let bind_addr = format!("{}:{}", args.host, args.port);
        tracing::info!("Using HTTP transport on {}", bind_addr);
        server.run_http(&bind_addr).await?;
    } else {
        tracing::info!("Using stdio transport (for Claude Desktop integration)");
        // This will handle JSON-RPC messages from stdin and write responses to stdout
        server.run_stdio().await?;
    }

    // Cleanup: Stop the runtime when MCP server exits
    tracing::info!("Stopping StreamRuntime...");
    let mut rt = runtime.lock().await;
    rt.stop().await?;
    tracing::info!("StreamRuntime stopped");

    Ok(())
}
