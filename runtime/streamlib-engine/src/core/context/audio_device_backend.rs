// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The audio device seam: the one path anything opens an audio stream through.
//!
//! It sits beside the audio clock rather than inside a built-in, so audio
//! reaches hardware the way every other device class does — through a
//! handle-shaped primitive. The built-ins, the null backend and every test
//! open their streams here; there is no second audio device path.

use std::sync::{Arc, OnceLock};

use super::SharedAudioClock;
use super::silent_null_audio_device_backend::SilentNullAudioDeviceBackend;
use crate::core::Result;

/// How the scalars an audio stream carries are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSampleFormat {
    /// 32-bit little-endian float.
    F32,
    /// 16-bit little-endian signed integer.
    I16,
}

impl AudioSampleFormat {
    /// Bytes one scalar occupies in a payload.
    pub fn bytes_per_sample(self) -> usize {
        match self {
            AudioSampleFormat::F32 => 4,
            AudioSampleFormat::I16 => 2,
        }
    }
}

/// What a stream carries in either direction, fixed for its lifetime.
///
/// One type for capture and playback because the triple is the same one: a
/// second, direction-named copy of it would let the two drift, and a block
/// crossing from a microphone to a speaker is compared against exactly this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioStreamFormat {
    /// Sample rate in hertz.
    pub sample_rate: u32,
    /// Channel count every payload is interleaved by.
    pub channels: u32,
    /// How to read the scalars in a payload.
    pub sample_format: AudioSampleFormat,
}

impl AudioStreamFormat {
    /// Bytes a block of `sample_count` per-channel samples occupies once
    /// interleaved.
    pub fn interleaved_byte_count_for(self, sample_count: u32) -> usize {
        sample_count as usize * self.channels as usize * self.sample_format.bytes_per_sample()
    }
}

/// One block of samples as a device delivered it, borrowed for the length of
/// the hand-off — a callee that keeps the samples copies them.
#[derive(Debug, Clone, Copy)]
pub struct CapturedAudioBlockFromDevice<'a> {
    /// Interleaved little-endian scalars, read according to the stream's
    /// [`AudioStreamFormat::sample_format`].
    pub interleaved_sample_bytes: &'a [u8],
    /// Per-channel sample count: the payload carries `sample_count × channels`
    /// scalars.
    pub sample_count: u32,
    /// Monotonic timestamp of the block's first sample as the device timed it,
    /// never the instant of delivery — it is what makes joining audio to a
    /// camera frame a subtraction.
    pub first_sample_timestamp_ns: i64,
}

