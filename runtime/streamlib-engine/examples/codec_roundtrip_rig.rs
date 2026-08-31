// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine-owned codec round-trip rig: a source → encoder → decoder →
//! `DisplayWindow` graph, run against real hardware.
//!
//! Two source arms. `--source fixture` replays the checked-in PSNR
//! reference PNGs, each held for a run of frames long enough to cross a GOP
//! boundary, so a scorer can pair a decoded frame with the reference that
//! produced it. `--source camera` runs `CameraSource` unchanged, which is
//! the arm the real-hardware races are reproduced on — vivid hides that
//! class.
//!
//! Two codec arms. `--codec h264` and `--codec h265` swap the encoder and
//! decoder pair and change nothing else — the two built-in pairs share
//! their whole body, so the graph, the scoring and the shutdown path are
//! the same run twice. H.265 is the arm that carries a CTU pad: a
//! 1920x1080 source is coded at 1920x1088, so a decoder publishing 1088
//! would be visible here as a decoded extent that does not match its
//! reference.
//!
//! Rig-only to run: it needs Vulkan Video encode and decode queues, a
//! display server, and for the camera arm a `/dev/video*` device. CI
//! compiles it, which is what keeps it from rotting between rig runs.
//!
//! Scoring is not this binary's job: it hosts the control plane and stays up,
//! so `streamlib tap` and the surface-id `exchange` read the decoded
//! channel's exact pixels out of process — no window in the observation path,
//! and the graph unchanged by being watched.
//!
//! A scorer joins the two taps on the **frame-header timestamp**, not on
//! `sequence_index`. The ordering pair is an encoded-frame field and a
//! decoded frame is an ordinary video-frame bag, which carries no such pair;
//! what the decoder does propagate, unchanged, is the encoded frame's own
//! timestamp. So: tap the encoded channel for `sequence_index` → timestamp,
//! tap the decoded channel for timestamp → `surface_id`, and exchange that.
//!
//! ```text
//! cargo run -p streamlib-engine --example codec_roundtrip_rig
//! cargo run -p streamlib-engine --example codec_roundtrip_rig -- --codec h265
//! cargo run -p streamlib-engine --example codec_roundtrip_rig -- --source camera
//! cargo run -p streamlib-engine --example codec_roundtrip_rig -- --source camera --camera /dev/video1
//! cargo run -p streamlib-engine --example codec_roundtrip_rig -- --source camera --camera /dev/video1 --camera-max-width 3840 --camera-max-height 2160
//! streamlib exchange --channel <decoder_id>/video --out /tmp/decoded --count 4
//! ```

fn main() -> streamlib::sdk::error::Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux_rig::run()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(streamlib::sdk::error::Error::Runtime(
            "codec_roundtrip_rig needs Vulkan Video, which is Linux-only".into(),
        ))
    }
}

#[cfg(target_os = "linux")]
mod linux_rig {
    use serde::{Deserialize, Serialize};
    use streamlib::sdk::App;
    use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
    use streamlib::sdk::descriptors::ProcessorClassImportPath;
    use streamlib::sdk::error::{Error, Result};
    use streamlib::sdk::media_clock::MediaClock;
    use streamlib::sdk::processors::ContinuousProcessor;
    use streamlib::sdk::rhi::{PixelBuffer, PixelFormat, PublishedPixelBufferFrameId};
    use streamlib_media_builtins::video_frame::{
        ColorInfo, Primaries, Range, Transfer, VideoFrame,
    };
    use streamlib_media_builtins::{
        CameraSource, DisplayWindow, H264Decoder, H264Encoder, H265Decoder, H265Encoder,
        register_media_builtin_processor_types, stage_tightly_packed_rgba_into_pooled_pixel_buffer,
    };

    /// The fixture source's publish rate, and the twin of the
    /// `interval_ms = 100` on its `#[processor]` attribute below — that
    /// attribute is what actually schedules, this is what every frame
    /// declares, and an encoder mints its SPS VUI timing from the declaration.
    /// The attribute takes a literal, so the two are kept in step here and
    /// nowhere else.
    ///
    /// 10 fps rather than camera cadence because a faster source lets the
    /// display's `newest` input skip frames right at a reference boundary,
    /// which is where a scorer's pairing is most fragile.
    const FIXTURE_PUBLISH_FPS: u32 = 10;

