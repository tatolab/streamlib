// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The audio backend chain's second arm: ALSA, reached entirely at runtime.
//!
//! `libasound.so.2` is opened with `libloading` and every entry point is a
//! `dlsym` result, the way `vulkan/rhi/drm_modifier_probe.rs` reaches
//! `libEGL.so.1`. Nothing here links an audio library, so the wheel's
//! `DT_NEEDED` set does not grow. Unlike the PipeWire arm this one needs no
//! compiled shim: ALSA's API is opaque-pointer C with no header-only inline
//! layer behind it.
//!
//! ALSA offers no usable callback: `snd_async_add_pcm_handler` delivers on
//! `SIGIO`, where almost nothing is legal to call. So this arm owns a reader
//! thread that waits for a period, reads the device's own timing out of
//! `snd_pcm_status`, and hands the block off — and, for playback, a writer
//! thread that waits for a period of room and asks for one. The cadence is
//! still the device's in both directions: the wait is what the device
//! releases.

use std::ffi::{CStr, CString, c_char, c_int, c_long, c_uint, c_ulong, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use libloading::Library;

use crate::core::context::{
    AudioBlockForPlaybackHandOff, AudioBlockRequestedByDevice, AudioCaptureStream,
    AudioDeviceBackend, AudioDeviceBackendArmUnavailableReason, AudioDeviceStreamRequest,
    AudioPlaybackStream, AudioSampleFormat, AudioStreamFailureReason, AudioStreamFailureRecorder,
    AudioStreamFormat, AudioStreamLivenessReport, CapturedAudioBlockFromDevice,
    CapturedAudioBlockHandOff,
};
use crate::core::execution::ThreadPriority;
use crate::core::media_clock::MediaClock;
use crate::core::{Error, Result};
use crate::linux::thread_priority::apply_thread_priority;

/// The versioned soname. ALSA's is decades stable, and the bare `.so` symlink
/// ships only in `libasound2-dev` — which the machines this arm exists for do
/// not have.
const ALSA_LIBRARY_SONAME: &str = "libasound.so.2";

/// The PCM name opened when no caller names a device, in either direction.
///
/// `default` and never a raw `hw:` node: raw hardware access bypasses any
/// daemon holding the card and returns `EBUSY` (measured on the rig). A caller
/// that names `hw:0,0` gets exactly that, because a named device is a wiring
/// statement.
const DEFAULT_PCM_NAME: &str = "default";

/// `snd_pcm_uframes_t` — `unsigned long`, per `<alsa/pcm.h>`.
type SndPcmUframes = c_ulong;

/// `snd_pcm_sframes_t` — `signed long`, per `<alsa/pcm.h>`.
type SndPcmSframes = c_long;

/// `SND_PCM_STREAM_PLAYBACK`, the first variant of `snd_pcm_stream_t`.
const SND_PCM_STREAM_PLAYBACK: c_int = 0;

/// `SND_PCM_STREAM_CAPTURE`, the second variant of `snd_pcm_stream_t`.
const SND_PCM_STREAM_CAPTURE: c_int = 1;

/// Blocking mode: neither `SND_PCM_NONBLOCK` nor `SND_PCM_ASYNC`. The reader
/// thread bounds its own waits, so non-blocking buys nothing.
const SND_PCM_OPEN_MODE_BLOCKING: c_int = 0;

/// `SND_PCM_ACCESS_RW_INTERLEAVED`, the fourth variant of `snd_pcm_access_t` —
/// the layout `AudioBlock` states on the wire.
const SND_PCM_ACCESS_RW_INTERLEAVED: c_int = 3;

/// `SND_PCM_FORMAT_S16_LE`, the fourth variant of `snd_pcm_format_t`.
const SND_PCM_FORMAT_S16_LE: c_int = 2;

/// `SND_PCM_FORMAT_FLOAT_LE`, the sixteenth variant of `snd_pcm_format_t`.
const SND_PCM_FORMAT_FLOAT_LE: c_int = 14;

/// `SND_PCM_TSTAMP_ENABLE`, the second variant of `snd_pcm_tstamp_t`. Without
/// it `snd_pcm_status` leaves `htstamp` zeroed and the device reports no timing
/// at all.
const SND_PCM_TSTAMP_ENABLE: c_int = 1;

/// `SND_PCM_TSTAMP_TYPE_MONOTONIC`, the second variant of
/// `snd_pcm_tstamp_type_t`. Set explicitly because the first variant is
/// `gettimeofday`-based and `alsa.conf` leaves the default to the host
/// (`defaults.pcm.tstamp_type`).
const SND_PCM_TSTAMP_TYPE_MONOTONIC: c_int = 1;

/// `SND_PCM_STATE_RUNNING`, the fourth variant of `snd_pcm_state_t`.
const SND_PCM_STATE_RUNNING: c_int = 3;

/// `-EPIPE`. One value, two names: the device overran a capture stream, or
/// underran a playback one. Both mean audio is missing and the stream needs
/// putting back together.
const ALSA_BROKEN_PIPE: c_int = -32;

/// `-ESTRPIPE`: the stream was suspended (system sleep). `snd_pcm_recover`
/// handles it too.
const ALSA_SUSPENDED: c_int = -86;

/// Preferred rate, in either direction. ALSA hands back a range rather than
/// negotiating one the way PipeWire does, so the arm has to state a preference
/// — 48 kHz is what every modern device does natively and what the deviceless
/// clock defaults to. `_near` means the device's own answer is what the stream
/// reports.
const PREFERRED_SAMPLE_RATE: c_uint = 48_000;

/// Preferred capture channel count. Mono is what a capture endpoint is usually
/// asked for and what the null arm produces; `_near` lets a stereo-only device
/// say so.
const PREFERRED_CAPTURE_CHANNELS: c_uint = 1;

/// Preferred playback channel count. Stereo rather than the capture side's
/// mono because that is what a sink almost always is; `_near` lets a mono-only
/// device say so. The two preferences differ because the devices do, and there
/// is no resampler on this rung to reconcile them — a speaker refuses a block
/// whose channel count it cannot play, naming both.
const PREFERRED_PLAYBACK_CHANNELS: c_uint = 2;

/// Preferred period, ~10.7 ms at 48 kHz — a block small enough to be a useful
/// latency unit and large enough that a wake per period is not a busy loop.
const PREFERRED_PERIOD_SAMPLE_COUNT: SndPcmUframes = 512;

/// Periods held in the device buffer. Four is the usual floor for surviving a
/// scheduling hiccup without a break in the stream.
const PERIODS_PER_DEVICE_BUFFER: SndPcmUframes = 4;

/// How long a transfer thread waits for the device before looking at the stop
/// flag again. Long enough that it is not a poll loop, short enough that
/// stopping joins promptly.
const DEVICE_WAIT_TIMEOUT_MS: c_int = 200;

/// `snd_pcm_wait` returning this many times in a row without releasing the
/// device is a device that stopped rather than a slow one.
const CONSECUTIVE_SILENT_WAITS_BEFORE_GIVING_UP: u32 = 25;

/// How many waits the timestamp proof gets. One period answers it, so this is
/// only slack for a busy machine — and it is a separate budget because the
/// chain's probe runs it, where the reader loop's much longer patience would
/// stall graph construction behind a device that opens and never delivers.
const CONSECUTIVE_SILENT_WAITS_BEFORE_A_DEVICE_CANNOT_BE_TIMED: u32 = 3;

/// Declares the entry-point table and resolves it, from one list.
///
/// Name and signature sit together and appear once, so the table cannot drift
/// from what it calls: adding a symbol is one line here and nothing else.
macro_rules! alsa_library_entry_points {
    ($( fn $entry_point:ident( $($argument:ty),* $(,)? ) $(-> $returns:ty)? ; )*) => {
        /// The loaded library and one typed entry point per symbol this arm
        /// calls.
        struct AlsaLibraryEntryPoints {
            /// Kept solely to hold the library open: every pointer below points
            /// into it.
            _library: Library,
            $( $entry_point: unsafe extern "C" fn($($argument),*) $(-> $returns)?, )*
        }

        impl AlsaLibraryEntryPoints {
            /// The soname is a parameter so that both refusals — the library is
            /// not there, and the library is there but exports none of this —
            /// are reachable from a test without an ALSA-shaped stub on disk.
            fn resolve_from(
                library_soname: &str,
            ) -> std::result::Result<Self, AudioDeviceBackendArmUnavailableReason> {
                // SAFETY: `dlopen` of a soname. Loading an audio library can run
                // its initialisers, which is what the chain's probe-by-opening
                // accepts; nothing is dereferenced until `get` succeeds.
                let library = unsafe { Library::new(library_soname) }.map_err(|e| {
                    AudioDeviceBackendArmUnavailableReason::of(format!(
                        "{library_soname} could not be loaded: {e}"
                    ))
                })?;
                $(
                    // SAFETY: `dlsym` with a NUL-terminated name; the resulting
                    // address is kept alive by the `Library` stored beside it.
                    let $entry_point = unsafe {
                        library.get::<unsafe extern "C" fn($($argument),*) $(-> $returns)?>(
                            concat!(stringify!($entry_point), "\0").as_bytes(),
                        )
                    }
                    .map(|symbol| *symbol)
                    .map_err(|_| {
                        AudioDeviceBackendArmUnavailableReason::of(format!(
                            "{library_soname} exports no {}, so it is not the ALSA library \
                             this arm binds against",
                            stringify!($entry_point),
                        ))
                    })?;
                )*
                Ok(Self { _library: library, $($entry_point),* })
            }
        }
    };
}

alsa_library_entry_points! {
    fn snd_asoundlib_version() -> *const c_char;
    fn snd_strerror(c_int) -> *const c_char;

    fn snd_pcm_open(*mut *mut c_void, *const c_char, c_int, c_int) -> c_int;
    fn snd_pcm_close(*mut c_void) -> c_int;
    fn snd_pcm_prepare(*mut c_void) -> c_int;
    fn snd_pcm_start(*mut c_void) -> c_int;
    fn snd_pcm_drop(*mut c_void) -> c_int;
    fn snd_pcm_wait(*mut c_void, c_int) -> c_int;
    fn snd_pcm_readi(*mut c_void, *mut c_void, SndPcmUframes) -> SndPcmSframes;
    fn snd_pcm_writei(*mut c_void, *const c_void, SndPcmUframes) -> SndPcmSframes;
    fn snd_pcm_recover(*mut c_void, c_int, c_int) -> c_int;
    fn snd_pcm_state(*mut c_void) -> c_int;

    fn snd_pcm_hw_params_malloc(*mut *mut c_void) -> c_int;
    fn snd_pcm_hw_params_free(*mut c_void);
    fn snd_pcm_hw_params_any(*mut c_void, *mut c_void) -> c_int;
    fn snd_pcm_hw_params_set_access(*mut c_void, *mut c_void, c_int) -> c_int;
    fn snd_pcm_hw_params_set_format(*mut c_void, *mut c_void, c_int) -> c_int;
    fn snd_pcm_hw_params_set_channels_near(*mut c_void, *mut c_void, *mut c_uint) -> c_int;
    fn snd_pcm_hw_params_set_rate_near(*mut c_void, *mut c_void, *mut c_uint, *mut c_int) -> c_int;
    fn snd_pcm_hw_params_set_period_size_near(
        *mut c_void,
        *mut c_void,
        *mut SndPcmUframes,
        *mut c_int,
    ) -> c_int;
    fn snd_pcm_hw_params_set_buffer_size_near(
        *mut c_void,
        *mut c_void,
        *mut SndPcmUframes,
    ) -> c_int;
    fn snd_pcm_hw_params(*mut c_void, *mut c_void) -> c_int;
    fn snd_pcm_hw_params_get_channels(*const c_void, *mut c_uint) -> c_int;
    fn snd_pcm_hw_params_get_rate(*const c_void, *mut c_uint, *mut c_int) -> c_int;
    fn snd_pcm_hw_params_get_period_size(*const c_void, *mut SndPcmUframes, *mut c_int) -> c_int;

    fn snd_pcm_sw_params_malloc(*mut *mut c_void) -> c_int;
    fn snd_pcm_sw_params_free(*mut c_void);
    fn snd_pcm_sw_params_current(*mut c_void, *mut c_void) -> c_int;
    fn snd_pcm_sw_params_set_tstamp_mode(*mut c_void, *mut c_void, c_int) -> c_int;
    fn snd_pcm_sw_params_set_tstamp_type(*mut c_void, *mut c_void, c_int) -> c_int;
    fn snd_pcm_sw_params_set_avail_min(*mut c_void, *mut c_void, SndPcmUframes) -> c_int;
    fn snd_pcm_sw_params_get_boundary(*const c_void, *mut SndPcmUframes) -> c_int;
    fn snd_pcm_sw_params_set_start_threshold(*mut c_void, *mut c_void, SndPcmUframes) -> c_int;
    fn snd_pcm_sw_params(*mut c_void, *mut c_void) -> c_int;

    fn snd_pcm_status_malloc(*mut *mut c_void) -> c_int;
    fn snd_pcm_status_free(*mut c_void);
    fn snd_pcm_status(*mut c_void, *mut c_void) -> c_int;
    fn snd_pcm_status_get_htstamp(*const c_void, *mut libc::timespec);
    fn snd_pcm_status_get_delay(*const c_void) -> SndPcmSframes;
}

impl AlsaLibraryEntryPoints {
    fn resolve() -> std::result::Result<Self, AudioDeviceBackendArmUnavailableReason> {
        Self::resolve_from(ALSA_LIBRARY_SONAME)
    }

    /// libasound's own words for a negative return code.
    fn error_text(&self, error_code: c_int) -> String {
        // SAFETY: `snd_strerror` returns a pointer into libasound's own static
        // string table for every input, valid while the library is loaded.
        let text = unsafe { (self.snd_strerror)(error_code) };
        if text.is_null() {
            return format!("error {error_code}");
        }
        // SAFETY: non-null return values are NUL-terminated static strings.
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    }

    /// Turn a libasound return code into the core error a caller handles,
    /// carrying what was being attempted.
    ///
    /// `Arguments` rather than `&str` so the description formats only when
    /// there is a failure to describe: the reader thread checks a return code
    /// once per period, and it asked for realtime scheduling.
    fn refuse(&self, attempted: std::fmt::Arguments<'_>, error_code: c_int) -> Error {
        Error::Runtime(format!(
            "ALSA refused {attempted}: {}",
            self.error_text(error_code)
        ))
    }

    /// Every libasound entry point reports failure as a negative return code,
    /// so this is the one place that reading happens.
    fn refuse_a_negative_return_code(
        &self,
        attempted: std::fmt::Arguments<'_>,
        return_code: c_int,
    ) -> Result<()> {
        if return_code < 0 {
            return Err(self.refuse(attempted, return_code));
        }
        Ok(())
    }
}

/// A libasound-allocated object, freed by the entry point that allocated it.
///
/// The `_alloca` spellings in `<alsa/pcm.h>` are C macros over `alloca` and are
/// not reachable through `dlsym`; the `_malloc` / `_free` pair is the API's own
/// answer for callers that are not C.
struct AlsaAllocatedObject {
    /// Held so the library outlives the allocation and the `free` pointer
    /// below, which points into it.
    _entry_points: Arc<AlsaLibraryEntryPoints>,
    pointer: *mut c_void,
    free: unsafe extern "C" fn(*mut c_void),
}

impl AlsaAllocatedObject {
    fn hardware_parameters(entry_points: &Arc<AlsaLibraryEntryPoints>) -> Result<Self> {
        Self::allocated_by(
            entry_points,
            entry_points.snd_pcm_hw_params_malloc,
            entry_points.snd_pcm_hw_params_free,
            "hardware parameters",
        )
    }

    fn software_parameters(entry_points: &Arc<AlsaLibraryEntryPoints>) -> Result<Self> {
        Self::allocated_by(
            entry_points,
            entry_points.snd_pcm_sw_params_malloc,
            entry_points.snd_pcm_sw_params_free,
            "software parameters",
        )
    }

    fn device_status(entry_points: &Arc<AlsaLibraryEntryPoints>) -> Result<Self> {
        Self::allocated_by(
            entry_points,
            entry_points.snd_pcm_status_malloc,
            entry_points.snd_pcm_status_free,
            "a device status object",
        )
    }

    /// Private behind the three named constructors above, so an allocator can
    /// never be paired with another object's `free` — a mistake that would
    /// surface as heap corruption in `Drop` rather than as a compile error.
    fn allocated_by(
        entry_points: &Arc<AlsaLibraryEntryPoints>,
        allocate: unsafe extern "C" fn(*mut *mut c_void) -> c_int,
        free: unsafe extern "C" fn(*mut c_void),
        allocated_object_description: &str,
    ) -> Result<Self> {
        let mut pointer = std::ptr::null_mut();
        // SAFETY: the out-parameter is an owned local, and libasound writes a
        // pointer it allocated into it on success.
        let allocated = unsafe { allocate(&mut pointer) };
        if allocated < 0 || pointer.is_null() {
            return Err(entry_points.refuse(
                format_args!("allocating {allocated_object_description}"),
                allocated,
            ));
        }
        Ok(Self {
            _entry_points: Arc::clone(entry_points),
            pointer,
            free,
        })
    }

    fn pointer(&self) -> *mut c_void {
        self.pointer
    }
}

impl Drop for AlsaAllocatedObject {
    fn drop(&mut self) {
        // SAFETY: the pointer came from the paired allocator and is freed
        // exactly once, here; the `Arc` above keeps `free` itself mapped.
        unsafe { (self.free)(self.pointer) };
    }
}

// The pointer addresses a libasound heap allocation this struct exclusively
// owns — nothing else holds a copy, and only the thread holding the struct
// touches it — and the struct holds the library that both the allocation and
// its `free` live in, so moving it between threads cannot outlive either.
unsafe impl Send for AlsaAllocatedObject {}

/// An open PCM in either direction, closed exactly once when the last holder
/// drops.
struct OpenedAlsaPcm {
    entry_points: Arc<AlsaLibraryEntryPoints>,
    pcm: *mut c_void,
}

impl Drop for OpenedAlsaPcm {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `snd_pcm_open` and is closed once.
        unsafe { (self.entry_points.snd_pcm_close)(self.pcm) };
    }
}