/// What a capture stream calls with each block it captures.
///
/// Runs on the backend's own callback thread, so it must not block, and it
/// must not re-enter the stream it was installed on: a backend is free to hold
/// the stream's own lock across this call, so calling back into
/// [`AudioCaptureStream::stop_delivering`] from here deadlocks it.
pub type CapturedAudioBlockHandOff = Box<dyn Fn(CapturedAudioBlockFromDevice<'_>) + Send + Sync>;

/// What a caller asks a backend to open a stream for, in either direction.
pub struct AudioDeviceStreamRequest {
    /// Backend-named device. `None` takes the backend's default; a name the
    /// backend cannot open is an error, never a quiet landing on a different
    /// device.
    pub device_id: Option<String>,
    /// The clock a backend with no device paces its blocks from. A backend
    /// whose device provides the cadence ignores it, so device ticks and timer
    /// ticks never interleave.
    pub deviceless_pacing_clock: SharedAudioClock,
}

/// Why a stream stopped serving its device without being asked to.
///
/// Text rather than a core [`crate::core::Error`]: it is read repeatedly off an
/// [`AudioStreamLivenessReport`] long after the thread that produced it is
/// gone, and `Error` is not `Clone`. What an owner does with it is decide —
/// log it, fail, retry — and every one of those needs the reason to survive
/// being read.
#[derive(Clone, PartialEq, Eq)]
pub struct AudioStreamFailureReason(String);

impl AudioStreamFailureReason {
    /// State why a stream stopped serving its device.
    pub fn of(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

impl std::fmt::Display for AudioStreamFailureReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

// Written by hand rather than derived, and the same text as `Display`: this is
// read in a log field, where the derived `AudioStreamFailureReason("…")`
// wrapper would be noise around the only part anyone acts on.
impl std::fmt::Debug for AudioStreamFailureReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Whether a stream is still serving its device, and why it stopped if it is
/// not.
///
/// Handed out and cloned rather than answered off the stream itself, because
/// the thread that would act on the answer is never the one holding the
/// stream: a source owns its stream on the processor and does its work on a
/// publishing thread, so it hands that thread a report instead of the stream.
///
/// A device that dies is otherwise indistinguishable from one that went quiet
/// — a finished reader thread looks exactly like a running one, and stopping
/// the stream still succeeds — so without this an owner has no way to notice,
/// retry, or fail.
///
/// Read-only by construction: stating a failure needs an
/// [`AudioStreamFailureRecorder`], which only the arm that owns the device
/// holds. An owner cannot forge a death it then reads back as real.
///
/// A failure latches for the life of the stream, and nothing clears it.
/// Restarting delivery on a stream whose device died does not revive it — the
/// device is gone, and a report that forgot would let a source go back to
/// looking healthy, which is the defect this exists to remove. Recovering
/// means opening a new stream, which mints a new report with it.
#[derive(Clone, Debug)]
pub struct AudioStreamLivenessReport {
    failure_that_ended_the_stream: Arc<OnceLock<AudioStreamFailureReason>>,
}

impl AudioStreamLivenessReport {
    /// A report for a stream nothing can stop serving its device.
    ///
    /// The null backend's whole answer: a stream paced by a timer against no
    /// device has no device to lose, so there is no recorder to pair this
    /// with. Its own constructor rather than a recorder whose failure branch
    /// is unreachable, because "never dies" is part of that arm's design and
    /// deserves to be stated.
    pub fn of_a_stream_that_cannot_fail() -> Self {
        Self {
            failure_that_ended_the_stream: Arc::new(OnceLock::new()),
        }
    }

    /// Why the stream stopped serving its device on its own, or `None` while
    /// it is still serving it.
    ///
    /// A stream its owner stopped deliberately answers `None`: being told to
    /// stop is not a failure, and reporting it as one would make the signal
    /// useless at exactly the moment an owner reads it.
    pub fn failure_that_ended_the_stream(&self) -> Option<AudioStreamFailureReason> {
        self.failure_that_ended_the_stream.get().cloned()
    }
}

/// The write side of a stream's liveness, held by the arm that owns the device.
///
/// Separate from the report so the direction is structural rather than a rule
/// in prose: an owner reads, an arm states, and neither can do the other's job.
#[derive(Clone, Debug)]
pub struct AudioStreamFailureRecorder {
    failure_that_ended_the_stream: Arc<OnceLock<AudioStreamFailureReason>>,
}

impl AudioStreamFailureRecorder {
    /// Mint the pair a stream opens with: the recorder its own device thread
    /// keeps, and the report its owner reads.
    pub fn recording_into_a_new_report() -> (Self, AudioStreamLivenessReport) {
        let failure_that_ended_the_stream = Arc::new(OnceLock::new());
        (
            Self {
                failure_that_ended_the_stream: Arc::clone(&failure_that_ended_the_stream),
            },
            AudioStreamLivenessReport {
                failure_that_ended_the_stream,
            },
        )
    }

    /// Record why the stream stopped serving its device.
    ///
    /// The first reason recorded is the one kept — `OnceLock` is what makes
    /// that a property of the type rather than a convention: a stream on its
    /// way down reports more than once, and the first names the cause where
    /// everything after it names a consequence.
    pub fn record_the_failure_that_ended_the_stream(&self, reason: AudioStreamFailureReason) {
        let _ = self.failure_that_ended_the_stream.set(reason);
    }
}

/// A capture stream a backend opened.
pub trait AudioCaptureStream: Send {
    /// The rate, channel count and scalar encoding of every block this stream
    /// delivers.
    fn stream_format(&self) -> AudioStreamFormat;

    /// Whether this stream is still capturing, readable from whatever thread
    /// the owner does its work on.
    ///
    /// The report belongs to the stream and outlives any one delivery, so an
    /// owner that took it at open reads the same answer after a stop as
    /// during a run — and a failure it names outlives
    /// [`Self::start_delivering_to`] too, because restarting delivery does not
    /// bring a device back.
    fn liveness_report(&self) -> AudioStreamLivenessReport;

    /// Begin delivering captured blocks to `hand_off`, replacing any hand-off
    /// an earlier call installed.
    fn start_delivering_to(&mut self, hand_off: CapturedAudioBlockHandOff) -> Result<()>;

    /// Stop delivering. The hand-off is not called again once this returns.
    fn stop_delivering(&mut self) -> Result<()>;
}

/// One block of samples a device is asking for, borrowed for the length of the
/// hand-off — the buffer is the device's and is invalid the moment it returns.
#[derive(Debug)]
pub struct AudioBlockRequestedByDevice<'a> {
    /// Interleaved little-endian scalars to write, in the stream's
    /// [`AudioStreamFormat::sample_format`].
    ///
    /// The hand-off fills all of it. A hand-off holding fewer samples than
    /// this has room for writes silence into the remainder and counts what it
    /// invented — a device buffer left partly unwritten plays whatever the
    /// previous cycle put there.
    pub interleaved_sample_bytes_to_fill: &'a mut [u8],
    /// Per-channel sample count the buffer has room for: it holds
    /// `sample_count × channels` scalars.
    pub sample_count: u32,
}

/// What a playback stream calls each time its device needs samples.
///
/// Runs on the backend's own callback thread, so it must not block, and it
/// must not re-enter the stream it was installed on: a backend is free to hold
/// the stream's own lock across this call, so calling back into
/// [`AudioPlaybackStream::stop_requesting`] from here deadlocks it.
pub type AudioBlockForPlaybackHandOff = Box<dyn Fn(AudioBlockRequestedByDevice<'_>) + Send + Sync>;

/// A playback stream a backend opened.
pub trait AudioPlaybackStream: Send {
    /// The rate, channel count and scalar encoding every block handed to this
    /// stream must already be in. Conversion belongs to the read-side window
    /// stage at a consuming port, not to a backend: a stream reports what it
    /// opened and a caller matches it.
    fn stream_format(&self) -> AudioStreamFormat;

    /// Whether this stream is still playing, readable from whatever thread the
    /// owner does its work on — the capture seam's report, in the direction a
    /// sink cares about, under the same latching rule.
    fn liveness_report(&self) -> AudioStreamLivenessReport;

    /// Begin asking `hand_off` for samples, replacing any hand-off an earlier
    /// call installed.
    fn start_requesting_from(&mut self, hand_off: AudioBlockForPlaybackHandOff) -> Result<()>;

    /// Stop asking. The hand-off is not called again once this returns.
    fn stop_requesting(&mut self) -> Result<()>;
}

/// The audio device seam every audio stream is opened through.
pub trait AudioDeviceBackend: Send + Sync {
    /// The arm's name, for the one probe log line and for error text.
    fn backend_name(&self) -> &'static str;

    /// Open a capture stream against the named device, or the backend's
    /// default when none is named.
    fn open_capture_stream(
        &self,
        request: &AudioDeviceStreamRequest,
    ) -> Result<Box<dyn AudioCaptureStream>>;

    /// Open a playback stream against the named device, or the backend's
    /// default when none is named.
    fn open_playback_stream(
        &self,
        request: &AudioDeviceStreamRequest,
    ) -> Result<Box<dyn AudioPlaybackStream>>;
}

/// Shared handle to the backend the chain probed.
pub type SharedAudioDeviceBackend = Arc<dyn AudioDeviceBackend>;

/// Why an arm of the chain cannot serve, in the words the demotion log line
/// carries.
///
/// Not a core [`crate::core::Error`]: nothing failed that a caller must handle.
/// The chain has another arm, and this is what tells a reader whether the
/// library was absent, the daemon was, or the device was.
#[derive(Debug)]
pub struct AudioDeviceBackendArmUnavailableReason(String);

impl AudioDeviceBackendArmUnavailableReason {
    /// State why this arm cannot serve.
    pub fn of(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

impl std::fmt::Display for AudioDeviceBackendArmUnavailableReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// What opening one arm of the chain yields.
type AudioDeviceBackendArmOpenOutcome =
    std::result::Result<SharedAudioDeviceBackend, AudioDeviceBackendArmUnavailableReason>;

/// One arm of the chain: its name, and the attempt to open it.
///
/// The attempt is held unrun so the chain's *order* can be read — and asserted
/// — without loading an audio library or touching a device. Private to the
/// walk: nothing outside chooses an arm, because no dial selects a backend.
struct AudioDeviceBackendArm {
    backend_name: &'static str,
    open: Box<dyn FnOnce() -> AudioDeviceBackendArmOpenOutcome>,
}

impl AudioDeviceBackendArm {
    /// The only way to build one, so an arm's name always comes from the same
    /// place as the attempt that opens it.
    // A platform whose floor is the null backend has no arms to construct.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn named(
        backend_name: &'static str,
        open: impl FnOnce() -> AudioDeviceBackendArmOpenOutcome + 'static,
    ) -> Self {
        Self {
            backend_name,
            open: Box::new(open),
        }
    }
}

static PROBED_AUDIO_DEVICE_BACKEND: OnceLock<SharedAudioDeviceBackend> = OnceLock::new();

/// Probe the audio backend chain once per process and log the arm it chose.
///
/// No configuration dial selects an arm and no environment variable overrides
/// the probe. The last arm needs no audio library at all, so the chain always
/// resolves to something and a machine with no audio runs the graph rather than
/// failing to start.
pub fn probe_audio_device_backend() -> SharedAudioDeviceBackend {
    Arc::clone(PROBED_AUDIO_DEVICE_BACKEND.get_or_init(|| {
        let backend = first_audio_device_backend_arm_that_opens();
        tracing::info!(
            audio_backend = backend.backend_name(),
            "audio device backend chain probed"
        );
        backend
    }))
}

/// Walk the chain in order and take the first arm that actually opens.
///
/// An arm is chosen by opening, not by loading: a library that resolves but
/// yields no usable connection — `libpipewire` present with no daemon
/// answering, the ordinary container case — demotes exactly as a missing
/// library does. Probing on presence alone would strand precisely the machines
/// the chain exists to serve.
fn first_audio_device_backend_arm_that_opens() -> SharedAudioDeviceBackend {
    first_audio_device_backend_arm_that_opens_among(platform_audio_device_backend_arms())
        .unwrap_or_else(|| Arc::new(SilentNullAudioDeviceBackend))
}

/// Take the first arm that opens, logging each demotion with the reason that
/// caused it.
///
/// Separate from the platform arm list so the walk is exercised by arms that
/// fail on purpose: the order this chain demotes in is the decided behaviour
/// (`docs/plan/ARCHITECTURE.md` §Media I/O `[audio-subsystem]`), and asserting
/// it in prose proves nothing.
fn first_audio_device_backend_arm_that_opens_among(
    arms: impl IntoIterator<Item = AudioDeviceBackendArm>,
) -> Option<SharedAudioDeviceBackend> {
    for arm in arms {
        match (arm.open)() {
            Ok(backend) => return Some(backend),
            Err(reason) => {
                // Info rather than warn: a machine with no audio server is a
                // supported environment, and the next arm is the answer rather
                // than the consolation prize. The reason is what tells a reader
                // whether the library was absent or the device was.
                tracing::info!(
                    audio_backend = arm.backend_name,
                    %reason,
                    "audio device backend chain: demoting to the next arm"
                );
            }
        }
    }
    None
}

/// The chain's real arms, in the order the plan decided: PipeWire, else ALSA,
/// else — once both have declined — the null backend the walk falls through to.
#[cfg(target_os = "linux")]
fn platform_audio_device_backend_arms() -> Vec<AudioDeviceBackendArm> {
    use crate::linux::alsa_audio_device_backend::AlsaAudioDeviceBackend;
    use crate::linux::pipewire_audio_device_backend::PipeWireAudioDeviceBackend;

    vec![
        AudioDeviceBackendArm::named("pipewire", || {
            PipeWireAudioDeviceBackend::load_and_connect()
                .map(|backend| Arc::new(backend) as SharedAudioDeviceBackend)
        }),
        AudioDeviceBackendArm::named("alsa", || {
            AlsaAudioDeviceBackend::load_and_open()
                .map(|backend| Arc::new(backend) as SharedAudioDeviceBackend)
        }),
    ]
}

/// The platform floor is Linux; every other target lands on the null backend,
/// which needs no audio library and captures silence.
#[cfg(not(target_os = "linux"))]
fn platform_audio_device_backend_arms() -> Vec<AudioDeviceBackendArm> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An arm that opens, standing in for a real backend so the walk can be
    /// driven without an audio library or a device.
    struct ArmThatOpened(&'static str);

    impl AudioDeviceBackend for ArmThatOpened {
        fn backend_name(&self) -> &'static str {
            self.0
        }

        fn open_capture_stream(
            &self,
            _request: &AudioDeviceStreamRequest,
        ) -> Result<Box<dyn AudioCaptureStream>> {
            unreachable!("the walk only ever opens the arm, never a stream on it")
        }

        fn open_playback_stream(
            &self,
            _request: &AudioDeviceStreamRequest,
        ) -> Result<Box<dyn AudioPlaybackStream>> {
            unreachable!("the walk only ever opens the arm, never a stream on it")
        }
    }

    fn an_arm_that_opens(backend_name: &'static str) -> AudioDeviceBackendArm {
        AudioDeviceBackendArm::named(backend_name, move || {
            Ok(Arc::new(ArmThatOpened(backend_name)) as SharedAudioDeviceBackend)
        })
    }

    fn an_arm_that_declines(backend_name: &'static str) -> AudioDeviceBackendArm {
        AudioDeviceBackendArm::named(backend_name, move || {
            Err(AudioDeviceBackendArmUnavailableReason::of(format!(
                "{backend_name} was made to decline by this test"
            )))
        })
    }

    #[test]
    fn the_chain_is_probed_once_and_hands_back_the_same_backend_every_time() {
        let first = probe_audio_device_backend();
        let second = probe_audio_device_backend();
        assert!(
            Arc::ptr_eq(&first, &second),
            "the chain is probed once per process, so every caller shares one backend"
        );
    }

    /// The chain resolves on any machine, which is what lets the wheel import
    /// and run in `manylinux_2_28` and in a headless container — neither of
    /// which carries an audio library at all.
    #[test]
    fn the_chain_always_lands_on_an_arm_however_little_audio_the_machine_has() {
        let backend = probe_audio_device_backend();
        assert!(
            ["pipewire", "alsa", "silent-null"].contains(&backend.backend_name()),
            "the chain resolved to an arm nothing declares: {}",
            backend.backend_name()
        );
    }

    /// The decided order, read off the list the probe itself walks rather than
    /// restated beside it — so an arm inserted in the wrong place fails here.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_linux_chain_offers_pipewire_then_alsa_before_falling_through_to_null() {
        let arm_names: Vec<&str> = platform_audio_device_backend_arms()
            .iter()
            .map(|arm| arm.backend_name)
            .collect();
        assert_eq!(
            arm_names,
            ["pipewire", "alsa"],
            "the plan decides PipeWire, else ALSA, else null — and the null arm is the \
             fall-through the walk takes when this list is exhausted, never an entry in it"
        );
    }

    #[test]
    fn the_walk_takes_the_first_arm_that_opens_and_asks_no_arm_behind_it() {
        let chosen = first_audio_device_backend_arm_that_opens_among([
            an_arm_that_opens("first"),
            AudioDeviceBackendArm::named("second", || {
                unreachable!("an arm behind one that opened is never asked")
            }),
        ])
        .expect("an arm opened");
        assert_eq!(chosen.backend_name(), "first");
    }

    /// Each arm made to fail in turn: the chain demotes past exactly the arms
    /// that declined and stops at the first that did not.
    #[test]
    fn the_walk_demotes_past_every_arm_that_declines_in_the_order_it_was_given() {
        for failing_arm_count in 0..3 {
            let arms: Vec<AudioDeviceBackendArm> = ["first", "second", "third"]
                .into_iter()
                .enumerate()
                .map(|(position, backend_name)| {
                    if position < failing_arm_count {
                        an_arm_that_declines(backend_name)
                    } else {
                        an_arm_that_opens(backend_name)
                    }
                })
                .collect();
            let chosen = first_audio_device_backend_arm_that_opens_among(arms)
                .expect("one arm was left able to open");
            assert_eq!(
                chosen.backend_name(),
                ["first", "second", "third"][failing_arm_count],
                "with the leading {failing_arm_count} arm(s) declining, the chain must land \
                 on the next one rather than skipping it or stopping short"
            );
        }
    }

    /// The fall-through the wheel depends on: with every arm declining there is
    /// still a backend, because `manylinux_2_28` and headless containers carry
    /// no audio library at all and must run the graph rather than fail to start.
    #[test]
    fn a_chain_whose_arms_all_decline_falls_through_to_the_null_backend() {
        assert!(
            first_audio_device_backend_arm_that_opens_among([
                an_arm_that_declines("first"),
                an_arm_that_declines("second"),
            ])
            .is_none(),
            "no arm opened, so the walk yields nothing and the caller supplies the null arm"
        );
        assert_eq!(
            SilentNullAudioDeviceBackend.backend_name(),
            "silent-null",
            "the arm the walk falls through to"
        );
    }

    #[test]
    fn a_scalars_width_is_the_width_the_wire_dtype_names() {
        assert_eq!(AudioSampleFormat::F32.bytes_per_sample(), 4);
        assert_eq!(AudioSampleFormat::I16.bytes_per_sample(), 2);
    }

    #[test]
    fn a_stream_nothing_has_gone_wrong_with_reports_no_failure() {
        let (_recorder, report) = AudioStreamFailureRecorder::recording_into_a_new_report();
        assert!(
            report.failure_that_ended_the_stream().is_none(),
            "a report that answered a failure before one happened would fire on every \
             healthy stream, which is worth no more than the silence it replaces"
        );
    }

    /// The arm that cannot lose a device answers the same question, so an
    /// owner writes one piece of code and it means the same thing everywhere.
    #[test]
    fn a_stream_that_cannot_fail_reports_no_failure() {
        assert!(
            AudioStreamLivenessReport::of_a_stream_that_cannot_fail()
                .failure_that_ended_the_stream()
                .is_none()
        );
    }

    /// The whole point of the shape: the thread that records the failure is
    /// never the thread that reads it.
    #[test]
    fn a_failure_the_arm_records_is_read_through_the_owners_own_clone() {
        let (recorder, report) = AudioStreamFailureRecorder::recording_into_a_new_report();
        let report_the_publishing_thread_holds = report.clone();

        recorder.record_the_failure_that_ended_the_stream(AudioStreamFailureReason::of(
            "the device delivered nothing for 25 consecutive waits",
        ));

        assert_eq!(
            report_the_publishing_thread_holds
                .failure_that_ended_the_stream()
                .map(|reason| reason.to_string()),
            Some("the device delivered nothing for 25 consecutive waits".to_string())
        );
    }

    /// A dying stream reports more than once — the read that failed, then the
    /// teardown behind it — and the first is the one that says what happened.
    #[test]
    fn the_first_reason_recorded_is_the_one_the_owner_reads() {
        let (recorder, report) = AudioStreamFailureRecorder::recording_into_a_new_report();

        recorder.record_the_failure_that_ended_the_stream(AudioStreamFailureReason::of(
            "what actually killed the stream",
        ));
        recorder.record_the_failure_that_ended_the_stream(AudioStreamFailureReason::of(
            "a consequence of the first",
        ));

        assert_eq!(
            report
                .failure_that_ended_the_stream()
                .map(|reason| reason.to_string()),
            Some("what actually killed the stream".to_string()),
            "the reason kept has to be the cause, not the last thing the teardown said"
        );
    }

    /// The reason is read in a log field, so its `Debug` is the text and not a
    /// wrapper around it.
    #[test]
    fn a_reason_reads_the_same_whether_it_is_printed_for_a_human_or_a_log_field() {
        let reason = AudioStreamFailureReason::of("the device went away");
        assert_eq!(format!("{reason:?}"), "the device went away");
        assert_eq!(reason.to_string(), "the device went away");
    }
}