    /// Seconds between the encoder's IDRs, stated by the rig rather than
    /// left to the encoder's own default, because the reference run length
    /// below is derived from it.
    const ENCODER_KEYFRAME_INTERVAL_SECONDS: u32 = 2;

    /// Frames each reference is held for: exactly one GOP
    /// (`keyframe_interval_seconds × fps`), so every reference run opens on a
    /// sync point and is independently decodable from a mid-stream join. A
    /// run shorter than a GOP would leave some references carrying none.
    const DEFAULT_FRAMES_PER_REFERENCE: u32 =
        ENCODER_KEYFRAME_INTERVAL_SECONDS * FIXTURE_PUBLISH_FPS;

    /// Which codec pair the round trip runs through. The pairs share their
    /// whole body, so this picks two registration names and nothing else.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum RoundTripCodecArm {
        H264,
        H265,
    }

    impl RoundTripCodecArm {
        /// The registered class paths of this arm's encoder and decoder.
        fn encoder_and_decoder_class_import_paths(
            self,
        ) -> (ProcessorClassImportPath, ProcessorClassImportPath) {
            match self {
                RoundTripCodecArm::H264 => (
                    H264Encoder::Processor::processor_class_import_path(),
                    H264Decoder::Processor::processor_class_import_path(),
                ),
                RoundTripCodecArm::H265 => (
                    H265Encoder::Processor::processor_class_import_path(),
                    H265Decoder::Processor::processor_class_import_path(),
                ),
            }
        }
    }

    /// Which source arm feeds the encoder.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum RoundTripSourceArm {
        /// Replay the checked-in reference PNGs — deterministic, and the
        /// only arm a PSNR score can be computed against.
        PsnrReferenceFixtures,
        /// Real capture hardware, which is where the `DEVICE_LOST` and
        /// shutdown races live.
        Camera,
    }

    /// Configuration for [`PsnrReferenceFixtureSource`].
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct PsnrReferenceFixtureSourceConfig {
        /// Directory of reference PNGs, replayed in sorted filename order.
        #[serde(default = "default_fixtures_directory")]
        fixtures_directory: String,
        /// Frames each reference is published for before the next one.
        #[serde(default = "default_frames_per_reference")]
        frames_per_reference: u32,
    }

    /// The checked-in reference set, resolved against this crate rather than
    /// the working directory a rig run happens to start in.
    fn default_fixtures_directory() -> String {
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/psnr").to_string()
    }

    fn default_frames_per_reference() -> u32 {
        DEFAULT_FRAMES_PER_REFERENCE
    }

    impl Default for PsnrReferenceFixtureSourceConfig {
        fn default() -> Self {
            Self {
                fixtures_directory: default_fixtures_directory(),
                frames_per_reference: default_frames_per_reference(),
            }
        }
    }

    /// One reference PNG staged into a pooled pixel buffer that stays
    /// acquired for the processor's life, so its surface id names the same
    /// picture every time the reference comes round again.
    struct StagedReferenceFixture {
        file_name: String,
        published_frame_id: PublishedPixelBufferFrameId,
        /// Held for the processor's lifetime: the [`PixelBuffer`] keeps the
        /// pool slot, and thus the surface id, alive.
        _pixel_buffer: PixelBuffer,
    }

    #[streamlib::sdk::processor(
        description = "Replays the checked-in PSNR reference PNGs as published video surfaces",
        execution = continuous(interval_ms = 100),
        config = crate::linux_rig::PsnrReferenceFixtureSourceConfig,
        output("video", description = "Reference frames to encode"),
    )]
    pub struct PsnrReferenceFixtureSource {
        staged_references: Vec<StagedReferenceFixture>,
        frame_extent: (u32, u32),
        frames_published: u64,
    }

    impl ContinuousProcessor for PsnrReferenceFixtureSource::Processor {
        fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
            if self.config.frames_per_reference == 0 {
                return Err(Error::Runtime(
                    "PsnrReferenceFixtureSource: frames_per_reference must be at least 1".into(),
                ));
            }
            let reference_paths = sorted_reference_png_paths(&self.config.fixtures_directory)?;

            for reference_path in reference_paths {
                let (rgba_pixels, width, height) = decode_png_as_rgba8(&reference_path)?;
                if !self.staged_references.is_empty() && self.frame_extent != (width, height) {
                    let (staged_width, staged_height) = self.frame_extent;
                    return Err(Error::Runtime(format!(
                        "PsnrReferenceFixtureSource: {} is {width}x{height} but the set opened \
                         at {staged_width}x{staged_height}; one session encodes one extent",
                        reference_path.display()
                    )));
                }
                self.frame_extent = (width, height);

                let (published_frame_id, pixel_buffer) = ctx
                    .gpu_full_access()
                    .acquire_pixel_buffer(width, height, PixelFormat::Rgba32)?;
                stage_tightly_packed_rgba_into_pooled_pixel_buffer(
                    &pixel_buffer,
                    &rgba_pixels,
                    width,
                    height,
                )?;
                self.staged_references.push(StagedReferenceFixture {
                    file_name: reference_path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    published_frame_id,
                    _pixel_buffer: pixel_buffer,
                });
            }

            if self.staged_references.is_empty() {
                return Err(Error::Runtime(format!(
                    "PsnrReferenceFixtureSource: no reference PNGs in {}",
                    self.config.fixtures_directory
                )));
            }
            tracing::info!(
                references = self.staged_references.len(),
                width = self.frame_extent.0,
                height = self.frame_extent.1,
                frames_per_reference = self.config.frames_per_reference,
                "PsnrReferenceFixtureSource: reference set staged"
            );
            Ok(())
        }

        fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
            tracing::info!(
                frames_published = self.frames_published,
                "PsnrReferenceFixtureSource: teardown"
            );
            self.staged_references.clear();
            Ok(())
        }

        fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
            let run_index = self.frames_published / u64::from(self.config.frames_per_reference);
            let reference_index = (run_index as usize) % self.staged_references.len();
            let reference = &self.staged_references[reference_index];
            let (width, height) = self.frame_extent;

            let frame = VideoFrame {
                surface_id: reference.published_frame_id.to_string(),
                width,
                height,
                timestamp_ns: MediaClock::now().as_nanos() as i64,
                fps: Some(FIXTURE_PUBLISH_FPS),
                // The references are full-range sRGB RGBA, which is what
                // the encoder's SPS VUI is then minted from.
                color_info: Some(ColorInfo {
                    primaries: Some(Primaries::Bt709),
                    transfer: Some(Transfer::Srgb),
                    matrix: None,
                    range: Some(Range::Full),
                }),
                content_light: None,
                mastering_display: None,
                texture_layout: None,
            };
            self.outputs.write("video", &frame)?;

            if self
                .frames_published
                .is_multiple_of(u64::from(self.config.frames_per_reference))
            {
                tracing::info!(
                    reference = reference.file_name,
                    surface_id = %reference.published_frame_id,
                    frames_published = self.frames_published,
                    "PsnrReferenceFixtureSource: reference on air"
                );
            }
            self.frames_published += 1;
            Ok(())
        }
    }

    /// The reference PNGs, in sorted filename order so a run's reference
    /// sequence is the same every time.
    fn sorted_reference_png_paths(fixtures_directory: &str) -> Result<Vec<std::path::PathBuf>> {
        let entries = std::fs::read_dir(fixtures_directory).map_err(|read_failure| {
            Error::Runtime(format!(
                "PsnrReferenceFixtureSource: cannot read {fixtures_directory}: {read_failure}"
            ))
        })?;
        let mut reference_paths: Vec<std::path::PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|extension| extension == "png"))
            .collect();
        reference_paths.sort();
        Ok(reference_paths)
    }

    /// Decode a reference PNG to tightly-packed RGBA8. The set is mixed —
    /// palette, 1-bit and 16-bit greyscale, truecolour — so every input
    /// colour type normalises to 8 bits and then widens to RGBA.
    fn decode_png_as_rgba8(reference_path: &std::path::Path) -> Result<(Vec<u8>, u32, u32)> {
        let file = std::fs::File::open(reference_path).map_err(|open_failure| {
            Error::Runtime(format!(
                "PsnrReferenceFixtureSource: cannot open {}: {open_failure}",
                reference_path.display()
            ))
        })?;
        let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder.read_info().map_err(|decode_failure| {
            Error::Runtime(format!(
                "PsnrReferenceFixtureSource: {} is not a readable PNG: {decode_failure}",
                reference_path.display()
            ))
        })?;

        let mut decoded = vec![0u8; reader.output_buffer_size()];
        let decoded_frame = reader.next_frame(&mut decoded).map_err(|decode_failure| {
            Error::Runtime(format!(
                "PsnrReferenceFixtureSource: {} did not decode: {decode_failure}",
                reference_path.display()
            ))
        })?;
        decoded.truncate(decoded_frame.buffer_size());
        let (width, height) = (decoded_frame.width, decoded_frame.height);
        let pixel_count = (width as usize) * (height as usize);

        let rgba_pixels = match decoded_frame.color_type {
            png::ColorType::Rgba => decoded,
            png::ColorType::Rgb => widen_to_rgba8(&decoded, pixel_count, |[red, green, blue]| {
                [red, green, blue, 0xFF]
            }),
            png::ColorType::Grayscale => {
                widen_to_rgba8(&decoded, pixel_count, |[luma]| [luma, luma, luma, 0xFF])
            }
            png::ColorType::GrayscaleAlpha => {
                widen_to_rgba8(&decoded, pixel_count, |[luma, alpha]| {
                    [luma, luma, luma, alpha]
                })
            }
            unexpected => {
                return Err(Error::Runtime(format!(
                    "PsnrReferenceFixtureSource: {} normalised to {unexpected:?}, which this \
                     reader does not widen to RGBA",
                    reference_path.display()
                )));
            }
        };
        Ok((rgba_pixels, width, height))
    }

    /// Widen `SOURCE_BYTES_PER_PIXEL`-wide samples to RGBA8, one pixel at a
    /// time. The source width is a const generic so a widening closure that
    /// reads more channels than the caller declared is a compile error rather
    /// than an out-of-bounds index on a decode nobody re-reads.
    fn widen_to_rgba8<const SOURCE_BYTES_PER_PIXEL: usize>(
        decoded: &[u8],
        pixel_count: usize,
        widen_one_pixel: impl Fn([u8; SOURCE_BYTES_PER_PIXEL]) -> [u8; 4],
    ) -> Vec<u8> {
        decoded
            .chunks_exact(SOURCE_BYTES_PER_PIXEL)
            .take(pixel_count)
            .flat_map(|source| {
                let source: [u8; SOURCE_BYTES_PER_PIXEL] =
                    source.try_into().expect("chunks_exact yields exact widths");
                widen_one_pixel(source)
            })
            .collect()
    }

    /// The port the hosted control plane binds, so `streamlib nodes` finds
    /// the run. The wheel's own default; the api-server increments on
    /// collision.
    const DEFAULT_CONTROL_PLANE_PORT: u16 = 9000;

    /// What the command line asked for.
    struct RoundTripRigArguments {
        source_arm: RoundTripSourceArm,
        codec_arm: RoundTripCodecArm,
        fixtures_directory: String,
        frames_per_reference: u32,
        control_plane_port: u16,
        /// V4L2 node the camera arm opens. Absent: the first capture-capable
        /// device the engine finds — which on a rig carrying both a virtual
        /// and a real camera is whichever enumerates first, so the arm that
        /// must run on real hardware names its node.
        camera_device_id: Option<String>,
        /// Resolution caps the camera arm negotiates against. Absent: the
        /// built-in's own 1920x1080 default. Naming a higher cap is how a
        /// run asks for more decode load than 1080p60 presents — the lever
        /// the decoder-lags-encoder scenario needs on hardware whose decode
        /// path has headroom at 60 fps.
        camera_max_width: Option<u32>,
        camera_max_height: Option<u32>,
    }

    fn parse_arguments() -> Result<RoundTripRigArguments> {
        let source_defaults = PsnrReferenceFixtureSourceConfig::default();
        let mut arguments = RoundTripRigArguments {
            source_arm: RoundTripSourceArm::PsnrReferenceFixtures,
            codec_arm: RoundTripCodecArm::H264,
            fixtures_directory: source_defaults.fixtures_directory,
            frames_per_reference: source_defaults.frames_per_reference,
            control_plane_port: DEFAULT_CONTROL_PLANE_PORT,
            camera_device_id: None,
            camera_max_width: None,
            camera_max_height: None,
        };

        let mut command_line = std::env::args().skip(1);
        while let Some(flag) = command_line.next() {
            let mut next_value_for_this_flag = || {
                command_line
                    .next()
                    .ok_or_else(|| Error::Runtime(format!("{flag} needs a value")))
            };
            match flag.as_str() {
                "--source" => {
                    arguments.source_arm = match next_value_for_this_flag()?.as_str() {
                        "fixture" => RoundTripSourceArm::PsnrReferenceFixtures,
                        "camera" => RoundTripSourceArm::Camera,
                        unknown => {
                            return Err(Error::Runtime(format!(
                                "--source {unknown} is neither `fixture` nor `camera`"
                            )));
                        }
                    }
                }
                "--codec" => {
                    arguments.codec_arm = match next_value_for_this_flag()?.as_str() {
                        "h264" => RoundTripCodecArm::H264,
                        "h265" => RoundTripCodecArm::H265,
                        unknown => {
                            return Err(Error::Runtime(format!(
                                "--codec {unknown} is neither `h264` nor `h265`"
                            )));
                        }
                    }
                }
                "--fixtures" => arguments.fixtures_directory = next_value_for_this_flag()?,
                "--camera" => arguments.camera_device_id = Some(next_value_for_this_flag()?),
                "--camera-max-width" => {
                    arguments.camera_max_width =
                        Some(next_value_for_this_flag()?.parse().map_err(|_| {
                            Error::Runtime("--camera-max-width takes a whole number".into())
                        })?)
                }
                "--camera-max-height" => {
                    arguments.camera_max_height =
                        Some(next_value_for_this_flag()?.parse().map_err(|_| {
                            Error::Runtime("--camera-max-height takes a whole number".into())
                        })?)
                }
                "--control-plane-port" => {
                    arguments.control_plane_port = next_value_for_this_flag()?
                        .parse()
                        .map_err(|_| Error::Runtime("--control-plane-port takes a port".into()))?
                }
                "--frames-per-reference" => {
                    arguments.frames_per_reference =
                        next_value_for_this_flag()?.parse().map_err(|_| {
                            Error::Runtime("--frames-per-reference takes a whole number".into())
                        })?
                }
                unknown => {
                    return Err(Error::Runtime(format!(
                        "unknown flag {unknown}; the rig takes --source, --codec, --camera, \
                         --camera-max-width, --camera-max-height, --fixtures, \
                         --frames-per-reference and --control-plane-port"
                    )));
                }
            }
        }
        Ok(arguments)
    }

    pub fn run() -> Result<()> {
        let arguments = parse_arguments()?;
        register_media_builtin_processor_types();

        let app = App::new()?;
        let source = match arguments.source_arm {
            RoundTripSourceArm::PsnrReferenceFixtures => app
                .add_local::<PsnrReferenceFixtureSource::Processor>(
                PsnrReferenceFixtureSourceConfig {
                    fixtures_directory: arguments.fixtures_directory.clone(),
                    frames_per_reference: arguments.frames_per_reference,
                },
                Some("fixture_source"),
            )?,
            RoundTripSourceArm::Camera => app.add(
                CameraSource::Processor::processor_class_import_path(),
                serde_json::json!({
                    "device_id": arguments.camera_device_id,
                    "max_width": arguments.camera_max_width,
                    "max_height": arguments.camera_max_height,
                }),
                Some("camera"),
            )?,
        };
        let (encoder_class_import_path, decoder_class_import_path) =
            arguments.codec_arm.encoder_and_decoder_class_import_paths();
        let encoder = app.add(
            encoder_class_import_path,
            serde_json::json!({ "keyframe_interval_seconds": ENCODER_KEYFRAME_INTERVAL_SECONDS }),
            Some("encoder"),
        )?;
        let decoder = app.add(
            decoder_class_import_path,
            serde_json::json!({}),
            Some("decoder"),
        )?;
        let display = app.add(
            DisplayWindow::Processor::processor_class_import_path(),
            serde_json::json!({ "title": "streamlib codec round-trip rig" }),
            Some("display"),
        )?;

        // Hosted, not optional: an unobservable rig can only be watched, and
        // the codec proof is scored by tapping the decoded channel and
        // exchanging its surface ids for exact pixels.
        streamlib_api_server::control_plane_host::register_api_server_control_plane_processor_on_runtime(
            app.runner(),
            streamlib_api_server::control_plane_host::ApiServerControlPlaneHostConfig {
                bind_host: "127.0.0.1".to_string(),
                bind_port: arguments.control_plane_port,
                node_name: Some("codec-roundtrip-rig".to_string()),
            },
        )?;

        app.connect((&source, "video"), (&encoder, "video"))?;
        app.connect((&encoder, "encoded_video"), (&decoder, "encoded_video"))?;
        app.connect((&decoder, "video"), (&display, "video"))?;

        tracing::info!(
            source_arm = ?arguments.source_arm,
            codec_arm = ?arguments.codec_arm,
            "codec_roundtrip_rig: {} -> encoder -> decoder -> display",
            source.display_name()
        );
        app.run()
    }
}