// A PCM handle is not internally synchronised, so this is a claim about the
// callers rather than about libasound: a stream makes its own ALSA calls before
// it spawns its transfer thread and after it has joined it, and `Drop` stops
// the transfer before it drops this — so exactly one thread is ever inside
// libasound with this handle.
unsafe impl Send for OpenedAlsaPcm {}
unsafe impl Sync for OpenedAlsaPcm {}

/// Audio over ALSA, with libasound bound at runtime.
pub struct AlsaAudioDeviceBackend {
    entry_points: Arc<AlsaLibraryEntryPoints>,
}

impl AlsaAudioDeviceBackend {
    /// Load libasound and confirm a capture device opens *and can be timed*,
    /// or say why this arm cannot serve so the chain can demote.
    ///
    /// The round trip is the point, and it is the rule the PipeWire arm's
    /// connection check follows: `libasound` present with no `/dev/snd` behind
    /// it is the ordinary container case, and probing on presence alone would
    /// strand exactly the machines the chain exists to serve. Timing is part of
    /// opening rather than a later check, because a device whose stamps are not
    /// on the machine monotonic clock cannot serve this seam at all — and an
    /// arm that cannot serve demotes, exactly as a missing library does.
    pub fn load_and_open() -> std::result::Result<Self, AudioDeviceBackendArmUnavailableReason> {
        let entry_points = Arc::new(AlsaLibraryEntryPoints::resolve()?);
        let backend = Self { entry_points };

        // Opened, timed, and closed again: holding a device across the probe
        // would claim a card nothing is capturing from yet.
        let probe_outcome = backend
            .open_alsa_capture_stream(DEFAULT_PCM_NAME)
            .and_then(|mut probe| probe.prove_the_device_can_be_timed());
        if let Err(refusal) = probe_outcome {
            return Err(AudioDeviceBackendArmUnavailableReason::of(format!(
                "{ALSA_LIBRARY_SONAME} loaded but no capture device answered on \
                 '{DEFAULT_PCM_NAME}': {refusal}"
            )));
        }

        // SAFETY: returns a pointer to libasound's own static version string,
        // valid for as long as the library is loaded.
        let library_version = unsafe { (backend.entry_points.snd_asoundlib_version)() };
        let library_version = if library_version.is_null() {
            "unknown".to_string()
        } else {
            // SAFETY: non-null means a NUL-terminated static string.
            unsafe { CStr::from_ptr(library_version) }
                .to_string_lossy()
                .into_owned()
        };
        tracing::debug!(
            library = ALSA_LIBRARY_SONAME,
            version = %library_version,
            "ALSA audio arm: a capture device opened and stamped in the monotonic domain"
        );

        Ok(backend)
    }

