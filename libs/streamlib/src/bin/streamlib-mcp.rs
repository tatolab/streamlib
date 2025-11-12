
use streamlib::{global_registry, mcp::McpServer};
use streamlib::core::StreamRuntime;
use streamlib::{request_camera_permission, request_display_permission, request_audio_permission};
use clap::Parser;
use std::sync::Arc;
use parking_lot::Mutex;

#[derive(Parser, Debug)]
#[command(name = "streamlib-mcp")]
#[command(about = "MCP server for streamlib processors")]
#[command(version)]
struct Args {
    #[arg(long)]
    http: bool,

    #[arg(long, default_value = "3050")]
    port: u16,

    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    #[arg(long)]
    allow_all: bool,

    #[arg(long)]
    allow_camera: bool,

    #[arg(long)]
    allow_display: bool,

    #[arg(long)]
    allow_audio: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!("Starting streamlib MCP server");


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

    // IMPORTANT: Run tokio on a background thread and keep main thread for CFRunLoop
    let (result_tx, result_rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(async_main(args, permissions));
        result_tx.send(result).ok();
    });

    tracing::info!("Main thread now running CFRunLoop for GCD dispatches");

    #[cfg(target_os = "macos")]
    {
        use core_foundation::runloop::{CFRunLoop, kCFRunLoopDefaultMode};
        use std::time::Duration;

        loop {
            if let Ok(result) = result_rx.try_recv() {
                return result;
            }

            unsafe {
                CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, Duration::from_millis(100), true);
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        result_rx.recv().unwrap()
    }
}

async fn async_main(args: Args, permissions: std::collections::HashSet<String>) -> anyhow::Result<()> {
    tracing::info!("Entered async runtime");

    let registry = global_registry();

    tracing::info!("Auto-registered {} processors from inventory", {
        let reg = registry.lock();
        reg.list().len()
    });

    tracing::info!("Creating StreamRuntime");
    let mut runtime = StreamRuntime::new();

    // Configure macOS event loop before starting
    #[cfg(target_os = "macos")]
    streamlib::configure_macos_event_loop(&mut runtime);

    // Spawn runtime in dedicated thread (runtime.run() blocks)
    let runtime = Arc::new(Mutex::new(runtime));
    let runtime_clone = Arc::clone(&runtime);

    let runtime_handle = std::thread::spawn(move || {
        tracing::info!("Starting StreamRuntime on dedicated thread...");
        let mut rt = runtime_clone.lock();
        if let Err(e) = rt.run() {
            tracing::error!("StreamRuntime error: {}", e);
        }
        tracing::info!("StreamRuntime stopped");
    });

    // Give runtime a moment to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    tracing::info!("StreamRuntime started successfully on background thread");

    let server = McpServer::with_runtime(registry.clone(), runtime.clone())
        .with_permissions(permissions);

    tracing::info!(
        "MCP server {} v{} ready (APPLICATION MODE - runtime control enabled)",
        server.name(),
        server.version()
    );

    if args.http {
        let bind_addr = format!("{}:{}", args.host, args.port);
        tracing::info!("Using HTTP transport on {}", bind_addr);
        server.run_http(&bind_addr).await?;
    } else {
        tracing::info!("Using stdio transport (for Claude Desktop integration)");
        server.run_stdio().await?;
    }

    tracing::info!("Shutting down StreamRuntime...");
    // Send shutdown signal to runtime thread
    {
        let mut rt = runtime.lock();
        rt.stop()?;
    }

    // Wait for runtime thread to finish
    runtime_handle.join().map_err(|_| anyhow::anyhow!("Failed to join runtime thread"))?;
    tracing::info!("StreamRuntime thread joined");

    Ok(())
}
