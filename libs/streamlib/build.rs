// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

fn main() {
    // Link Metal framework on macOS for MP4 writer
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rustc-link-lib=framework=Metal");

        // Compile broker gRPC proto if it exists
        let proto_path = "src/apple/subprocess_rhi/proto/broker.proto";
        if std::path::Path::new(proto_path).exists() {
            tonic_build::configure()
                .build_server(true)
                .build_client(true)
                .out_dir("src/apple/subprocess_rhi/proto")
                .compile_protos(&[proto_path], &["src/apple/subprocess_rhi/proto"])
                .expect("Failed to compile broker.proto");
        }
    }
}