    /// Open and negotiate one capture stream, before any delivery starts.
    fn open_alsa_capture_stream(&self, pcm_name: &str) -> Result<AlsaAudioCaptureStream> {
        let opened_pcm =
            OpenedAlsaPcm::open(&self.entry_points, pcm_name, AlsaStreamDirection::Capture)?;
        let negotiated = negotiate_hardware_parameters(
            &self.entry_points,
            opened_pcm.pcm,
            pcm_name,
            AlsaStreamDirection::Capture,
        )?;
        negotiate_capture_timestamp_contract(
            &self.entry_points,
            opened_pcm.pcm,
            SndPcmUframes::from(negotiated.period_sample_count),
            pcm_name,
        )?;

        let (failure_recorder, liveness_report) =
            AudioStreamFailureRecorder::recording_into_a_new_report();
        Ok(AlsaAudioCaptureStream {
            opened_pcm: Arc::new(opened_pcm),
            capture_stream_format: negotiated.stream_format,
            period_sample_count: negotiated.period_sample_count,
            device_name: pcm_name.to_string(),
            capture_is_running: false,
            failure_recorder,
            liveness_report,
            delivery: None,
        })
    }

    /// Open and negotiate one playback stream, before any sample is written.
    fn open_alsa_playback_stream(&self, pcm_name: &str) -> Result<AlsaAudioPlaybackStream> {
        let opened_pcm =
            OpenedAlsaPcm::open(&self.entry_points, pcm_name, AlsaStreamDirection::Playback)?;
        let negotiated = negotiate_hardware_parameters(
            &self.entry_points,
            opened_pcm.pcm,
            pcm_name,
            AlsaStreamDirection::Playback,
        )?;
        negotiate_playback_start_contract(
            &self.entry_points,
            opened_pcm.pcm,
            SndPcmUframes::from(negotiated.period_sample_count),
            pcm_name,
        )?;

        let (failure_recorder, liveness_report) =
            AudioStreamFailureRecorder::recording_into_a_new_report();
        Ok(AlsaAudioPlaybackStream {
            opened_pcm: Arc::new(opened_pcm),
            playback_stream_format: negotiated.stream_format,
            period_sample_count: negotiated.period_sample_count,
            device_name: pcm_name.to_string(),
            failure_recorder,
            liveness_report,
            playback: None,
        })
    }
}

impl OpenedAlsaPcm {
    fn open(
        entry_points: &Arc<AlsaLibraryEntryPoints>,
        pcm_name: &str,
        direction: AlsaStreamDirection,
    ) -> Result<Self> {
        let pcm_name_for_c = CString::new(pcm_name).map_err(|_| {
            Error::Configuration(format!(
                "audio device id '{pcm_name}' contains a NUL byte and cannot name an ALSA PCM"
            ))
        })?;
        let mut pcm = std::ptr::null_mut();
        // SAFETY: the out-parameter is an owned local and the name is a live
        // `CString` for the length of the call.
        let opened = unsafe {
            (entry_points.snd_pcm_open)(
                &mut pcm,
                pcm_name_for_c.as_ptr(),
                direction.snd_pcm_stream(),
                SND_PCM_OPEN_MODE_BLOCKING,
            )
        };
        if opened < 0 || pcm.is_null() {
            return Err(entry_points.refuse(
                format_args!("opening {} PCM '{pcm_name}'", direction.as_word()),
                opened,
            ));
        }
        Ok(Self {
            entry_points: Arc::clone(entry_points),
            pcm,
        })
    }
}

/// Which way samples travel on a PCM.
///
/// Carried rather than duplicated into two open paths: the direction decides
/// one libasound constant and one word of error text, and everything else
/// about opening a PCM is the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlsaStreamDirection {
    Capture,
    Playback,
}

impl AlsaStreamDirection {
    fn snd_pcm_stream(self) -> c_int {
        match self {
            AlsaStreamDirection::Capture => SND_PCM_STREAM_CAPTURE,
            AlsaStreamDirection::Playback => SND_PCM_STREAM_PLAYBACK,
        }
    }

    /// How the direction is spelled in error text a reader has to act on.
    fn as_word(self) -> &'static str {
        match self {
            AlsaStreamDirection::Capture => "capture",
            AlsaStreamDirection::Playback => "playback",
        }
    }

    /// The channel count this direction asks a device for.
    fn preferred_channels(self) -> c_uint {
        match self {
            AlsaStreamDirection::Capture => PREFERRED_CAPTURE_CHANNELS,
            AlsaStreamDirection::Playback => PREFERRED_PLAYBACK_CHANNELS,
        }
    }

    /// What a device in this direction stops doing when it stalls.
    ///
    /// The wait is the same call in both directions and a stalled one means
    /// the opposite thing either side of it: a capture device has released no
    /// samples, a playback device has taken none.
    fn what_a_stalled_device_stopped_doing(self) -> &'static str {
        match self {
            AlsaStreamDirection::Capture => "delivered",
            AlsaStreamDirection::Playback => "took",
        }
    }

    /// Where a reader can see the audio a break swallowed.
    ///
    /// The evidence differs by direction and the recovery is shared, so the
    /// sentence travels with the direction: a capture stream publishes stamped
    /// blocks whose gap is arithmetic, and a playback stream publishes nothing
    /// at all — what it has is the count of silence the device was handed
    /// instead.
    fn where_the_missing_audio_shows_up(self) -> &'static str {
        match self {
            AlsaStreamDirection::Capture => {
                "the gap is derivable from the timestamps of the blocks either side of it"
            }
            AlsaStreamDirection::Playback => {
                "the speaker counts the silence the device was given in its place"
            }
        }
    }
}

impl AudioDeviceBackend for AlsaAudioDeviceBackend {
    fn backend_name(&self) -> &'static str {
        "alsa"
    }

    fn open_capture_stream(
        &self,
        request: &AudioDeviceStreamRequest,
    ) -> Result<Box<dyn AudioCaptureStream>> {
        let pcm_name = request.device_id.as_deref().unwrap_or(DEFAULT_PCM_NAME);
        Ok(Box::new(self.open_alsa_capture_stream(pcm_name)?))
    }

    fn open_playback_stream(
        &self,
        request: &AudioDeviceStreamRequest,
    ) -> Result<Box<dyn AudioPlaybackStream>> {
        let pcm_name = request.device_id.as_deref().unwrap_or(DEFAULT_PCM_NAME);
        Ok(Box::new(self.open_alsa_playback_stream(pcm_name)?))
    }
}

/// What the hardware parameter pass settled on.
struct NegotiatedAlsaStream {
    stream_format: AudioStreamFormat,
    period_sample_count: u32,
}

/// Settle the hardware parameters both directions share.
///
/// The software pass is the caller's, because it is the half that differs: a
/// capture stream states the timestamp contract there and a playback stream
/// states when the device starts.
fn negotiate_hardware_parameters(
    entry_points: &Arc<AlsaLibraryEntryPoints>,
    pcm: *mut c_void,
    pcm_name: &str,
    direction: AlsaStreamDirection,
) -> Result<NegotiatedAlsaStream> {
    let hardware_parameters = AlsaAllocatedObject::hardware_parameters(entry_points)?;

    // SAFETY (every call in this function): `pcm` is an open capture handle and
    // the parameter object is a live libasound allocation; every out-parameter
    // is an owned local outliving its call.
    entry_points.refuse_a_negative_return_code(
        format_args!("reading the parameter space of '{pcm_name}'"),
        unsafe { (entry_points.snd_pcm_hw_params_any)(pcm, hardware_parameters.pointer()) },
    )?;
    entry_points.refuse_a_negative_return_code(
        format_args!("interleaved read/write access"),
        unsafe {
            (entry_points.snd_pcm_hw_params_set_access)(
                pcm,
                hardware_parameters.pointer(),
                SND_PCM_ACCESS_RW_INTERLEAVED,
            )
        },
    )?;

    let sample_format = negotiate_sample_format(entry_points, pcm, hardware_parameters.pointer())?;

    let mut channels = direction.preferred_channels();
    entry_points.refuse_a_negative_return_code(format_args!("a channel count"), unsafe {
        (entry_points.snd_pcm_hw_params_set_channels_near)(
            pcm,
            hardware_parameters.pointer(),
            &mut channels,
        )
    })?;

    let mut sample_rate = PREFERRED_SAMPLE_RATE;
    entry_points.refuse_a_negative_return_code(format_args!("a sample rate"), unsafe {
        (entry_points.snd_pcm_hw_params_set_rate_near)(
            pcm,
            hardware_parameters.pointer(),
            &mut sample_rate,
            std::ptr::null_mut(),
        )
    })?;

    let mut period_sample_count = PREFERRED_PERIOD_SAMPLE_COUNT;
    entry_points.refuse_a_negative_return_code(format_args!("a period size"), unsafe {
        (entry_points.snd_pcm_hw_params_set_period_size_near)(
            pcm,
            hardware_parameters.pointer(),
            &mut period_sample_count,
            std::ptr::null_mut(),
        )
    })?;

    let mut device_buffer_sample_count =
        period_sample_count.saturating_mul(PERIODS_PER_DEVICE_BUFFER);
    entry_points.refuse_a_negative_return_code(format_args!("a device buffer size"), unsafe {
        (entry_points.snd_pcm_hw_params_set_buffer_size_near)(
            pcm,
            hardware_parameters.pointer(),
            &mut device_buffer_sample_count,
        )
    })?;

    entry_points.refuse_a_negative_return_code(
        format_args!("the negotiated hardware parameters for '{pcm_name}'"),
        unsafe { (entry_points.snd_pcm_hw_params)(pcm, hardware_parameters.pointer()) },
    )?;

    // Read back rather than trusting the requests: every `_near` setter is free
    // to land somewhere else, and what the stream reports has to be what the
    // device is actually doing.
    let mut settled_channels = 0;
    entry_points.refuse_a_negative_return_code(
        format_args!("reading back the channel count"),
        unsafe {
            (entry_points.snd_pcm_hw_params_get_channels)(
                hardware_parameters.pointer(),
                &mut settled_channels,
            )
        },
    )?;
    let mut settled_sample_rate = 0;
    entry_points.refuse_a_negative_return_code(
        format_args!("reading back the sample rate"),
        unsafe {
            (entry_points.snd_pcm_hw_params_get_rate)(
                hardware_parameters.pointer(),
                &mut settled_sample_rate,
                std::ptr::null_mut(),
            )
        },
    )?;
    let mut settled_period_sample_count = 0;
    entry_points.refuse_a_negative_return_code(
        format_args!("reading back the period size"),
        unsafe {
            (entry_points.snd_pcm_hw_params_get_period_size)(
                hardware_parameters.pointer(),
                &mut settled_period_sample_count,
                std::ptr::null_mut(),
            )
        },
    )?;
    if settled_sample_rate == 0 || settled_channels == 0 || settled_period_sample_count == 0 {
        return Err(Error::Runtime(format!(
            "ALSA settled '{pcm_name}' on {settled_sample_rate} Hz, {settled_channels} \
             channels and a {settled_period_sample_count}-sample period — no block duration \
             is derivable from that"
        )));
    }

    Ok(NegotiatedAlsaStream {
        stream_format: AudioStreamFormat {
            sample_rate: settled_sample_rate,
            channels: settled_channels,
            sample_format,
        },
        period_sample_count: u32::try_from(settled_period_sample_count).map_err(|_| {
            Error::Runtime(format!(
                "ALSA settled '{pcm_name}' on a period of {settled_period_sample_count} \
                 samples, which no block can carry"
            ))
        })?,
    })
}

