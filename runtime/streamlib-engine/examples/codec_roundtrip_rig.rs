// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine-owned codec round-trip rig: a source → encoder → decoder →
//! `DisplayWindow` graph, run against real hardware.
//!
//! Three source arms. `--source fixture` replays the checked-in PSNR
//! reference PNGs, each held for a run of frames long enough to cross a GOP
//! boundary, so a scorer can pair a decoded frame with the reference that
//! produced it. `--source camera` runs `CameraSource` unchanged, which is
//! the arm the real-hardware races are reproduced on — vivid hides that
//! class. `--source mp4:<path>` demuxes an `Mp4Sink` recording's video track
//! back into access units and publishes them into the decoder directly, with
//! no encoder in the graph: the decode-back that proves the container carried
//! the encoder's bytes untouched, scored against the same baseline the live
//! camera path locks to with one file in between.
//!
//! Two codec arms. `--codec h264` and `--codec h265` swap the encoder and
//! decoder pair and change nothing else — the two built-in pairs share
//! their whole body, so the graph, the scoring and the shutdown path are
//! the same run twice.
//!
//! Neither arm gates the conformance crop: the scorer crops each decode to
//! its reference extent first, and the window's origin is (0, 0), so a
//! decoder publishing the padded extent scores identically. That contract
//! belongs to `h265_decoder_completes_the_round_trip`.
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
//! cargo run -p streamlib-engine --example codec_roundtrip_rig -- --source mp4:/tmp/recording.mp4 --codec h264
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
    use mp4_atom::{Atom, Codec, Header, Moof, Moov, ReadAtom, ReadFrom};
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
    use streamlib_media_builtins::mp4_annex_b_access_unit::NAL_UNIT_LENGTH_PREFIX_BYTES;
    use streamlib_media_builtins::{
        CameraSource, DisplayWindow, EncodedVideoCodec, EncodedVideoFrame, H264Decoder,
        H264Encoder, H265Decoder, H265Encoder, register_media_builtin_processor_types,
        stage_tightly_packed_rgba_into_pooled_pixel_buffer,
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
        /// The elementary stream this arm's pair codes, which is also what a
        /// recording names its own track by.
        fn encoded_video_codec(self) -> EncodedVideoCodec {
            match self {
                RoundTripCodecArm::H264 => EncodedVideoCodec::H264,
                RoundTripCodecArm::H265 => EncodedVideoCodec::H265,
            }
        }

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

    /// Which source arm feeds the graph.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum RoundTripSourceArm {
        /// Replay the checked-in reference PNGs — deterministic, and the
        /// only arm a PSNR score can be computed against.
        PsnrReferenceFixtures,
        /// Real capture hardware, which is where the `DEVICE_LOST` and
        /// shutdown races live.
        Camera,
        /// Replay an `Mp4Sink` recording's video track. The only arm that
        /// publishes the encoded domain itself, so the only one with no
        /// encoder in the graph.
        RecordedMp4File { recording_path: String },
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

    /// What `--source` prefixes a recording's path with.
    const RECORDED_MP4_SOURCE_PREFIX: &str = "mp4:";

    /// The Annex-B start code every NAL unit is re-prefixed with on the way
    /// out of a sample. Four bytes rather than three because that is what the
    /// encoder emitted and what the decoder was proven against.
    const ANNEX_B_START_CODE: [u8; 4] = [0, 0, 0, 1];

    /// `sample_is_non_sync_sample`, ISO/IEC 14496-12 §8.8.3.1. A sample entry
    /// without it set is a sync sample.
    const SAMPLE_FLAG_IS_NON_SYNC_SAMPLE: u32 = 0x0001_0000;

    /// One access unit read back out of a recording, already in the Annex-B
    /// shape an encoded-video link carries.
    struct ReplayableAccessUnit {
        annex_b_access_unit_bytes: Vec<u8>,
        is_sync_point: bool,
    }

    /// A recording's video track, demuxed into the bags a decoder reads.
    struct RecordedVideoTrackReplay {
        codec: EncodedVideoCodec,
        /// The coded extent the sample entry states — before the conformance
        /// crop, as an encoded frame's own `width`/`height` are.
        coded_width: u32,
        coded_height: u32,
        access_units: Vec<ReplayableAccessUnit>,
    }

    /// What a track's sample entry says about the elementary stream inside it.
    struct RecordedVideoSampleEntry {
        codec: EncodedVideoCodec,
        coded_width: u32,
        coded_height: u32,
        /// The parameter sets `avcC`/`hvcC` carries, in the order a decoder
        /// wants them re-prepended in — 14496-15 keeps them out of the samples,
        /// so a sync sample is only decodable with these back in front of it.
        parameter_set_nal_units: Vec<Vec<u8>>,
        /// How many bytes prefix each NAL unit inside a sample.
        nal_unit_length_prefix_bytes: u8,
    }

    /// Configuration for [`RecordedMp4TrackReplaySource`].
    ///
    /// `Default` is the empty pair every processor config owes; the rig always
    /// states both, and an unset path is refused by name at `setup()`.
    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    pub struct RecordedMp4TrackReplaySourceConfig {
        /// The recording to replay.
        recording_path: String,
        /// The wire codec the rig's decoder was built for. The file names its
        /// own, so a mismatch is refused at `setup()` naming both rather than
        /// reaching a decoder that would refuse every bag.
        expected_codec: String,
    }

    /// Every top-level `moov` and `moof` in a recording, each fragment paired
    /// with where its box starts.
    ///
    /// The offset is what makes the samples findable: `trun.data_offset` is
    /// relative to the `moof` it rides in (`tfhd.default_base_is_moof`), so a
    /// fragment read out of its position in the file locates nothing.
    fn read_moov_and_fragments(
        file_bytes: &[u8],
        recording_path: &str,
    ) -> Result<(Moov, Vec<(usize, Moof)>)> {
        let mut reader = std::io::Cursor::new(file_bytes);
        let mut moov: Option<Moov> = None;
        let mut fragments: Vec<(usize, Moof)> = Vec::new();

        loop {
            let box_start = reader.position() as usize;
            // A header that cannot be read is the end of the file, or a
            // trailing box a killed run never finished. Both stop the walk
            // rather than fail it — that is what the fragmented layout is for.
            let Ok(header) = Header::read_from(&mut reader) else {
                break;
            };
            let body_start = reader.position();
            match header.kind {
                kind if kind == Moov::KIND => {
                    moov = Some(Moov::read_atom(&header, &mut reader).map_err(|failure| {
                        Error::Runtime(format!(
                            "RecordedMp4TrackReplaySource: {recording_path}'s `moov` did not \
                             parse: {failure}"
                        ))
                    })?);
                }
                kind if kind == Moof::KIND => {
                    let fragment = Moof::read_atom(&header, &mut reader).map_err(|failure| {
                        Error::Runtime(format!(
                            "RecordedMp4TrackReplaySource: {recording_path} carries a `moof` \
                             that did not parse: {failure}"
                        ))
                    })?;
                    fragments.push((box_start, fragment));
                }
                _ => {}
            }
            // A box with no declared size runs to the end of the file.
            let Some(body_bytes) = header.size else {
                break;
            };
            let next_box_start = body_start + body_bytes as u64;
            if next_box_start > file_bytes.len() as u64 {
                break;
            }
            reader.set_position(next_box_start);
        }

        let moov = moov.ok_or_else(|| {
            Error::Runtime(format!(
                "RecordedMp4TrackReplaySource: {recording_path} carries no `moov`, so it \
                 describes no track — a recording whose header never landed"
            ))
        })?;
        Ok((moov, fragments))
    }

    /// Read the first video track's sample entry, or say why the recording
    /// holds nothing this rig can replay.
    fn read_video_sample_entry(
        moov: &Moov,
        recording_path: &str,
    ) -> Result<(u32, RecordedVideoSampleEntry)> {
        for trak in &moov.trak {
            let sample_entry = match trak.mdia.minf.stbl.stsd.codecs.first() {
                Some(Codec::Avc1(avc1)) => RecordedVideoSampleEntry {
                    codec: EncodedVideoCodec::H264,
                    coded_width: u32::from(avc1.visual.width),
                    coded_height: u32::from(avc1.visual.height),
                    parameter_set_nal_units: avc1
                        .avcc
                        .sequence_parameter_sets
                        .iter()
                        .chain(avc1.avcc.picture_parameter_sets.iter())
                        .cloned()
                        .collect(),
                    nal_unit_length_prefix_bytes: avc1.avcc.length_size,
                },
                Some(Codec::Hvc1(hvc1)) => RecordedVideoSampleEntry {
                    codec: EncodedVideoCodec::H265,
                    coded_width: u32::from(hvc1.visual.width),
                    coded_height: u32::from(hvc1.visual.height),
                    // The arrays are written VPS, SPS, PPS and read back in
                    // that order, which is the order they go back in front of
                    // a sync sample.
                    parameter_set_nal_units: hvc1
                        .hvcc
                        .arrays
                        .iter()
                        .flat_map(|array| array.nalus.iter())
                        .cloned()
                        .collect(),
                    nal_unit_length_prefix_bytes: hvc1.hvcc.length_size_minus_one + 1,
                },
                _ => continue,
            };
            if sample_entry.parameter_set_nal_units.is_empty() {
                return Err(Error::Runtime(format!(
                    "RecordedMp4TrackReplaySource: {recording_path}'s video track carries no \
                     parameter sets in its sample entry, so no sync sample in it is decodable"
                )));
            }
            if sample_entry.nal_unit_length_prefix_bytes != NAL_UNIT_LENGTH_PREFIX_BYTES {
                return Err(Error::Runtime(format!(
                    "RecordedMp4TrackReplaySource: {recording_path}'s video track prefixes each \
                     NAL unit with {} bytes; this replay reads the {NAL_UNIT_LENGTH_PREFIX_BYTES} \
                     the sink writes",
                    sample_entry.nal_unit_length_prefix_bytes
                )));
            }
            return Ok((trak.tkhd.track_id, sample_entry));
        }
        Err(Error::Runtime(format!(
            "RecordedMp4TrackReplaySource: {recording_path} holds no `avc1` or `hvc1` track, so \
             there is no video to replay"
        )))
    }

    /// One length-prefixed sample back into the Annex-B access unit the
    /// encoder published.
    ///
    /// The parameter sets go back in front of every sync sample because
    /// 14496-15 forbids them inside a sample under `avc1`/`hvc1` — the sink
    /// stripped exactly these on the way in, and the encoder had prepended
    /// them to every IDR.
    fn annex_b_access_unit_from_sample(
        sample_bytes: &[u8],
        sample_entry: &RecordedVideoSampleEntry,
        is_sync_point: bool,
        recording_path: &str,
    ) -> Result<Vec<u8>> {
        let mut access_unit = Vec::with_capacity(sample_bytes.len() + ANNEX_B_START_CODE.len());
        if is_sync_point {
            for parameter_set in &sample_entry.parameter_set_nal_units {
                access_unit.extend_from_slice(&ANNEX_B_START_CODE);
                access_unit.extend_from_slice(parameter_set);
            }
        }

        let prefix_bytes = usize::from(sample_entry.nal_unit_length_prefix_bytes);
        let mut at = 0;
        while at < sample_bytes.len() {
            let length_prefix = sample_bytes
                .get(at..at + prefix_bytes)
                .ok_or_else(|| truncated_sample(recording_path))?;
            at += prefix_bytes;
            let nal_unit_bytes = length_prefix
                .iter()
                .fold(0usize, |length, byte| (length << 8) | usize::from(*byte));
            let nal_unit = sample_bytes
                .get(at..at + nal_unit_bytes)
                .ok_or_else(|| truncated_sample(recording_path))?;
            at += nal_unit_bytes;
            access_unit.extend_from_slice(&ANNEX_B_START_CODE);
            access_unit.extend_from_slice(nal_unit);
        }
        Ok(access_unit)
    }

    fn truncated_sample(recording_path: &str) -> Error {
        Error::Runtime(format!(
            "RecordedMp4TrackReplaySource: a sample in {recording_path} declares a NAL unit \
             longer than the sample itself, so the track is not the length-prefixed shape its \
             sample entry claims"
        ))
    }

    /// Read a recording's video track back into the access units that made it.
    ///
    /// The file is read whole rather than streamed: a sample is located from
    /// its fragment's own offset in the file, and a rig recording is bounded
    /// by the run that wrote it.
    fn demux_recorded_video_track(recording_path: &str) -> Result<RecordedVideoTrackReplay> {
        let file_bytes = std::fs::read(recording_path).map_err(|read_failure| {
            Error::Runtime(format!(
                "RecordedMp4TrackReplaySource: {recording_path} could not be read: {read_failure}"
            ))
        })?;
        let (moov, fragments) = read_moov_and_fragments(&file_bytes, recording_path)?;
        let (video_track_id, sample_entry) = read_video_sample_entry(&moov, recording_path)?;

        // A `trun` entry may omit its size and inherit it: 14496-12 §8.8.7.1
        // lets `tfhd` override §8.8.3.2's `trex`.
        let default_sample_size_from_trex = moov
            .mvex
            .as_ref()
            .and_then(|mvex| {
                mvex.trex
                    .iter()
                    .find(|trex| trex.track_id == video_track_id)
            })
            .map(|trex| trex.default_sample_size);
        let default_sample_flags_from_trex = moov
            .mvex
            .as_ref()
            .and_then(|mvex| {
                mvex.trex
                    .iter()
                    .find(|trex| trex.track_id == video_track_id)
            })
            .map(|trex| trex.default_sample_flags);

        let mut access_units = Vec::new();
        for (moof_start, fragment) in &fragments {
            for track_fragment in fragment
                .traf
                .iter()
                .filter(|traf| traf.tfhd.track_id == video_track_id)
            {
                for run in &track_fragment.trun {
                    let mut at = i64::try_from(*moof_start)
                        .ok()
                        .and_then(|start| start.checked_add(i64::from(run.data_offset.unwrap_or(0))))
                        .and_then(|offset| usize::try_from(offset).ok())
                        .ok_or_else(|| {
                            Error::Runtime(format!(
                                "RecordedMp4TrackReplaySource: a fragment in {recording_path} \
                                 points its samples outside the file"
                            ))
                        })?;
                    for entry in &run.entries {
                        let sample_bytes_length = entry
                            .size
                            .or(track_fragment.tfhd.default_sample_size)
                            .or(default_sample_size_from_trex)
                            .ok_or_else(|| {
                                Error::Runtime(format!(
                                    "RecordedMp4TrackReplaySource: a sample in {recording_path} \
                                     states no size and inherits none, so its bytes cannot be \
                                     found"
                                ))
                            })? as usize;
                        let sample_bytes = file_bytes
                            .get(at..at + sample_bytes_length)
                            .ok_or_else(|| {
                                Error::Runtime(format!(
                                    "RecordedMp4TrackReplaySource: a fragment in \
                                     {recording_path} points past the end of the file — a \
                                     recording truncated mid-`mdat`"
                                ))
                            })?;
                        at += sample_bytes_length;

                        let sample_flags = entry
                            .flags
                            .or(track_fragment.tfhd.default_sample_flags)
                            .or(default_sample_flags_from_trex)
                            .unwrap_or(0);
                        let is_sync_point = sample_flags & SAMPLE_FLAG_IS_NON_SYNC_SAMPLE == 0;
                        access_units.push(ReplayableAccessUnit {
                            annex_b_access_unit_bytes: annex_b_access_unit_from_sample(
                                sample_bytes,
                                &sample_entry,
                                is_sync_point,
                                recording_path,
                            )?,
                            is_sync_point,
                        });
                    }
                }
            }
        }

        if access_units.is_empty() {
            return Err(Error::Runtime(format!(
                "RecordedMp4TrackReplaySource: {recording_path}'s video track carries no \
                 samples — a recording whose `moov` landed and whose fragments did not"
            )));
        }
        Ok(RecordedVideoTrackReplay {
            codec: sample_entry.codec,
            coded_width: sample_entry.coded_width,
            coded_height: sample_entry.coded_height,
            access_units,
        })
    }

    #[streamlib::sdk::processor(
        description = "Replays a recording's video track as encoded-frame bags, for the decode-back arm",
        execution = continuous(interval_ms = 100),
        config = crate::linux_rig::RecordedMp4TrackReplaySourceConfig,
        output(
            "encoded_video",
            description = "Access units read back out of the recording"
        ),
    )]
    pub struct RecordedMp4TrackReplaySource {
        replay: Option<RecordedVideoTrackReplay>,
        access_units_published: usize,
        sync_points_published: u64,
    }

    impl ContinuousProcessor for RecordedMp4TrackReplaySource::Processor {
        fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
            let replay = demux_recorded_video_track(&self.config.recording_path)?;
            if replay.codec.as_wire_str() != self.config.expected_codec {
                return Err(Error::Runtime(format!(
                    "RecordedMp4TrackReplaySource: {} holds a `{}` track and this run wired the \
                     `{}` decoder — run the arm with `--codec {}`",
                    self.config.recording_path,
                    replay.codec.as_wire_str(),
                    self.config.expected_codec,
                    replay.codec.as_wire_str(),
                )));
            }
            tracing::info!(
                recording = self.config.recording_path,
                codec = replay.codec.as_wire_str(),
                access_units = replay.access_units.len(),
                sync_points = replay
                    .access_units
                    .iter()
                    .filter(|access_unit| access_unit.is_sync_point)
                    .count(),
                width = replay.coded_width,
                height = replay.coded_height,
                "RecordedMp4TrackReplaySource: recording demuxed"
            );
            self.replay = Some(replay);
            Ok(())
        }

        fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
            tracing::info!(
                access_units_published = self.access_units_published,
                "RecordedMp4TrackReplaySource: teardown"
            );
            self.replay = None;
            Ok(())
        }

        fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
            let Some(replay) = self.replay.as_ref() else {
                return Ok(());
            };
            let Some(access_unit) = replay.access_units.get(self.access_units_published) else {
                return Ok(());
            };

            if access_unit.is_sync_point {
                self.sync_points_published += 1;
            }
            let frame = EncodedVideoFrame {
                codec: replay.codec,
                annex_b_access_unit_bytes: access_unit.annex_b_access_unit_bytes.clone(),
                is_sync_point: access_unit.is_sync_point,
                // The first access unit is a sync point, so the first group is
                // zero and the count is one ahead of the index.
                group_index: self.sync_points_published.saturating_sub(1),
                sequence_index: self.access_units_published as u64,
                width: replay.coded_width,
                height: replay.coded_height,
                // The re-prepended SPS carries the VUI the encoder minted, and
                // a parsed VUI outranks a producer's attestation — so stating
                // one here could only disagree with the bitstream.
                color: None,
            };
            // Stamped at publication rather than at the recorded decode time:
            // a `tfdt` is an offset from the recording's own epoch, not an
            // instant on this run's monotonic clock.
            self.outputs.write("encoded_video", &frame)?;

            self.access_units_published += 1;
            if self.access_units_published == replay.access_units.len() {
                tracing::info!(
                    access_units_published = self.access_units_published,
                    "RecordedMp4TrackReplaySource: the recording is fully replayed"
                );
            }
            Ok(())
        }
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
                    let named_source = next_value_for_this_flag()?;
                    arguments.source_arm = match named_source.as_str() {
                        "fixture" => RoundTripSourceArm::PsnrReferenceFixtures,
                        "camera" => RoundTripSourceArm::Camera,
                        recorded if recorded.starts_with(RECORDED_MP4_SOURCE_PREFIX) => {
                            let recording_path =
                                recorded[RECORDED_MP4_SOURCE_PREFIX.len()..].to_string();
                            if recording_path.is_empty() {
                                return Err(Error::Runtime(
                                    "--source mp4: names no file; it takes `mp4:<path>`".into(),
                                ));
                            }
                            RoundTripSourceArm::RecordedMp4File { recording_path }
                        }
                        unknown => {
                            return Err(Error::Runtime(format!(
                                "--source {unknown} is none of `fixture`, `camera` or \
                                 `mp4:<path>`"
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
        let source = match &arguments.source_arm {
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
            RoundTripSourceArm::RecordedMp4File { recording_path } => app
                .add_local::<RecordedMp4TrackReplaySource::Processor>(
                RecordedMp4TrackReplaySourceConfig {
                    recording_path: recording_path.clone(),
                    expected_codec: arguments
                        .codec_arm
                        .encoded_video_codec()
                        .as_wire_str()
                        .to_string(),
                },
                Some("mp4_replay"),
            )?,
        };
        let (encoder_class_import_path, decoder_class_import_path) =
            arguments.codec_arm.encoder_and_decoder_class_import_paths();
        // The replay arm publishes the encoded domain itself, so there is
        // nothing left for an encoder to do: what it would produce is what the
        // recording already holds, and re-encoding it would score a second
        // generation rather than the container.
        let encoder = match arguments.source_arm {
            RoundTripSourceArm::RecordedMp4File { .. } => None,
            _ => Some(app.add(
                encoder_class_import_path,
                serde_json::json!({
                    "keyframe_interval_seconds": ENCODER_KEYFRAME_INTERVAL_SECONDS
                }),
                Some("encoder"),
            )?),
        };
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

        match &encoder {
            Some(encoder) => {
                app.connect((&source, "video"), (encoder, "video"))?;
                app.connect((encoder, "encoded_video"), (&decoder, "encoded_video"))?;
            }
            None => {
                app.connect((&source, "encoded_video"), (&decoder, "encoded_video"))?;
            }
        }
        app.connect((&decoder, "video"), (&display, "video"))?;

        tracing::info!(
            source_arm = ?arguments.source_arm,
            codec_arm = ?arguments.codec_arm,
            "codec_roundtrip_rig: {} -> {}decoder -> display",
            source.display_name(),
            if encoder.is_some() { "encoder -> " } else { "" },
        );
        app.run()
    }
}
