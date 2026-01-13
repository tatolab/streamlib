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
            println!("cargo:rerun-if-changed={}", proto_path);
            println!("cargo:rerun-if-changed=src/apple/subprocess_rhi/proto");

            match tonic_build::configure()
                .build_server(true)
                .build_client(true)
                .out_dir("src/apple/subprocess_rhi/proto")
                .compile_protos(&[proto_path], &["src/apple/subprocess_rhi/proto"])
            {
                Ok(_) => {}
                Err(e) => {
                    // Don't fail build if protoc isn't installed - just warn
                    // Generated code can be committed to repo for CI without protoc
                    println!(
                        "cargo:warning=Failed to compile broker.proto: {}. \
                         Install protoc with: brew install protobuf",
                        e
                    );
                }
            }
        }
    }
}