/// Take float samples if the device offers them, 16-bit integers otherwise —
/// the two encodings `AudioBlock`'s `dtype` names.
fn negotiate_sample_format(
    entry_points: &AlsaLibraryEntryPoints,
    pcm: *mut c_void,
    hardware_parameters: *mut c_void,
) -> Result<AudioSampleFormat> {
    for (alsa_format, sample_format) in [
        (SND_PCM_FORMAT_FLOAT_LE, AudioSampleFormat::F32),
        (SND_PCM_FORMAT_S16_LE, AudioSampleFormat::I16),
    ] {
        // SAFETY: `pcm` is an open capture handle and `hardware_parameters` a
        // live libasound allocation.
        let format_set = unsafe {
            (entry_points.snd_pcm_hw_params_set_format)(pcm, hardware_parameters, alsa_format)
        };
        if format_set >= 0 {
            return Ok(sample_format);
        }
    }
    Err(Error::NotSupported(
        "this ALSA device offers neither little-endian float nor little-endian 16-bit \
         integer samples, and those are the two encodings an AudioBlock's dtype names"
            .into(),
    ))
}

/// State the timestamp contract on a capture stream before it runs.
///
/// This is the whole reason the arm can be trusted for A/V join: without the
/// tstamp *mode* `snd_pcm_status` reports no time at all, and without the
/// tstamp *type* it reports one on the wrong clock.
fn negotiate_capture_timestamp_contract(
    entry_points: &Arc<AlsaLibraryEntryPoints>,
    pcm: *mut c_void,
    period_sample_count: SndPcmUframes,
    pcm_name: &str,
) -> Result<()> {
    let software_parameters = AlsaAllocatedObject::software_parameters(entry_points)?;

    // SAFETY (every call here): `pcm` is an open capture handle whose hardware
    // parameters are applied, and the parameter object is a live libasound
    // allocation; every out-parameter is an owned local outliving its call.
    entry_points.refuse_a_negative_return_code(
        format_args!("reading the software parameters of '{pcm_name}'"),
        unsafe { (entry_points.snd_pcm_sw_params_current)(pcm, software_parameters.pointer()) },
    )?;
    entry_points.refuse_a_negative_return_code(
        format_args!("timestamping on the capture stream"),
        unsafe {
            (entry_points.snd_pcm_sw_params_set_tstamp_mode)(
                pcm,
                software_parameters.pointer(),
                SND_PCM_TSTAMP_ENABLE,
            )
        },
    )?;
    entry_points.refuse_a_negative_return_code(
        format_args!("monotonic timestamps on the capture stream"),
        unsafe {
            (entry_points.snd_pcm_sw_params_set_tstamp_type)(
                pcm,
                software_parameters.pointer(),
                SND_PCM_TSTAMP_TYPE_MONOTONIC,
            )
        },
    )?;

    // Wake the reader once a whole period is readable, which is what makes
    // "status minus reported delay" name the first sample of the block about to
    // be read rather than a sample still being captured.
    entry_points.refuse_a_negative_return_code(
        format_args!("a one-period wake threshold"),
        unsafe {
            (entry_points.snd_pcm_sw_params_set_avail_min)(
                pcm,
                software_parameters.pointer(),
                period_sample_count,
            )
        },
    )?;

    // The stream's `boundary` is ALSA's own "unreachable frame count", and a
    // start threshold of it is the API's way to spell "never start implicitly".
    // A capture stream starts when `snd_pcm_start` says so, so that the
    // monotonic bracket taken around the start is a real bracket.
    let mut never_start_implicitly = 0;
    entry_points.refuse_a_negative_return_code(
        format_args!("reading the stream's frame boundary"),
        unsafe {
            (entry_points.snd_pcm_sw_params_get_boundary)(
                software_parameters.pointer(),
                &mut never_start_implicitly,
            )
        },
    )?;
    entry_points.refuse_a_negative_return_code(
        format_args!("an explicit start threshold"),
        unsafe {
            (entry_points.snd_pcm_sw_params_set_start_threshold)(
                pcm,
                software_parameters.pointer(),
                never_start_implicitly,
            )
        },
    )?;

    entry_points.refuse_a_negative_return_code(
        format_args!("the timestamp contract on '{pcm_name}'"),
        unsafe { (entry_points.snd_pcm_sw_params)(pcm, software_parameters.pointer()) },
    )
}

/// State when a playback stream starts, before a sample is written.
///
/// The start threshold is the whole of it: at one period the device begins
/// playing as soon as the first period is queued, which is why the writer
/// thread never calls `snd_pcm_start` — and why recovery must not either,
/// since starting a prepared stream whose buffer is empty underruns it on the
/// spot.
fn negotiate_playback_start_contract(
    entry_points: &Arc<AlsaLibraryEntryPoints>,
    pcm: *mut c_void,
    period_sample_count: SndPcmUframes,
    pcm_name: &str,
) -> Result<()> {
    let software_parameters = AlsaAllocatedObject::software_parameters(entry_points)?;

    // SAFETY (every call here): `pcm` is an open playback handle whose hardware
    // parameters are applied, and the parameter object is a live libasound
    // allocation; every out-parameter is an owned local outliving its call.
    entry_points.refuse_a_negative_return_code(
        format_args!("reading the software parameters of '{pcm_name}'"),
        unsafe { (entry_points.snd_pcm_sw_params_current)(pcm, software_parameters.pointer()) },
    )?;

    // Wake the writer once a whole period of room is free, so it assembles one
    // period per wake rather than dribbling frames in.
    entry_points.refuse_a_negative_return_code(
        format_args!("a one-period wake threshold"),
        unsafe {
            (entry_points.snd_pcm_sw_params_set_avail_min)(
                pcm,
                software_parameters.pointer(),
                period_sample_count,
            )
        },
    )?;

    entry_points.refuse_a_negative_return_code(
        format_args!("a one-period start threshold"),
        unsafe {
            (entry_points.snd_pcm_sw_params_set_start_threshold)(
                pcm,
                software_parameters.pointer(),
                period_sample_count,
            )
        },
    )?;

    entry_points.refuse_a_negative_return_code(
        format_args!("the playback start contract on '{pcm_name}'"),
        unsafe { (entry_points.snd_pcm_sw_params)(pcm, software_parameters.pointer()) },
    )
}

/// The writer thread and the flag that stops it.
struct PlaybackWriterThread {
    stop_requested: Arc<AtomicBool>,
    writer_thread: JoinHandle<()>,
}

/// One ALSA playback stream, negotiated and ready to run.
struct AlsaAudioPlaybackStream {
    opened_pcm: Arc<OpenedAlsaPcm>,
    playback_stream_format: AudioStreamFormat,
    period_sample_count: u32,
    /// What the caller named, or `default` — for error text a reader can act on.
    device_name: String,
    /// The write and read halves, minted with the stream for the reason its
    /// capture sibling states.
    failure_recorder: AudioStreamFailureRecorder,
    liveness_report: AudioStreamLivenessReport,
    playback: Option<PlaybackWriterThread>,
}

impl AudioPlaybackStream for AlsaAudioPlaybackStream {
    fn stream_format(&self) -> AudioStreamFormat {
        self.playback_stream_format
    }

    fn liveness_report(&self) -> AudioStreamLivenessReport {
        self.liveness_report.clone()
    }

    fn start_requesting_from(&mut self, hand_off: AudioBlockForPlaybackHandOff) -> Result<()> {
        self.stop_requesting()?;

        let entry_points = &self.opened_pcm.entry_points;
        // SAFETY: any writer thread has been joined, so this is the only thread
        // holding the handle.
        entry_points.refuse_a_negative_return_code(
            format_args!("preparing playback on '{}'", self.device_name),
            unsafe { (entry_points.snd_pcm_prepare)(self.opened_pcm.pcm) },
        )?;

        let stop_requested = Arc::new(AtomicBool::new(false));
        let writer_thread = spawn_playback_writer_thread(PlaybackWriterThreadInputs {
            opened_pcm: Arc::clone(&self.opened_pcm),
            playback_stream_format: self.playback_stream_format,
            period_sample_count: self.period_sample_count,
            device_name: self.device_name.clone(),
            stop_requested: Arc::clone(&stop_requested),
            failure_recorder: self.failure_recorder.clone(),
            hand_off,
        })?;
        self.playback = Some(PlaybackWriterThread {
            stop_requested,
            writer_thread,
        });
        Ok(())
    }

    fn stop_requesting(&mut self) -> Result<()> {
        let Some(playback) = self.playback.take() else {
            return Ok(());
        };
        playback.stop_requested.store(true, Ordering::Release);
        // Joining is what the trait's "the hand-off is not called again once
        // this returns" means here: the writer owns the hand-off and drops it
        // as it ends, so nothing can still be inside it afterwards.
        let writer_panicked = playback.writer_thread.join().is_err();

        // Dropped rather than drained: a stop is a stop, and draining would
        // hold the runtime's shutdown chain for whatever the device still has
        // buffered. The device is wound back before the panic is reported, so
        // a writer that died does not also leave a running PCM for the next
        // start to trip over.
        let entry_points = &self.opened_pcm.entry_points;
        // SAFETY: the writer thread has been joined, so this is the only thread
        // holding the handle.
        let stopped = entry_points.refuse_a_negative_return_code(
            format_args!("stopping playback on '{}'", self.device_name),
            unsafe { (entry_points.snd_pcm_drop)(self.opened_pcm.pcm) },
        );
        if writer_panicked {
            if let Err(refusal) = stopped {
                tracing::warn!(device = %self.device_name, %refusal, "ALSA playback did not stop");
            }
            return Err(Error::Runtime(format!(
                "the ALSA playback writer for '{}' panicked",
                self.device_name
            )));
        }
        stopped
    }
}

impl Drop for AlsaAudioPlaybackStream {
    fn drop(&mut self) {
        if let Err(refusal) = self.stop_requesting() {
            tracing::warn!(
                device = %self.device_name,
                %refusal,
                "ALSA playback stream did not stop cleanly on drop"
            );
        }
    }
}

/// Why an ALSA device thread stopped running its stream.
///
/// Returned from the transfer loop rather than logged where it happens, so the
/// one judgement that matters — whether the stream failed or was told to stop
/// — is made in a single place for both directions, and can be held by a test
/// on a machine with no `libasound` at all.
enum AlsaDeviceThreadExit {
    /// `stop_delivering` / `stop_requesting` set the flag.
    StopThatWasAskedFor,
    /// The device released no period for long enough that it has stopped
    /// rather than slowed.
    DeviceWentQuiet { consecutive_silent_waits: u32 },
    /// libasound refused while the stream was being driven.
    DeviceRefused(Error),
    /// A transfer broke and `snd_pcm_recover` could not put it back together.
    StreamCouldNotBeRecovered,
}

