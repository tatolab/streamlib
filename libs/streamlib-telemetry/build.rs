// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "broker")]
    {
        let out_dir = "src/proto";
        std::fs::create_dir_all(out_dir)?;

        tonic_build::configure()
            .build_server(true)
            .build_client(true)
            .out_dir(out_dir)
            .compile_protos(&["proto/telemetry_ingest.proto"], &["proto/"])?;

        // Add copyright header to generated file
        let proto_path = std::path::Path::new("src/proto/streamlib.telemetry.rs");
        if proto_path.exists() {
            let content = std::fs::read_to_string(proto_path)?;
            if !content.starts_with("// Copyright") {
                let with_header = format!(
                    "// Copyright (c) 2025 Jonathan Fontanez\n\
                     // SPDX-License-Identifier: BUSL-1.1\n\n{}",
                    content
                );
                std::fs::write(proto_path, with_header)?;
            }
        }
    }

    println!("cargo:rerun-if-changed=proto/telemetry_ingest.proto");
    Ok(())
}
