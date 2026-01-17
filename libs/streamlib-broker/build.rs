// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Generate gRPC server and client code from broker.proto
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .out_dir("src/proto")
        .compile_protos(&["proto/broker.proto"], &["proto/"])?;

    println!("cargo:rerun-if-changed=proto/broker.proto");
    Ok(())
}
