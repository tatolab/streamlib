// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::fs;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Generate gRPC server and client code from broker.proto
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .out_dir("src/proto")
        .compile_protos(&["proto/broker.proto"], &["proto/"])?;

    // Add copyright header to generated file (prost-build doesn't include it)
    let proto_path = Path::new("src/proto/streamlib.broker.rs");
    if proto_path.exists() {
        let content = fs::read_to_string(proto_path)?;
        if !content.starts_with("// Copyright") {
            let with_header = format!(
                "// Copyright (c) 2025 Jonathan Fontanez\n\
                 // SPDX-License-Identifier: BUSL-1.1\n\n{}",
                content
            );
            fs::write(proto_path, with_header)?;
        }
    }

    println!("cargo:rerun-if-changed=proto/broker.proto");
    Ok(())
}
