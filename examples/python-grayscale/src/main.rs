// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Example: Load a Python-defined grayscale processor.
//!
//! This example demonstrates loading a Python processor script
//! and extracting its metadata. The processor uses a GPU shader
//! for grayscale conversion.

use std::path::PathBuf;
use streamlib_python::PythonHostProcessorConfig;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

fn main() -> streamlib::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    tracing::info!("Python Grayscale Processor Example");
    tracing::info!("===================================");

    // Path to the Python project directory
    let project_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    tracing::info!("Loading Python processor from: {}", project_path.display());

    // Create configuration for the Python host processor
    let config = PythonHostProcessorConfig {
        project_path,
        class_name: "GrayscaleProcessor".to_string(),
        entry_point: Some("grayscale_processor.py".to_string()),
    };

    tracing::info!("Configuration: {:?}", config);

    // Note: To actually run the processor, you would need to:
    // 1. Create a StreamRuntime
    // 2. Add the PythonHostProcessor to the graph
    // 3. Connect it to video sources/sinks
    // 4. Start the runtime
    //
    // See examples/camera-python-display for a complete pipeline example.

    tracing::info!("Python processor configuration created successfully!");
    tracing::info!("To run the full pipeline, see examples/camera-python-display");

    Ok(())
}
