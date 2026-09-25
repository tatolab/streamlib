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
//! `--destination` is that same `OpusEncoder` left unwired, and `--wiring-agent`
//! is a runtime holding no processor at all. Together they are the other half
//! of the bar: a link neither of them asked for, wired by a third runtime over
//! MCP, which is the only way a third-party wiring is made.
//!
//! `--video-source` runs a `TestPatternSource` and offers it; `--video-reader`
//! links from `<source runtime name>/TestPatternSource/video` into an
//! `H264Encoder` on Linux and a `DisplayWindow` on macOS. That arm is the
//! frame-carrying one: a video bag names a surface, and a surface id means
//! nothing on another machine, so the sending runtime copies the frame's
//! pixels out and the reading one mints a local surface for them.
//! `tests/fixtures/verify_cross_runtime_frame.sh` reads the two ids and
//! exchanges each on its own node.
//!
//! The audio arms need no hardware — under the null backend a microphone
//! publishes silent blocks and the graph runs unchanged. The video arms need
//! a GPU on both ends, which is why they are rig-only like everything else
//! here.
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
    use streamlib::sdk::app::{AddedProcessor, App};
    use streamlib::sdk::error::{Error, Result};
    use streamlib::sdk::graph::{InputLinkPortRef, MeshPortAddress, OutputLinkPortRef};
    use streamlib_media_builtins::{
        MicrophoneSource, OpusEncoder, TestPatternSource, register_media_builtin_processor_types,
    };

    /// The display name the source gives its microphone, and the reader
    /// addresses it by. Stated rather than defaulted so the two agree without
    /// either reading the other's code.
    const THE_SOURCES_DISPLAY_NAME: &str = "MicrophoneSource";

    /// The port the source publishes and the reader links from.
    const THE_PORT: &str = "audio";

    /// The display name the video source gives its pattern, and the video
    /// reader addresses it by.
    const THE_VIDEO_SOURCES_DISPLAY_NAME: &str = "TestPatternSource";

    /// The port the video source publishes and the video reader links from.
    const THE_VIDEO_PORT: &str = "video";

    /// The pattern's extent. Small on purpose: a 1080p RGBA frame is 8.3 MB,
    /// which has to queue inside Zenoh's fragment deadline, and what this arm
    /// is here to prove is that the pixels arrive rather than how many fit
    /// down a wire.
    const THE_PATTERNS_WIDTH: u32 = 320;
    const THE_PATTERNS_HEIGHT: u32 = 240;

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
        /// Hold an input for another runtime to push into, and wire nothing.
        ///
        /// The same encoder `--reader` wires for itself, left unwired: whoever
        /// asks for the link is the point of the arm this end serves.
        TheDestination,
        /// Publish one video port and offer it on the mesh.
        ///
        /// The frame-carrying arm: every bag this publishes names a surface,
        /// which is the one key the engine reads on the way across.
        TheVideoSource,
        /// Link from the video source's port and encode what arrives.
        TheVideoReader {
            /// The runtime name the video source is addressed by.
            source_runtime_name: String,
        },
        /// Wire two other runtimes together, being neither end of the link.
        ///
        /// Adds no processor at all — it drives its own control plane's MCP
        /// `connect` with both ends named on the mesh, which is the only way a
        /// third-party wiring is asked for.
        TheWiringAgent,
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
            WhichEndOfTheLink::TheDestination => {
                app.add(
                    OpusEncoder::Processor::processor_class_import_path(),
                    serde_json::json!({}),
                    Some("OpusEncoder"),
                )?;
                tracing::info!(
                    "cross_runtime_link_rig: holding OpusEncoder/{THE_PORT} for another runtime \
                     to push into, as {}",
                    app.runner().runtime_name()
                );
            }
            WhichEndOfTheLink::TheVideoSource => {
                app.add(
                    TestPatternSource::Processor::processor_class_import_path(),
                    serde_json::json!({
                        "width": THE_PATTERNS_WIDTH,
                        "height": THE_PATTERNS_HEIGHT,
                    }),
                    Some(THE_VIDEO_SOURCES_DISPLAY_NAME),
                )?;
                tracing::info!(
                    "cross_runtime_link_rig: offering {}/{THE_VIDEO_PORT} as {}",
                    THE_VIDEO_SOURCES_DISPLAY_NAME,
                    app.runner().runtime_name()
                );
            }
            WhichEndOfTheLink::TheVideoReader {
                source_runtime_name,
            } => {
                let encoder = the_video_readers_consumer(&app)?;
                let source = MeshPortAddress::new(
                    source_runtime_name,
                    THE_VIDEO_SOURCES_DISPLAY_NAME,
                    THE_VIDEO_PORT,
                )?;
                app.runner().connect(
                    OutputLinkPortRef::on_another_runtime(source.clone()),
                    InputLinkPortRef::new(encoder.processor_id(), THE_VIDEO_PORT),
                )?;
                tracing::info!("cross_runtime_link_rig: reading {source} into the encoder");
            }
            WhichEndOfTheLink::TheWiringAgent => {
                tracing::info!(
                    "cross_runtime_link_rig: wiring two other runtimes, as {}",
                    app.runner().runtime_name()
                );
            }
        }

        app.run()
    }

    /// What the video reader links the crossed frames into.
    ///
    /// An `H264Encoder`, because a consumer that merely receives the bag
    /// would not prove the surface it names is usable — the encoder resolves
    /// it and encodes it, which is the whole claim.
    #[cfg(target_os = "linux")]
    fn the_video_readers_consumer(app: &App) -> Result<AddedProcessor> {
        app.add(
            streamlib_media_builtins::H264Encoder::Processor::processor_class_import_path(),
            serde_json::json!({}),
            Some("H264Encoder"),
        )
    }

    /// A `DisplayWindow`, because macOS builds no hardware encoder yet — the
    /// window resolves the crossed surface and draws it, which is the same
    /// claim.
    #[cfg(target_os = "macos")]
    fn the_video_readers_consumer(app: &App) -> Result<AddedProcessor> {
        app.add(
            streamlib_media_builtins::DisplayWindow::Processor::processor_class_import_path(),
            serde_json::json!({ "title": "cross_runtime_link_rig video reader" }),
            Some("DisplayWindow"),
        )
    }

    /// No consumer on this platform resolves a crossed surface. Refused by
    /// name rather than left to fail as a missing processor class at `add`.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn the_video_readers_consumer(_app: &App) -> Result<AddedProcessor> {
        Err(Error::Runtime(
            "--video-reader needs a consumer that resolves the crossed surface, which this \
             platform does not build"
                .into(),
        ))
    }

    /// The two flags the fixture drives.
    fn read_the_command_line() -> Result<(WhichEndOfTheLink, u16)> {
        let mut which_end = None;
        let mut control_plane_port = DEFAULT_CONTROL_PLANE_PORT;
        let mut arguments = std::env::args().skip(1);
        while let Some(flag) = arguments.next() {
            match flag.as_str() {
                "--source" => which_end = Some(WhichEndOfTheLink::TheSource),
                "--video-source" => which_end = Some(WhichEndOfTheLink::TheVideoSource),
                "--video-reader" => {
                    which_end = Some(WhichEndOfTheLink::TheVideoReader {
                        source_runtime_name: arguments.next().ok_or_else(|| {
                            Error::Runtime(
                                "--video-reader takes the runtime name it links from".into(),
                            )
                        })?,
                    })
                }
                "--destination" => which_end = Some(WhichEndOfTheLink::TheDestination),
                "--wiring-agent" => which_end = Some(WhichEndOfTheLink::TheWiringAgent),
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
                        "unknown flag {unknown:?}; this rig takes --source, --reader <source \
                         runtime name>, --video-source, --video-reader <source runtime name>, \
                         --destination or --wiring-agent, and --control-plane-port"
                    )));
                }
            }
        }
        let which_end = which_end.ok_or_else(|| {
            Error::Runtime(
                "name an end: --source, --reader <source runtime name>, --video-source, \
                 --video-reader <source runtime name>, --destination, or --wiring-agent"
                    .into(),
            )
        })?;
        Ok((which_end, control_plane_port))
    }
}
