// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One link between two started runtimes, on real hardware.
//!
//! The CI proof of a cross-runtime link stands each runtime's mesh half up
//! without a `Runner`, because `Runner::start()` needs a GPU. This is the other
//! half of the bar: two whole runtimes, both started, one pulling the other's
//! output port through the ordinary `connect`.
//!
//! `--source` runs a `MicrophoneSource` and offers it. `--reader` links from
//! `<source runtime name>/MicrophoneSource/audio` into an `OpusEncoder`, whose
//! input declares a window contract — so the arm also covers the case where the
//! destination's own contract sizes the channel the ingress must publish onto.
//!
//! Audio rather than video on purpose: a video bag names a surface, and a
//! surface id means nothing on another machine, so the mesh does not carry one
//! until the frame's pixels do (#2290). An `AudioBlock`'s samples ride inline.
//! No audio hardware is needed either — under the null backend a microphone
//! publishes silent blocks and the graph runs unchanged.
//!
//! Both runtimes host a control plane, because an unobservable rig can only be
//! watched: `tests/fixtures/verify_cross_runtime_link.sh` reads `graph` on the
//! reader for the link's state and taps the port by its mesh address.
//!
//! Each runtime takes its name and mesh from the environment the way any
//! runtime does (`STREAMLIB_RUNTIME_NAME`, `STREAMLIB_MESH_NAME`), so the
//! fixture drives exactly the doors an author has.

fn main() -> streamlib::sdk::error::Result<()> {
    rig::run()
}

mod rig {
    use streamlib::sdk::app::App;
    use streamlib::sdk::error::{Error, Result};
    use streamlib::sdk::graph::{InputLinkPortRef, MeshPortAddress, OutputLinkPortRef};
    use streamlib_media_builtins::{
        MicrophoneSource, OpusEncoder, register_media_builtin_processor_types,
    };

    /// The display name the source gives its microphone, and the reader
    /// addresses it by. Stated rather than defaulted so the two agree without
    /// either reading the other's code.
    const THE_SOURCES_DISPLAY_NAME: &str = "MicrophoneSource";

    /// The port the source publishes and the reader links from.
    const THE_PORT: &str = "audio";

    /// The port the hosted control plane binds. The wheel's own default; the
    /// api-server increments on collision, so the fixture names one per end.
    const DEFAULT_CONTROL_PLANE_PORT: u16 = 9000;

    /// Which end of the link this process is.
    enum WhichEndOfTheLink {
        /// Publish one port and offer it on the mesh.
        TheSource,
        /// Link from the source's port and encode what arrives.
        TheReader {
            /// The runtime name the source is addressed by.
            source_runtime_name: String,
        },
    }

    pub fn run() -> Result<()> {
        let (which_end, control_plane_port) = read_the_command_line()?;
        register_media_builtin_processor_types();
        let app = App::new()?;

        // Hosted, not optional: the fixture reads the link's state and taps the
        // port by its address, and a rig nobody can observe proves nothing.
        streamlib_api_server::control_plane_host::register_api_server_control_plane_processor_on_runtime(
            app.runner(),
            streamlib_api_server::control_plane_host::ApiServerControlPlaneHostConfig {
                bind_host: "127.0.0.1".to_string(),
                bind_port: control_plane_port,
            },
        )?;

        match which_end {
            WhichEndOfTheLink::TheSource => {
                app.add(
                    MicrophoneSource::Processor::processor_class_import_path(),
                    serde_json::json!({}),
                    Some(THE_SOURCES_DISPLAY_NAME),
                )?;
                tracing::info!(
                    "cross_runtime_link_rig: offering {}/{THE_PORT} as {}",
                    THE_SOURCES_DISPLAY_NAME,
                    app.runner().runtime_name()
                );
            }
            WhichEndOfTheLink::TheReader {
                source_runtime_name,
            } => {
                let encoder = app.add(
                    OpusEncoder::Processor::processor_class_import_path(),
                    serde_json::json!({}),
                    Some("OpusEncoder"),
                )?;
                let source =
                    MeshPortAddress::new(source_runtime_name, THE_SOURCES_DISPLAY_NAME, THE_PORT)?;
                // The whole point of the arm: an ordinary `connect`, with a
                // source that names a port on another runtime. It returns at
                // once — `graph` carries the outcome.
                app.runner().connect(
                    OutputLinkPortRef::on_another_runtime(source.clone()),
                    InputLinkPortRef::new(encoder.processor_id(), THE_PORT),
                )?;
                tracing::info!("cross_runtime_link_rig: reading {source} into OpusEncoder");
            }
        }

        app.run()
    }

    /// The two flags the fixture drives.
    fn read_the_command_line() -> Result<(WhichEndOfTheLink, u16)> {
        let mut which_end = None;
        let mut control_plane_port = DEFAULT_CONTROL_PLANE_PORT;
        let mut arguments = std::env::args().skip(1);
        while let Some(flag) = arguments.next() {
            match flag.as_str() {
                "--source" => which_end = Some(WhichEndOfTheLink::TheSource),
                "--reader" => {
                    which_end = Some(WhichEndOfTheLink::TheReader {
                        source_runtime_name: arguments.next().ok_or_else(|| {
                            Error::Runtime("--reader takes the runtime name it links from".into())
                        })?,
                    })
                }
                "--control-plane-port" => {
                    control_plane_port = arguments
                        .next()
                        .and_then(|port| port.parse().ok())
                        .ok_or_else(|| Error::Runtime("--control-plane-port takes a port".into()))?
                }
                unknown => {
                    return Err(Error::Runtime(format!(
                        "unknown flag {unknown:?}; this rig takes --source or --reader \
                         <source runtime name>, and --control-plane-port"
                    )));
                }
            }
        }
        let which_end = which_end.ok_or_else(|| {
            Error::Runtime("name an end: --source, or --reader <source runtime name>".into())
        })?;
        Ok((which_end, control_plane_port))
    }
}