impl AlsaDeviceThreadExit {
    /// Why the stream stopped serving its device, or `None` when it stopped
    /// because its owner said so.
    ///
    /// The deliberate stop is the case the whole seam turns on: an owner that
    /// saw its own stop reported back as a failure would have no way to tell a
    /// dead microphone from one it switched off.
    fn failure_that_ended_the_stream(
        &self,
        direction: AlsaStreamDirection,
    ) -> Option<AudioStreamFailureReason> {
        let direction_word = direction.as_word();
        match self {
            AlsaDeviceThreadExit::StopThatWasAskedFor => None,
            AlsaDeviceThreadExit::DeviceWentQuiet {
                consecutive_silent_waits,
            } => Some(AudioStreamFailureReason::of(format!(
                "the ALSA {direction_word} device {} nothing for {consecutive_silent_waits} \
                 consecutive waits",
                direction.what_a_stalled_device_stopped_doing(),
            ))),
            AlsaDeviceThreadExit::DeviceRefused(refusal) => Some(AudioStreamFailureReason::of(
                format!("the ALSA {direction_word} device refused while it was running: {refusal}"),
            )),
            AlsaDeviceThreadExit::StreamCouldNotBeRecovered => Some(AudioStreamFailureReason::of(
                format!("the ALSA {direction_word} stream broke and could not be recovered"),
            )),
        }
    }
}

/// Log why a device thread stopped and record it where the stream's owner can
/// read it.
///
/// Both, rather than either: the log is for whoever is watching the run, and
/// the report is for the code that has to act — an owner that can only read
/// logs cannot retry, fail, or say anything to its own consumers.
fn record_an_alsa_device_thread_exit(
    exit: &AlsaDeviceThreadExit,
    direction: AlsaStreamDirection,
    device_name: &str,
    failure_recorder: &AudioStreamFailureRecorder,
) {
    let Some(reason) = exit.failure_that_ended_the_stream(direction) else {
        return;
    };
    tracing::error!(
        device = %device_name,
        %reason,
        "ALSA {} stopping",
        direction.as_word()
    );
    failure_recorder.record_the_failure_that_ended_the_stream(reason);
}

/// Everything the writer thread owns, handed over in one move.
struct PlaybackWriterThreadInputs {
    opened_pcm: Arc<OpenedAlsaPcm>,
    playback_stream_format: AudioStreamFormat,
    period_sample_count: u32,
    device_name: String,
    stop_requested: Arc<AtomicBool>,
    failure_recorder: AudioStreamFailureRecorder,
    hand_off: AudioBlockForPlaybackHandOff,
}

fn spawn_playback_writer_thread(inputs: PlaybackWriterThreadInputs) -> Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(format!("streamlib-alsa-playback-{}", inputs.device_name))
        .spawn(move || run_playback_writer_thread(inputs))
        .map_err(|e| {
            Error::Runtime(format!(
                "the ALSA playback writer thread could not start: {e}"
            ))
        })
}

fn run_playback_writer_thread(inputs: PlaybackWriterThreadInputs) {
    let PlaybackWriterThreadInputs {
        opened_pcm,
        playback_stream_format,
        period_sample_count,
        device_name,
        stop_requested,
        failure_recorder,
        hand_off,
    } = inputs;
    let entry_points = &opened_pcm.entry_points;

    // A PipeWire client is granted realtime by the daemon; an ALSA client has
    // to ask. Failure is logged and the thread continues on `SCHED_OTHER`,
    // which is what a container without rtkit gets.
    if let Err(refusal) = apply_thread_priority(ThreadPriority::RealTime) {
        tracing::debug!(
            device = %device_name,
            %refusal,
            "ALSA playback writer stays best-effort: realtime scheduling was refused"
        );
    }

    let mut one_period_interleaved_sample_bytes =
        vec![0u8; playback_stream_format.interleaved_byte_count_for(period_sample_count)];
    let mut consecutive_silent_waits = 0;

    // Labelled because the write loop below it has to leave both: a stream
    // that recovery could not put back together has nothing left to write into.
    // Named for what this loop iterates, so the `break` inside that inner loop
    // reads unambiguously from where it sits.
    let exit = 'serving_device_periods: loop {
        if stop_requested.load(Ordering::Acquire) {
            break AlsaDeviceThreadExit::StopThatWasAskedFor;
        }
        // A prepared playback stream has its whole buffer free, so the first
        // wait returns at once and the device starts on the first write the
        // start threshold sees.
        match wait_for_the_device_to_release_a_period(
            entry_points,
            opened_pcm.pcm,
            &device_name,
            AlsaStreamDirection::Playback,
        ) {
            Ok(DevicePeriodReadiness::Ready) => consecutive_silent_waits = 0,
            Ok(DevicePeriodReadiness::NothingYet) => {
                consecutive_silent_waits += 1;
                if consecutive_silent_waits >= CONSECUTIVE_SILENT_WAITS_BEFORE_GIVING_UP {
                    break AlsaDeviceThreadExit::DeviceWentQuiet {
                        consecutive_silent_waits,
                    };
                }
                continue;
            }
            Err(refusal) => break AlsaDeviceThreadExit::DeviceRefused(refusal),
        }

        hand_off(AudioBlockRequestedByDevice {
            interleaved_sample_bytes_to_fill: &mut one_period_interleaved_sample_bytes,
            sample_count: period_sample_count,
        });

        // Looped rather than written once. `snd_pcm_writei` may take fewer
        // frames than it was offered even in blocking mode — a signal, or an
        // underrun mid-write — and the samples it did not take are already out
        // of the ring. Writing the remainder is what keeps this period from
        // vanishing uncounted, which is the one thing this rung promises about
        // a sample.
        let mut frames_taken_by_the_device = 0u32;
        while frames_taken_by_the_device < period_sample_count {
            let offset_bytes =
                playback_stream_format.interleaved_byte_count_for(frames_taken_by_the_device);
            // SAFETY: the buffer holds exactly a period at the negotiated
            // format and `offset_bytes` is inside it, so the pointer and the
            // remaining frame count describe the same range; `pcm` is open and
            // prepared, and only this thread touches it.
            let written = unsafe {
                (entry_points.snd_pcm_writei)(
                    opened_pcm.pcm,
                    one_period_interleaved_sample_bytes[offset_bytes..]
                        .as_ptr()
                        .cast::<c_void>(),
                    SndPcmUframes::from(period_sample_count - frames_taken_by_the_device),
                )
            };
            if written < 0 {
                let error_code = c_int::try_from(written).unwrap_or(c_int::MIN);
                if recover_from_a_transfer_failure(
                    entry_points,
                    opened_pcm.pcm,
                    &device_name,
                    error_code,
                    AlsaStreamDirection::Playback,
                ) == AlsaTransferRecovery::StreamIsUnusable
                {
                    break 'serving_device_periods AlsaDeviceThreadExit::StreamCouldNotBeRecovered;
                }
                // Recovery reset the stream, so the rest of this period has
                // nowhere to go — it is part of the gap the recovery already
                // reported, not a second silent loss.
                break;
            }
            let Ok(written) = u32::try_from(written) else {
                break;
            };
            if written == 0 {
                // A device taking nothing while reporting success would spin
                // this loop forever; the next wait is where it gets judged.
                break;
            }
            frames_taken_by_the_device += written;
        }
    };

    record_an_alsa_device_thread_exit(
        &exit,
        AlsaStreamDirection::Playback,
        &device_name,
        &failure_recorder,
    );
}

/// The reader thread and the flag that stops it.
struct CaptureDeliveryThread {
    stop_requested: Arc<AtomicBool>,
    reader_thread: JoinHandle<()>,
}

/// One ALSA capture stream, negotiated and ready to run.
struct AlsaAudioCaptureStream {
    opened_pcm: Arc<OpenedAlsaPcm>,
    capture_stream_format: AudioStreamFormat,
    period_sample_count: u32,
    /// What the caller named, or `default` — for error text a reader can act on.
    device_name: String,
    /// Tracked rather than inferred from `delivery`: the PCM is started before
    /// the reader thread exists and can outlive an attempt that failed in
    /// between, and a PCM left running makes the next `snd_pcm_prepare` return
    /// `EBUSY` — a refusal whose text sends a reader hunting for another
    /// process holding the card.
    capture_is_running: bool,
    /// The write half, handed to each reader thread. Minted with the stream
    /// rather than with a delivery, so the reason a reader died outlives the
    /// reader.
    failure_recorder: AudioStreamFailureRecorder,
    /// The read half, cloned to whoever owns the stream.
    liveness_report: AudioStreamLivenessReport,
    delivery: Option<CaptureDeliveryThread>,
}

impl AudioCaptureStream for AlsaAudioCaptureStream {
    fn stream_format(&self) -> AudioStreamFormat {
        self.capture_stream_format
    }

    fn liveness_report(&self) -> AudioStreamLivenessReport {
        self.liveness_report.clone()
    }

    fn start_delivering_to(&mut self, hand_off: CapturedAudioBlockHandOff) -> Result<()> {
        self.stop_delivering()?;

        let device_status = AlsaAllocatedObject::device_status(&self.opened_pcm.entry_points)?;
        let started_at_ns = self.start_capture()?;
        self.refuse_a_device_that_cannot_be_timed(&device_status, started_at_ns)?;

        let stop_requested = Arc::new(AtomicBool::new(false));
        let reader_thread = spawn_capture_reader_thread(CaptureReaderThreadInputs {
            opened_pcm: Arc::clone(&self.opened_pcm),
            device_status,
            capture_stream_format: self.capture_stream_format,
            period_sample_count: self.period_sample_count,
            device_name: self.device_name.clone(),
            stop_requested: Arc::clone(&stop_requested),
            failure_recorder: self.failure_recorder.clone(),
            hand_off,
        })?;
        self.delivery = Some(CaptureDeliveryThread {
            stop_requested,
            reader_thread,
        });
        Ok(())
    }

    fn stop_delivering(&mut self) -> Result<()> {
        let mut reader_panicked = false;
        if let Some(delivery) = self.delivery.take() {
            delivery.stop_requested.store(true, Ordering::Release);
            // Joining is what the trait's "the hand-off is not called again
            // once this returns" means here: the reader owns the hand-off and
            // drops it as it ends, so nothing can still be inside it
            // afterwards.
            reader_panicked = delivery.reader_thread.join().is_err();
        }
        // The device is wound back before the panic is reported, so a reader
        // that died does not also leave a running PCM for the next start to
        // trip over.
        let stopped = self.stop_capture();
        if reader_panicked {
            if let Err(refusal) = stopped {
                tracing::warn!(device = %self.device_name, %refusal, "ALSA capture did not stop");
            }
            return Err(Error::Runtime(format!(
                "the ALSA capture reader for '{}' panicked",
                self.device_name
            )));
        }
        stopped
    }
}

