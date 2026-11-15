// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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
    AudioDeviceBackend, AudioDeviceStreamRequest, AudioPlaybackStream, AudioSampleFormat,
    AudioStreamFormat, CapturedAudioBlockFromDevice, CapturedAudioBlockHandOff,
    DeviceBackendArmUnavailableReason, DeviceStreamFailureReason, DeviceStreamFailureRecorder,
    DeviceStreamLivenessReport, RequestedAudioStreamFormat,
};
use crate::core::execution::ThreadPriority;
use crate::core::media_clock::MediaClock;
use crate::core::{Error, Result};
use crate::linux::thread_priority::apply_thread_priority;
use alsa_capture_stream_and_reader_thread::prepare_and_start_capture;
use alsa_hardware_parameter_negotiation::{
    negotiate_capture_timestamp_contract, negotiate_hardware_parameters,
    negotiate_playback_start_contract,
};
use alsa_period_wait_recovery_and_timestamps::{
    AlsaTransferRecovery, DevicePeriodReadiness, first_sample_timestamp_ns, monotonic_now_ns,
    read_htimestamp_ns, read_status_of_a_readable_period, recover_from_a_transfer_failure,
    refuse_a_stamp_outside_the_monotonic_domain, wait_for_the_device_to_release_a_period,
};

mod alsa_capture_stream_and_reader_thread;
mod alsa_hardware_parameter_negotiation;
mod alsa_period_wait_recovery_and_timestamps;
mod alsa_playback_stream_and_writer_thread;
#[cfg(test)]
mod tests;

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

/// Preferred playback channel count where a request names none. Stereo rather
/// than the capture side's mono because that is what a sink almost always is;
/// `_near` lets a mono-only device say so.
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
            ) -> std::result::Result<Self, DeviceBackendArmUnavailableReason> {
                // SAFETY: `dlopen` of a soname. Loading an audio library can run
                // its initialisers, which is what the chain's probe-by-opening
                // accepts; nothing is dereferenced until `get` succeeds.
                let library = unsafe { Library::new(library_soname) }.map_err(|e| {
                    DeviceBackendArmUnavailableReason::of(format!(
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
                        DeviceBackendArmUnavailableReason::of(format!(
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
    fn resolve() -> std::result::Result<Self, DeviceBackendArmUnavailableReason> {
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
    pub fn load_and_open() -> std::result::Result<Self, DeviceBackendArmUnavailableReason> {
        let entry_points = Arc::new(AlsaLibraryEntryPoints::resolve()?);
        let backend = Self { entry_points };

        // Opened, timed, and closed again: holding a device across the probe
        // would claim a card nothing is capturing from yet.
        let probe_outcome = backend
            .open_alsa_capture_stream(DEFAULT_PCM_NAME)
            .and_then(|mut probe| probe.prove_the_device_can_be_timed());
        if let Err(refusal) = probe_outcome {
            return Err(DeviceBackendArmUnavailableReason::of(format!(
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
        Ok(Box::new(self.open_alsa_playback_stream(
            pcm_name,
            request.requested_format,
        )?))
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
    ) -> Option<DeviceStreamFailureReason> {
        let direction_word = direction.as_word();
        match self {
            AlsaDeviceThreadExit::StopThatWasAskedFor => None,
            AlsaDeviceThreadExit::DeviceWentQuiet {
                consecutive_silent_waits,
            } => Some(DeviceStreamFailureReason::of(format!(
                "the ALSA {direction_word} device {} nothing for {consecutive_silent_waits} \
                 consecutive waits",
                direction.what_a_stalled_device_stopped_doing(),
            ))),
            AlsaDeviceThreadExit::DeviceRefused(refusal) => Some(DeviceStreamFailureReason::of(
                format!("the ALSA {direction_word} device refused while it was running: {refusal}"),
            )),
            AlsaDeviceThreadExit::StreamCouldNotBeRecovered => Some(DeviceStreamFailureReason::of(
                format!("the ALSA {direction_word} stream broke and could not be recovered"),
            )),
        }
    }
}

/// What a run of silent waits long enough to give up on actually means.
///
/// A wait blocks for up to `DEVICE_WAIT_TIMEOUT_MS`, so a stop can be requested
/// after the loop last read the flag and before the silence reaches its limit.
/// The stop outranks the silence: an owner that asked to stop and was told its
/// device had failed could no longer tell a dead microphone from one it
/// switched off, which is the single distinction this seam exists to make.
fn exit_for_a_device_that_delivered_nothing(
    stop_was_requested: bool,
    consecutive_silent_waits: u32,
) -> AlsaDeviceThreadExit {
    if stop_was_requested {
        return AlsaDeviceThreadExit::StopThatWasAskedFor;
    }
    AlsaDeviceThreadExit::DeviceWentQuiet {
        consecutive_silent_waits,
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
    failure_recorder: &DeviceStreamFailureRecorder,
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