impl AlsaAudioCaptureStream {
    /// Start the device, and take the monotonic instant it started at.
    fn start_capture(&mut self) -> Result<i64> {
        // Recorded before the call that can fail: `snd_pcm_start` returning an
        // error does not prove the stream stayed stopped, and dropping a
        // stopped PCM is harmless where leaving a started one is not.
        self.capture_is_running = true;
        prepare_and_start_capture(
            &self.opened_pcm.entry_points,
            self.opened_pcm.pcm,
            &self.device_name,
        )
    }

    fn stop_capture(&mut self) -> Result<()> {
        if !self.capture_is_running {
            return Ok(());
        }
        self.capture_is_running = false;
        let entry_points = &self.opened_pcm.entry_points;
        // SAFETY: any reader thread has been joined, so this is the only thread
        // holding the handle.
        entry_points.refuse_a_negative_return_code(
            format_args!("stopping capture on '{}'", self.device_name),
            unsafe { (entry_points.snd_pcm_drop)(self.opened_pcm.pcm) },
        )
    }

    /// Run the device long enough to read one status off it, and refuse it if
    /// its own stamp is not on the machine monotonic clock.
    ///
    /// Used by the chain's probe as well as by every stream open, so a device
    /// that cannot be timed demotes the arm rather than failing a graph.
    fn prove_the_device_can_be_timed(&mut self) -> Result<()> {
        let device_status = AlsaAllocatedObject::device_status(&self.opened_pcm.entry_points)?;
        let started_at_ns = self.start_capture()?;
        let proof = self.refuse_a_device_that_cannot_be_timed(&device_status, started_at_ns);
        // The device stays open for whatever the caller does next; only the
        // running stream is wound back.
        self.stop_capture().and(proof)
    }

    /// Read one status off the just-started stream and judge the epoch it
    /// stamped in.
    fn refuse_a_device_that_cannot_be_timed(
        &self,
        device_status: &AlsaAllocatedObject,
        started_at_ns: i64,
    ) -> Result<()> {
        let entry_points = &self.opened_pcm.entry_points;
        for _ in 0..CONSECUTIVE_SILENT_WAITS_BEFORE_A_DEVICE_CANNOT_BE_TIMED {
            if read_status_of_a_readable_period(
                entry_points,
                self.opened_pcm.pcm,
                device_status,
                &self.device_name,
            )? == DevicePeriodReadiness::NothingYet
            {
                continue;
            }
            // SAFETY: the status was just filled by `snd_pcm_status`.
            let device_stamp_ns = unsafe { read_htimestamp_ns(entry_points, device_status) };
            return refuse_a_stamp_outside_the_monotonic_domain(
                &self.device_name,
                device_stamp_ns,
                started_at_ns,
                monotonic_now_ns(),
            );
        }
        Err(Error::Runtime(format!(
            "ALSA device '{}' started but delivered no period to time itself by",
            self.device_name
        )))
    }
}

impl Drop for AlsaAudioCaptureStream {
    fn drop(&mut self) {
        if let Err(refusal) = self.stop_delivering() {
            tracing::warn!(
                device = %self.device_name,
                %refusal,
                "ALSA capture stream did not stop cleanly on drop"
            );
        }
    }
}

/// Bring a prepared-or-broken PCM back to running, and take the monotonic
/// instant it started at.
///
/// One function because the stream comes to life in two places — `start_capture`
/// and overrun recovery — and the pair is not separable: `snd_pcm_recover`
/// leaves the stream *prepared*, and this arm disables ALSA's implicit start,
/// so a recovery that did not start it again would leave the reader waiting on
/// a stream that never runs.
fn prepare_and_start_capture(
    entry_points: &AlsaLibraryEntryPoints,
    pcm: *mut c_void,
    device_name: &str,
) -> Result<i64> {
    // SAFETY: `pcm` is an open, fully negotiated capture handle, held by
    // whichever single thread is driving it at this point.
    entry_points.refuse_a_negative_return_code(
        format_args!("preparing capture on '{device_name}'"),
        unsafe { (entry_points.snd_pcm_prepare)(pcm) },
    )?;

    // Nothing can have been captured before this, which is what makes the
    // bracket the timestamp check reads against a real bracket.
    let started_at_ns = monotonic_now_ns();
    // SAFETY: as above.
    entry_points.refuse_a_negative_return_code(
        format_args!("starting capture on '{device_name}'"),
        unsafe { (entry_points.snd_pcm_start)(pcm) },
    )?;
    Ok(started_at_ns)
}

/// Everything the reader thread owns, handed over in one move.
struct CaptureReaderThreadInputs {
    opened_pcm: Arc<OpenedAlsaPcm>,
    device_status: AlsaAllocatedObject,
    capture_stream_format: AudioStreamFormat,
    period_sample_count: u32,
    device_name: String,
    stop_requested: Arc<AtomicBool>,
    failure_recorder: AudioStreamFailureRecorder,
    hand_off: CapturedAudioBlockHandOff,
}

fn spawn_capture_reader_thread(inputs: CaptureReaderThreadInputs) -> Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(format!("streamlib-alsa-capture-{}", inputs.device_name))
        .spawn(move || run_capture_reader_thread(inputs))
        .map_err(|e| {
            Error::Runtime(format!(
                "the ALSA capture reader thread could not start: {e}"
            ))
        })
}

fn run_capture_reader_thread(inputs: CaptureReaderThreadInputs) {
    let CaptureReaderThreadInputs {
        opened_pcm,
        device_status,
        capture_stream_format,
        period_sample_count,
        device_name,
        stop_requested,
        failure_recorder,
        hand_off,
    } = inputs;
    let entry_points = &opened_pcm.entry_points;

    // A PipeWire client is granted realtime by the daemon; an ALSA client has
    // to ask. Failure is logged and the thread continues on `SCHED_OTHER`,
    // which is what a container without rtkit gets.
    if let Err(refusal) = apply_thread_priority(ThreadPriority::RealTime) {
        tracing::debug!(
            device = %device_name,
            %refusal,
            "ALSA capture reader stays best-effort: realtime scheduling was refused"
        );
    }

    let mut one_period_interleaved_sample_bytes =
        vec![0u8; capture_stream_format.interleaved_byte_count_for(period_sample_count)];
    let mut consecutive_silent_waits = 0;

    let exit = loop {
        if stop_requested.load(Ordering::Acquire) {
            break AlsaDeviceThreadExit::StopThatWasAskedFor;
        }
        match read_status_of_a_readable_period(
            entry_points,
            opened_pcm.pcm,
            &device_status,
            &device_name,
        ) {
            Ok(DevicePeriodReadiness::Ready) => consecutive_silent_waits = 0,
            Ok(DevicePeriodReadiness::NothingYet) => {
                consecutive_silent_waits += 1;
                if consecutive_silent_waits >= CONSECUTIVE_SILENT_WAITS_BEFORE_GIVING_UP {
                    break AlsaDeviceThreadExit::DeviceWentQuiet {
                        consecutive_silent_waits,
                    };
                }
                continue;
            }
            Err(refusal) => break AlsaDeviceThreadExit::DeviceRefused(refusal),
        }

        // SAFETY: the status was just filled, and the out-parameter is an
        // owned local.
        let device_stamp_ns = unsafe { read_htimestamp_ns(entry_points, &device_status) };
        // SAFETY: the status was just filled by `snd_pcm_status`.
        let unread_sample_count =
            unsafe { (entry_points.snd_pcm_status_get_delay)(device_status.pointer()) };
        let first_sample_timestamp_ns = first_sample_timestamp_ns(
            device_stamp_ns,
            unread_sample_count,
            capture_stream_format.sample_rate,
        );

        // SAFETY: the buffer holds room for a whole period at the negotiated
        // format, and `pcm` is open and started; only this thread touches it.
        let read = unsafe {
            (entry_points.snd_pcm_readi)(
                opened_pcm.pcm,
                one_period_interleaved_sample_bytes
                    .as_mut_ptr()
                    .cast::<c_void>(),
                SndPcmUframes::from(period_sample_count),
            )
        };
        if read < 0 {
            let error_code = c_int::try_from(read).unwrap_or(c_int::MIN);
            if recover_from_a_transfer_failure(
                entry_points,
                opened_pcm.pcm,
                &device_name,
                error_code,
                AlsaStreamDirection::Capture,
            ) == AlsaTransferRecovery::StreamIsUnusable
            {
                break AlsaDeviceThreadExit::StreamCouldNotBeRecovered;
            }
            continue;
        }
        let Ok(sample_count) = u32::try_from(read) else {
            continue;
        };
        if sample_count == 0 {
            continue;
        }

        hand_off(CapturedAudioBlockFromDevice {
            interleaved_sample_bytes: &one_period_interleaved_sample_bytes
                [..capture_stream_format.interleaved_byte_count_for(sample_count)],
            sample_count,
            first_sample_timestamp_ns,
        });
    };

    record_an_alsa_device_thread_exit(
        &exit,
        AlsaStreamDirection::Capture,
        &device_name,
        &failure_recorder,
    );
}

/// Whether a wait produced a whole period the device is ready to transfer —
/// samples to read on a capture stream, room to write on a playback one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DevicePeriodReadiness {
    Ready,
    NothingYet,
}

/// Wait for the device to release a period in whichever direction it runs.
///
/// The wait is the cadence on this arm — `snd_pcm_wait` returns when the device
/// says so, not when a timer does — which is what makes an ALSA stream
/// device-paced despite the arm owning the thread.
fn wait_for_the_device_to_release_a_period(
    entry_points: &AlsaLibraryEntryPoints,
    pcm: *mut c_void,
    device_name: &str,
    direction: AlsaStreamDirection,
) -> Result<DevicePeriodReadiness> {
    // SAFETY: `pcm` is an open, started handle held by this thread alone;
    // `snd_pcm_wait` polls it and returns without touching anything else.
    let waited = unsafe { (entry_points.snd_pcm_wait)(pcm, DEVICE_WAIT_TIMEOUT_MS) };
    if waited == 0 {
        return Ok(DevicePeriodReadiness::NothingYet);
    }
    if waited < 0 {
        if recover_from_a_transfer_failure(entry_points, pcm, device_name, waited, direction)
            == AlsaTransferRecovery::StreamIsUnusable
        {
            return Err(entry_points.refuse(
                format_args!("waiting on '{device_name}' for {}", direction.as_word()),
                waited,
            ));
        }
        return Ok(DevicePeriodReadiness::NothingYet);
    }
    Ok(DevicePeriodReadiness::Ready)
}

/// Wait for a period, then fill `status` with the device's own account of where
/// it is.
///
/// Status is read *before* the samples are, which is what makes "status minus
/// reported delay" name the first sample of the block about to be read: at this
/// instant the reported delay is exactly the samples captured and not yet
/// handed over, and the oldest of them is the one the next read starts at.
fn read_status_of_a_readable_period(
    entry_points: &AlsaLibraryEntryPoints,
    pcm: *mut c_void,
    device_status: &AlsaAllocatedObject,
    device_name: &str,
) -> Result<DevicePeriodReadiness> {
    if wait_for_the_device_to_release_a_period(
        entry_points,
        pcm,
        device_name,
        AlsaStreamDirection::Capture,
    )? == DevicePeriodReadiness::NothingYet
    {
        return Ok(DevicePeriodReadiness::NothingYet);
    }

    // SAFETY: `pcm` is open and the status is a live libasound allocation.
    entry_points.refuse_a_negative_return_code(
        format_args!("reading the device status of '{device_name}'"),
        unsafe { (entry_points.snd_pcm_status)(pcm, device_status.pointer()) },
    )?;
    Ok(DevicePeriodReadiness::Ready)
}

/// Whether a stream survived a failed transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlsaTransferRecovery {
    TransferMayContinue,
    StreamIsUnusable,
}

/// Ask libasound to put a broken stream back together, and say whether the
/// transfer may continue.
///
/// A break leaves a gap, and the gap is left visible rather than papered over:
/// on capture the timestamps and sample counts either side of it say exactly
/// how much audio went missing, and on playback the count of what the device
/// had to be given instead does.
///
/// One function for both directions because everything but the restart is the
/// same, and the restart is the one thing that must *not* be: a capture stream
/// this arm started by hand has to be started again, and starting a prepared
/// playback stream whose buffer is empty underruns it on the spot.
fn recover_from_a_transfer_failure(
    entry_points: &AlsaLibraryEntryPoints,
    pcm: *mut c_void,
    device_name: &str,
    error_code: c_int,
    direction: AlsaStreamDirection,
) -> AlsaTransferRecovery {
    // SAFETY: `pcm` is an open handle held by this thread alone.
    let recovered = unsafe { (entry_points.snd_pcm_recover)(pcm, error_code, 1) };
    if recovered < 0 {
        tracing::error!(
            device = %device_name,
            "ALSA {} could not recover from {}: {}",
            direction.as_word(),
            entry_points.error_text(error_code),
            entry_points.error_text(recovered)
        );
        return AlsaTransferRecovery::StreamIsUnusable;
    }
    // Which state recovery left the stream in depends on what broke it, so the
    // stream is asked rather than assumed. `snd_pcm_prepare` against a running
    // PCM is `EBUSY` on a raw device — and silently fine through the PipeWire
    // ALSA plugin, which is why only a rig with real hardware would ever show
    // it.
    // SAFETY: `pcm` is an open handle held by this thread alone.
    let state_recovery_left = unsafe { (entry_points.snd_pcm_state)(pcm) };
    if direction == AlsaStreamDirection::Capture
        && a_recovered_stream_still_has_to_be_started(state_recovery_left)
        && let Err(refusal) = prepare_and_start_capture(entry_points, pcm, device_name)
    {
        tracing::error!(
            device = %device_name,
            %refusal,
            "ALSA capture recovered but could not be restarted"
        );
        return AlsaTransferRecovery::StreamIsUnusable;
    }
    if error_code == ALSA_BROKEN_PIPE || error_code == ALSA_SUSPENDED {
        tracing::warn!(
            device = %device_name,
            "ALSA {} recovered from {} — the audio it covers is missing, and {}",
            direction.as_word(),
            entry_points.error_text(error_code),
            direction.where_the_missing_audio_shows_up()
        );
    }
    AlsaTransferRecovery::TransferMayContinue
}

/// Whether a stream `snd_pcm_recover` handed back still has to be started.
///
/// Recovery does not mean the same thing for every failure: an overrun comes
/// back `PREPARED` and must be started again, because this arm disables ALSA's
/// implicit start — but an interrupted wait, and a suspend whose resume
/// succeeded, come back still `RUNNING`, where preparing is `EBUSY` and the
/// stream needs nothing.
fn a_recovered_stream_still_has_to_be_started(state_recovery_left: c_int) -> bool {
    state_recovery_left != SND_PCM_STATE_RUNNING
}

/// The device's own timing for its most recent hardware pointer update, in
/// nanoseconds on the clock the software parameters demanded.
///
/// # Safety
///
/// `device_status` must have been filled by a successful `snd_pcm_status` call.
unsafe fn read_htimestamp_ns(
    entry_points: &AlsaLibraryEntryPoints,
    device_status: &AlsaAllocatedObject,
) -> i64 {
    let mut device_stamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: the caller guarantees a filled status, and the out-parameter is an
    // owned local.
    unsafe {
        (entry_points.snd_pcm_status_get_htstamp)(device_status.pointer(), &mut device_stamp)
    };
    device_stamp
        .tv_sec
        .saturating_mul(1_000_000_000)
        .saturating_add(device_stamp.tv_nsec)
}

/// When the first sample of the block about to be read was captured.
///
/// `device_stamp_ns` is the device's time for its most recent hardware pointer
/// update — the newest sample it holds — and `unread_sample_count` is how many
/// samples it holds that have not been read. So the oldest unread sample, which
/// is the block's first, sits that many samples in the past. This is the value
/// a consumer joins a camera frame on, and it is not the instant of delivery: a
/// `MediaClock::now()`-at-publish stamp would land a whole block later.
fn first_sample_timestamp_ns(
    device_stamp_ns: i64,
    unread_sample_count: SndPcmSframes,
    sample_rate: u32,
) -> i64 {
    if sample_rate == 0 || unread_sample_count <= 0 {
        return device_stamp_ns;
    }
    let unread_nanoseconds =
        i128::from(unread_sample_count) * 1_000_000_000 / i128::from(sample_rate);
    device_stamp_ns.saturating_sub(i64::try_from(unread_nanoseconds).unwrap_or(i64::MAX))
}

/// Refuse a device whose own stamp is not on the machine monotonic clock.
///
/// Setting `SND_PCM_TSTAMP_TYPE_MONOTONIC` is a request, and a driver or an
/// ALSA plugin is free to fill `htstamp` with something else — or with nothing.
/// Publishing such a value would corrupt every A/V join downstream in a way
/// nothing later can detect.
///
/// The bracket is taken around the stream's own start, so nothing the device
/// captured can legitimately fall outside it: a wall-clock stamp misses by five
/// decades, and a zeroed one — the device reporting no time at all — misses by
/// however long the machine has been up.
fn refuse_a_stamp_outside_the_monotonic_domain(
    device_name: &str,
    device_stamp_ns: i64,
    started_at_ns: i64,
    read_back_at_ns: i64,
) -> Result<()> {
    if (started_at_ns..=read_back_at_ns).contains(&device_stamp_ns) {
        return Ok(());
    }
    Err(Error::NotSupported(format!(
        "ALSA device '{device_name}' stamped its first period at {device_stamp_ns} ns, \
         outside the CLOCK_MONOTONIC bracket [{started_at_ns}, {read_back_at_ns}] the stream \
         ran in — it ignored SND_PCM_TSTAMP_TYPE_MONOTONIC, so its stamps cannot be joined \
         with a frame timestamp"
    )))
}

fn monotonic_now_ns() -> i64 {
    MediaClock::now().as_nanos() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stop the owner asked for is not a failure, and this is the assertion
    /// that keeps it that way: reported as one, the signal would fire on every
    /// clean teardown and an owner could no longer tell a dead microphone from
    /// one it switched off.
    #[test]
    fn a_thread_that_stopped_because_it_was_told_to_reports_no_failure() {
        for direction in [AlsaStreamDirection::Capture, AlsaStreamDirection::Playback] {
            let (failure_recorder, liveness_report) =
                AudioStreamFailureRecorder::recording_into_a_new_report();

            record_an_alsa_device_thread_exit(
                &AlsaDeviceThreadExit::StopThatWasAskedFor,
                direction,
                "default",
                &failure_recorder,
            );

            assert!(
                liveness_report.failure_that_ended_the_stream().is_none(),
                "a deliberate stop was reported as a {} failure",
                direction.as_word()
            );
        }
    }

    /// Every way a transfer thread can end early, in both directions: each one
    /// leaves the owner a reason it can act on rather than only a log line.
    ///
    /// Driven through the exit value rather than through a device, which is
    /// what makes the three reachable at all — two of them need hardware that
    /// has begun failing, and the third needs a `snd_pcm_recover` that cannot.
    #[test]
    fn every_way_a_thread_dies_early_reaches_the_owner_naming_what_happened() {
        let exits_and_what_they_must_say = [
            (
                AlsaDeviceThreadExit::DeviceWentQuiet {
                    consecutive_silent_waits: CONSECUTIVE_SILENT_WAITS_BEFORE_GIVING_UP,
                },
                "consecutive waits",
            ),
            (
                AlsaDeviceThreadExit::DeviceRefused(Error::Runtime(
                    "snd_pcm_status refused".to_string(),
                )),
                "snd_pcm_status refused",
            ),
            (
                AlsaDeviceThreadExit::StreamCouldNotBeRecovered,
                "could not be recovered",
            ),
        ];

        for (exit, what_the_reason_must_say) in exits_and_what_they_must_say {
            for direction in [AlsaStreamDirection::Capture, AlsaStreamDirection::Playback] {
                let (failure_recorder, liveness_report) =
                    AudioStreamFailureRecorder::recording_into_a_new_report();

                record_an_alsa_device_thread_exit(&exit, direction, "hw:0,0", &failure_recorder);

                let reason = liveness_report
                    .failure_that_ended_the_stream()
                    .expect("a thread that ended early has to leave its owner a reason")
                    .to_string();
                assert!(
                    reason.contains(what_the_reason_must_say),
                    "a {} stream's reason has to name what happened, not just that something \
                     did: {reason}",
                    direction.as_word()
                );
                assert!(
                    reason.contains(direction.as_word()),
                    "the reason has to name the direction it came from, because a graph runs \
                     both against the same device: {reason}"
                );
            }
        }
    }

    /// The stalled-device reason says the opposite thing either side of the
    /// seam, and the wait that produced it is the same call — so a reader
    /// looking at a `took nothing` line knows it is the speaker.
    #[test]
    fn a_stalled_device_is_described_by_what_that_direction_stopped_doing() {
        assert_eq!(
            AlsaStreamDirection::Capture.what_a_stalled_device_stopped_doing(),
            "delivered"
        );
        assert_eq!(
            AlsaStreamDirection::Playback.what_a_stalled_device_stopped_doing(),
            "took"
        );
    }

    const SAMPLE_RATE_48K: u32 = 48_000;
    /// 512 samples at 48 kHz.
    const PERIOD_NANOS: i64 = 10_666_666;

    /// A soname nothing on any machine resolves, so the loader's first refusal
    /// is reachable without a stub on disk.
    const A_SONAME_NO_MACHINE_HAS: &str = "libasound-this-soname-names-nothing.so.999";

    #[test]
    fn the_loader_names_the_library_it_could_not_open() {
        let refusal = AlsaLibraryEntryPoints::resolve_from(A_SONAME_NO_MACHINE_HAS)
            .err()
            .expect("no machine has this library");
        let refusal = refusal.to_string();
        assert!(
            refusal.contains(A_SONAME_NO_MACHINE_HAS) && refusal.contains("could not be loaded"),
            "the demotion log line has to say which library was missing: {refusal}"
        );
    }

    /// A library present on every machine this arm targets — it is already
    /// mapped into this process — which exports no `snd_*` symbol at all.
    const A_LIBRARY_THAT_IS_NOT_ALSA: &str = "libc.so.6";

    /// The second refusal: a library that loads and exports none of this. It is
    /// the one a host with a stale or wrongly-named `libasound.so.2` produces,
    /// and reporting it as "missing" would send a reader looking for a file
    /// that is right there.
    #[test]
    fn the_loader_names_the_symbol_a_wrong_library_does_not_export() {
        let refusal = AlsaLibraryEntryPoints::resolve_from(A_LIBRARY_THAT_IS_NOT_ALSA)
            .err()
            .expect("libc is not libasound");
        let refusal = refusal.to_string();
        assert!(
            refusal.contains("exports no snd_"),
            "a library that loads but is not ALSA must be named as such, not reported as \
             missing: {refusal}"
        );
    }

    /// Mental revert: drop the delay term and this returns the status time,
    /// stamping the block at its end and making a joined camera frame one
    /// period late.
    #[test]
    fn a_blocks_stamp_sits_one_period_before_a_status_reporting_one_unread_period() {
        let device_stamp_ns = 541_560_469_372_719;
        assert_eq!(
            first_sample_timestamp_ns(device_stamp_ns, 512, SAMPLE_RATE_48K),
            device_stamp_ns - PERIOD_NANOS
        );
    }

    /// A device that has fallen further behind reports more unread samples, and
    /// the block it is about to hand over starts correspondingly earlier.
    #[test]
    fn more_unread_samples_move_the_stamp_further_into_the_past() {
        let device_stamp_ns = 1_000_000_000;
        assert_eq!(
            first_sample_timestamp_ns(device_stamp_ns, 1024, SAMPLE_RATE_48K),
            device_stamp_ns - 2 * PERIOD_NANOS - 1,
            "two periods of unread samples put the first one two periods back"
        );
    }

    /// Consecutive periods are one block apart, which is what makes
    /// `first_sample_timestamp_ns + sample_count / sample_rate` the next
    /// block's expected stamp — the property a consumer joins on.
    #[test]
    fn consecutive_periods_are_exactly_one_block_apart() {
        let first_status_ns = 1_000_000_000;
        let second_status_ns = first_status_ns + PERIOD_NANOS;
        assert_eq!(
            first_sample_timestamp_ns(second_status_ns, 512, SAMPLE_RATE_48K)
                - first_sample_timestamp_ns(first_status_ns, 512, SAMPLE_RATE_48K),
            PERIOD_NANOS
        );
    }

    const A_MONOTONIC_START_NS: i64 = 541_560_000_000_000;
    /// What `snd_pcm_status_get_htstamp` hands back when the device ignored the
    /// requested timestamp type: seconds since 1970, not since boot.
    const A_WALL_CLOCK_STAMP_NS: i64 = 1_787_879_043_681_980_184;

    fn refuse_a_stamp_of(device_stamp_ns: i64) -> Result<()> {
        refuse_a_stamp_outside_the_monotonic_domain(
            "a-test-device",
            device_stamp_ns,
            A_MONOTONIC_START_NS,
            A_MONOTONIC_START_NS + PERIOD_NANOS,
        )
    }

    /// The guard that keeps a wrong-epoch stamp off the data plane, driven red
    /// by each way a device can miss the domain — including what a host whose
    /// `alsa.conf` leaves `defaults.pcm.tstamp_type` unset actually produces.
    #[test]
    fn a_stamp_from_the_wrong_clock_is_refused_and_a_monotonic_one_is_not() {
        assert!(refuse_a_stamp_of(A_MONOTONIC_START_NS + 1).is_ok());

        let refusal = refuse_a_stamp_of(A_WALL_CLOCK_STAMP_NS)
            .expect_err("a wall-clock stamp is five decades outside any uptime bracket");
        assert!(
            matches!(refusal, Error::NotSupported(_)),
            "a device that cannot be timed is unsupported, not a runtime failure: {refusal:?}"
        );
        let refusal = refusal.to_string();
        assert!(
            refusal.contains("a-test-device")
                && refusal.contains(&A_WALL_CLOCK_STAMP_NS.to_string()),
            "the refusal has to name the device and the stamp it refused: {refusal}"
        );

        assert!(
            refuse_a_stamp_of(0).is_err(),
            "a zeroed htstamp is a device reporting no time at all, not time zero"
        );
        assert!(
            refuse_a_stamp_of(A_MONOTONIC_START_NS + PERIOD_NANOS + 1).is_err(),
            "a device cannot have captured a sample after the status that reported it"
        );
    }

    /// Ask libasound to spell back every ABI constant this arm hard-codes.
    ///
    /// The values are the arm's only claim about a library it never links, and
    /// a wrong one is silent: `SND_PCM_STATE_RUNNING` off by two restores the
    /// recovery defect with every other test still green, because those tests
    /// are written in terms of the constant rather than the value. libasound
    /// names its own enumerators, so they are checked against the library that
    /// defines them rather than against a header someone read once.
    #[test]
    fn libasound_spells_back_every_abi_constant_this_arm_hard_codes() {
        // SAFETY: `dlopen` of a soname; nothing is dereferenced unless it opens.
        let Ok(library) = (unsafe { Library::new(ALSA_LIBRARY_SONAME) }) else {
            // A machine with no ALSA cannot answer the question. The chain is
            // built for exactly that machine, so it is not a failure.
            return;
        };

        for (naming_entry_point, enumerator, expected_name) in [
            (
                &b"snd_pcm_stream_name\0"[..],
                SND_PCM_STREAM_CAPTURE,
                "CAPTURE",
            ),
            (
                b"snd_pcm_access_name\0",
                SND_PCM_ACCESS_RW_INTERLEAVED,
                "RW_INTERLEAVED",
            ),
            (b"snd_pcm_format_name\0", SND_PCM_FORMAT_S16_LE, "S16_LE"),
            (
                b"snd_pcm_format_name\0",
                SND_PCM_FORMAT_FLOAT_LE,
                "FLOAT_LE",
            ),
            (
                b"snd_pcm_tstamp_mode_name\0",
                SND_PCM_TSTAMP_ENABLE,
                "ENABLE",
            ),
            (
                b"snd_pcm_tstamp_type_name\0",
                SND_PCM_TSTAMP_TYPE_MONOTONIC,
                "MONOTONIC",
            ),
            (b"snd_pcm_state_name\0", SND_PCM_STATE_RUNNING, "RUNNING"),
        ] {
            // SAFETY: every one of these is an `ALSA_0.9` entry point taking one
            // enumerator and returning a pointer into libasound's own static
            // name table, valid while the library is loaded.
            let named = unsafe {
                let name_of: libloading::Symbol<'_, unsafe extern "C" fn(c_int) -> *const c_char> =
                    library
                        .get(naming_entry_point)
                        .expect("libasound names its own enumerators");
                let named = name_of(enumerator);
                assert!(!named.is_null(), "libasound named nothing for {enumerator}");
                CStr::from_ptr(named).to_string_lossy().into_owned()
            };
            assert_eq!(
                named, expected_name,
                "this arm calls {enumerator} {expected_name}, and libasound calls it {named}"
            );
        }

        // The two error codes have no name function; they are plain errnos, and
        // comparing them to the C library's own is exact where comparing
        // `snd_strerror` text would depend on the locale.
        assert_eq!(
            -ALSA_BROKEN_PIPE,
            libc::EPIPE,
            "a capture overrun and a playback underrun are both EPIPE"
        );
        assert_eq!(
            -ALSA_SUSPENDED,
            libc::ESTRPIPE,
            "a suspended stream is ESTRPIPE"
        );
    }

    /// `snd_pcm_recover` does not mean one thing. Reverting this to an
    /// unconditional restart is what a first reading of "recover only prepares"
    /// gives you, and it kills capture on the first interrupted wait —
    /// `snd_pcm_prepare` against a running PCM is `EBUSY` on a raw device, and
    /// silently fine through the PipeWire ALSA plugin, so only a rig with real
    /// hardware would ever show it.
    #[test]
    fn only_a_stream_recovery_left_stopped_is_started_again() {
        const SND_PCM_STATE_PREPARED: c_int = 2;
        const SND_PCM_STATE_XRUN: c_int = 4;
        const SND_PCM_STATE_SETUP: c_int = 1;

        assert!(
            !a_recovered_stream_still_has_to_be_started(SND_PCM_STATE_RUNNING),
            "an interrupted wait, and a suspend whose resume succeeded, come back running — \
             preparing one of those is EBUSY"
        );
        for state_recovery_left in [
            SND_PCM_STATE_PREPARED,
            SND_PCM_STATE_XRUN,
            SND_PCM_STATE_SETUP,
        ] {
            assert!(
                a_recovered_stream_still_has_to_be_started(state_recovery_left),
                "a stream left in state {state_recovery_left} delivers nothing until it is \
                 started, and this arm disables ALSA's implicit start"
            );
        }
    }

    /// A stream that has not settled reports neither a rate nor a delay, and no
    /// offset is derivable from that — the stamp must still be the device's
    /// rather than a division by zero or a stamp from the future.
    #[test]
    fn an_unsettled_stream_contributes_no_offset_rather_than_dividing_by_zero() {
        assert_eq!(
            first_sample_timestamp_ns(1_000_000_000, 512, 0),
            1_000_000_000
        );
        assert_eq!(
            first_sample_timestamp_ns(1_000_000_000, 0, SAMPLE_RATE_48K),
            1_000_000_000
        );
        assert_eq!(
            first_sample_timestamp_ns(1_000_000_000, -512, SAMPLE_RATE_48K),
            1_000_000_000,
            "a negative delay is not a stamp in the future"
        );
    }
}
