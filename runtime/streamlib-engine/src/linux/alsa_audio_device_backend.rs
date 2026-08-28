// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The audio backend chain's second arm: ALSA, reached entirely at runtime.
//!
//! It is the permanent fallback under PipeWire, and it is what makes the chain
//! honest on Debian desktops (which do not seed `pipewire-alsa`), on PulseAudio
//! holdouts, and on headless machines carrying `/dev/snd` and no audio daemon.
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
//! `snd_pcm_status`, and hands the block off — the cadence is still the
//! device's.

use std::ffi::{CStr, CString, c_char, c_int, c_long, c_uint, c_ulong, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use libloading::Library;

use crate::core::context::{
    AudioCaptureSampleFormat, AudioCaptureStream, AudioCaptureStreamFormat,
    AudioCaptureStreamRequest, AudioDeviceBackend, AudioDeviceBackendArmUnavailableReason,
    CapturedAudioBlockFromDevice, CapturedAudioBlockHandOff,
};
use crate::core::execution::ThreadPriority;
use crate::core::media_clock::MediaClock;
use crate::core::{Error, Result};
use crate::linux::thread_priority::apply_thread_priority;

/// The versioned soname. ALSA's is decades stable, and the bare `.so` symlink
/// ships only in `libasound2-dev` — which the machines this arm exists for do
/// not have.
const ALSA_LIBRARY_SONAME: &str = "libasound.so.2";

/// The PCM name opened when no caller names a device.
///
/// `default` and never a raw `hw:` node: raw hardware access bypasses any
/// daemon holding the card and returns `EBUSY` (measured on the rig). A caller
/// that names `hw:0,0` gets exactly that, because a named device is a wiring
/// statement.
const DEFAULT_CAPTURE_PCM_NAME: &str = "default";

/// `snd_pcm_uframes_t` — `unsigned long`, per `<alsa/pcm.h>`.
type SndPcmUframes = c_ulong;

/// `snd_pcm_sframes_t` — `signed long`, per `<alsa/pcm.h>`.
type SndPcmSframes = c_long;

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
/// `snd_pcm_tstamp_type_t`.
///
/// Set explicitly rather than assumed: the first variant is
/// `gettimeofday`-based, and a wall-clock stamp on the data plane is not a
/// style question — every StreamLib timestamp is the machine monotonic clock,
/// the epoch a `VideoFrame.timestamp_ns` carries, and joining audio to video is
/// subtracting two integers in it.
const SND_PCM_TSTAMP_TYPE_MONOTONIC: c_int = 1;

/// `-EPIPE`: a capture overrun. Recoverable, and the gap it leaves is visible
/// in the timestamps of the blocks either side of it.
const ALSA_OVERRUN: c_int = -32;

/// `-ESTRPIPE`: the stream was suspended (system sleep). `snd_pcm_recover`
/// handles it too.
const ALSA_SUSPENDED: c_int = -86;

/// Preferred capture rate. ALSA hands back a range rather than negotiating one
/// the way PipeWire does, so the arm has to state a preference — 48 kHz is what
/// every modern device does natively and what the deviceless clock defaults to.
/// `_near` means the device's own answer is what the stream reports.
const PREFERRED_CAPTURE_SAMPLE_RATE: c_uint = 48_000;

/// Preferred channel count. Mono is what a capture endpoint is usually asked
/// for and what the null arm produces; `_near` lets a stereo-only device say so.
const PREFERRED_CAPTURE_CHANNELS: c_uint = 1;

/// Preferred period, ~10.7 ms at 48 kHz — a block small enough to be a useful
/// latency unit and large enough that a wake per period is not a busy loop.
const PREFERRED_CAPTURE_PERIOD_SAMPLE_COUNT: SndPcmUframes = 512;

/// Periods held in the device buffer. Four is the usual floor for surviving a
/// scheduling hiccup without an overrun.
const CAPTURE_PERIODS_PER_DEVICE_BUFFER: SndPcmUframes = 4;

/// How long the reader thread waits for a period before looking at the stop
/// flag again. Long enough that it is not a poll loop, short enough that
/// `stop_delivering` joins promptly.
const CAPTURE_WAIT_TIMEOUT_MS: c_int = 200;

/// `snd_pcm_wait` returning this many times in a row with no data is a device
/// that stopped rather than a slow one.
const CONSECUTIVE_SILENT_WAITS_BEFORE_GIVING_UP: u32 = 25;

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
    fn snd_pcm_recover(*mut c_void, c_int, c_int) -> c_int;

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
    fn refuse(&self, attempted: &str, error_code: c_int) -> Error {
        Error::Runtime(format!(
            "ALSA refused {attempted}: {}",
            self.error_text(error_code)
        ))
    }
}

/// A libasound-allocated parameter object, freed by the entry point that owns
/// it.
///
/// The `_alloca` spellings in `<alsa/pcm.h>` are C macros over `alloca` and are
/// not reachable through `dlsym`; the `_malloc` / `_free` pair is the API's own
/// answer for callers that are not C.
struct AlsaAllocatedObject {
    pointer: *mut c_void,
    free: unsafe extern "C" fn(*mut c_void),
}

impl AlsaAllocatedObject {
    fn allocated_by(
        entry_points: &AlsaLibraryEntryPoints,
        allocate: unsafe extern "C" fn(*mut *mut c_void) -> c_int,
        free: unsafe extern "C" fn(*mut c_void),
        what: &str,
    ) -> Result<Self> {
        let mut pointer = std::ptr::null_mut();
        // SAFETY: the out-parameter is an owned local, and libasound writes a
        // pointer it allocated into it on success.
        let allocated = unsafe { allocate(&mut pointer) };
        if allocated < 0 || pointer.is_null() {
            return Err(entry_points.refuse(&format!("allocating {what}"), allocated));
        }
        Ok(Self { pointer, free })
    }
}

impl AlsaAllocatedObject {
    fn pointer(&self) -> *mut c_void {
        self.pointer
    }
}

impl Drop for AlsaAllocatedObject {
    fn drop(&mut self) {
        // SAFETY: the pointer came from the paired allocator and is freed
        // exactly once, here.
        unsafe { (self.free)(self.pointer) };
    }
}

// The pointer addresses a libasound heap allocation this struct exclusively
// owns; nothing else holds a copy, and only the thread holding the struct
// touches it.
unsafe impl Send for AlsaAllocatedObject {}

/// An open capture PCM, closed exactly once when the last holder drops.
struct OpenedAlsaCapturePcm {
    entry_points: Arc<AlsaLibraryEntryPoints>,
    pcm: *mut c_void,
}

impl Drop for OpenedAlsaCapturePcm {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `snd_pcm_open` and is closed once.
        unsafe { (self.entry_points.snd_pcm_close)(self.pcm) };
    }
}

// A PCM handle is not internally synchronised, so this is a claim about the
// callers rather than about libasound: the stream makes its own ALSA calls
// before it spawns the reader thread and after it has joined it, and `Drop`
// stops delivery before it drops this — so exactly one thread is ever inside
// libasound with this handle.
unsafe impl Send for OpenedAlsaCapturePcm {}
unsafe impl Sync for OpenedAlsaCapturePcm {}

/// Audio over ALSA, with libasound bound at runtime.
pub struct AlsaAudioDeviceBackend {
    entry_points: Arc<AlsaLibraryEntryPoints>,
}

impl AlsaAudioDeviceBackend {
    /// Load libasound and confirm a capture device actually opens, or say why
    /// this arm cannot serve so the chain can demote.
    ///
    /// The open round trip is the point, and it is the same rule the PipeWire
    /// arm's connection check follows: `libasound` present with no `/dev/snd`
    /// behind it is the ordinary container case, and probing on presence alone
    /// would strand exactly the machines the chain exists to serve.
    pub fn load_and_open() -> std::result::Result<Self, AudioDeviceBackendArmUnavailableReason> {
        let entry_points = Arc::new(AlsaLibraryEntryPoints::resolve()?);

        // Opened and closed again: the arm is chosen by opening, and holding a
        // device across the probe would claim a card nothing is capturing from
        // yet.
        let probed = OpenedAlsaCapturePcm::open(&entry_points, DEFAULT_CAPTURE_PCM_NAME).map_err(
            |refusal| {
                AudioDeviceBackendArmUnavailableReason::of(format!(
                    "{ALSA_LIBRARY_SONAME} loaded but no capture device answered on \
                     '{DEFAULT_CAPTURE_PCM_NAME}': {refusal}"
                ))
            },
        )?;
        drop(probed);

        // SAFETY: returns a pointer to libasound's own static version string,
        // valid for as long as the library is loaded.
        let library_version = unsafe { (entry_points.snd_asoundlib_version)() };
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
            "ALSA audio arm: a capture device opened"
        );

        Ok(Self { entry_points })
    }
}

impl OpenedAlsaCapturePcm {
    fn open(entry_points: &Arc<AlsaLibraryEntryPoints>, pcm_name: &str) -> Result<Self> {
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
                SND_PCM_STREAM_CAPTURE,
                SND_PCM_OPEN_MODE_BLOCKING,
            )
        };
        if opened < 0 || pcm.is_null() {
            return Err(entry_points.refuse(&format!("opening capture PCM '{pcm_name}'"), opened));
        }
        Ok(Self {
            entry_points: Arc::clone(entry_points),
            pcm,
        })
    }
}

impl AudioDeviceBackend for AlsaAudioDeviceBackend {
    fn backend_name(&self) -> &'static str {
        "alsa"
    }

    fn open_capture_stream(
        &self,
        request: &AudioCaptureStreamRequest,
    ) -> Result<Box<dyn AudioCaptureStream>> {
        // The device paces this arm, so the request's deviceless pacing clock
        // is deliberately untouched: a graph whose audio is device-paced never
        // starts the timer, which is what keeps device ticks and timer ticks
        // from interleaving.
        let pcm_name = request
            .device_id
            .as_deref()
            .unwrap_or(DEFAULT_CAPTURE_PCM_NAME);
        let opened_pcm = OpenedAlsaCapturePcm::open(&self.entry_points, pcm_name)?;
        let negotiated = negotiate_capture_stream(&self.entry_points, opened_pcm.pcm, pcm_name)?;

        Ok(Box::new(AlsaAudioCaptureStream {
            opened_pcm: Arc::new(opened_pcm),
            capture_stream_format: negotiated.capture_stream_format,
            period_sample_count: negotiated.period_sample_count,
            device_name: pcm_name.to_string(),
            delivery: None,
        }))
    }
}

/// What the hardware and software parameter passes settled on.
struct NegotiatedCaptureStream {
    capture_stream_format: AudioCaptureStreamFormat,
    period_sample_count: u32,
}

/// Settle the hardware parameters, then the software ones — the second pass is
/// where the timestamp contract is stated.
fn negotiate_capture_stream(
    entry_points: &AlsaLibraryEntryPoints,
    pcm: *mut c_void,
    pcm_name: &str,
) -> Result<NegotiatedCaptureStream> {
    let hardware_parameters = AlsaAllocatedObject::allocated_by(
        entry_points,
        entry_points.snd_pcm_hw_params_malloc,
        entry_points.snd_pcm_hw_params_free,
        "hardware parameters",
    )?;

    // SAFETY (every call in this function): `pcm` is an open capture handle and
    // the parameter object is a live libasound allocation; every out-parameter
    // is an owned local outliving its call.
    let filled =
        unsafe { (entry_points.snd_pcm_hw_params_any)(pcm, hardware_parameters.pointer()) };
    if filled < 0 {
        return Err(entry_points.refuse(
            &format!("reading the parameter space of '{pcm_name}'"),
            filled,
        ));
    }

    let access_set = unsafe {
        (entry_points.snd_pcm_hw_params_set_access)(
            pcm,
            hardware_parameters.pointer(),
            SND_PCM_ACCESS_RW_INTERLEAVED,
        )
    };
    if access_set < 0 {
        return Err(entry_points.refuse("interleaved read/write access", access_set));
    }

    let sample_format = negotiate_sample_format(entry_points, pcm, hardware_parameters.pointer())?;

    let mut channels = PREFERRED_CAPTURE_CHANNELS;
    let channels_set = unsafe {
        (entry_points.snd_pcm_hw_params_set_channels_near)(
            pcm,
            hardware_parameters.pointer(),
            &mut channels,
        )
    };
    if channels_set < 0 {
        return Err(entry_points.refuse("a channel count", channels_set));
    }

    let mut sample_rate = PREFERRED_CAPTURE_SAMPLE_RATE;
    let rate_set = unsafe {
        (entry_points.snd_pcm_hw_params_set_rate_near)(
            pcm,
            hardware_parameters.pointer(),
            &mut sample_rate,
            std::ptr::null_mut(),
        )
    };
    if rate_set < 0 {
        return Err(entry_points.refuse("a sample rate", rate_set));
    }

    let mut period_sample_count = PREFERRED_CAPTURE_PERIOD_SAMPLE_COUNT;
    let period_set = unsafe {
        (entry_points.snd_pcm_hw_params_set_period_size_near)(
            pcm,
            hardware_parameters.pointer(),
            &mut period_sample_count,
            std::ptr::null_mut(),
        )
    };
    if period_set < 0 {
        return Err(entry_points.refuse("a period size", period_set));
    }

    let mut device_buffer_sample_count =
        period_sample_count.saturating_mul(CAPTURE_PERIODS_PER_DEVICE_BUFFER);
    let buffer_set = unsafe {
        (entry_points.snd_pcm_hw_params_set_buffer_size_near)(
            pcm,
            hardware_parameters.pointer(),
            &mut device_buffer_sample_count,
        )
    };
    if buffer_set < 0 {
        return Err(entry_points.refuse("a device buffer size", buffer_set));
    }

    let applied = unsafe { (entry_points.snd_pcm_hw_params)(pcm, hardware_parameters.pointer()) };
    if applied < 0 {
        return Err(entry_points.refuse(
            &format!("the negotiated hardware parameters for '{pcm_name}'"),
            applied,
        ));
    }

    // Read back rather than trusting the requests: every `_near` setter is free
    // to land somewhere else, and what the stream reports has to be what the
    // device is actually doing.
    let mut settled_channels = 0;
    let read_channels = unsafe {
        (entry_points.snd_pcm_hw_params_get_channels)(
            hardware_parameters.pointer(),
            &mut settled_channels,
        )
    };
    if read_channels < 0 {
        return Err(entry_points.refuse("reading back the channel count", read_channels));
    }
    let mut settled_sample_rate = 0;
    let read_rate = unsafe {
        (entry_points.snd_pcm_hw_params_get_rate)(
            hardware_parameters.pointer(),
            &mut settled_sample_rate,
            std::ptr::null_mut(),
        )
    };
    if read_rate < 0 {
        return Err(entry_points.refuse("reading back the sample rate", read_rate));
    }
    let mut settled_period_sample_count = 0;
    let read_period = unsafe {
        (entry_points.snd_pcm_hw_params_get_period_size)(
            hardware_parameters.pointer(),
            &mut settled_period_sample_count,
            std::ptr::null_mut(),
        )
    };
    if read_period < 0 {
        return Err(entry_points.refuse("reading back the period size", read_period));
    }
    if settled_sample_rate == 0 || settled_channels == 0 || settled_period_sample_count == 0 {
        return Err(Error::Runtime(format!(
            "ALSA settled '{pcm_name}' on {settled_sample_rate} Hz, {settled_channels} \
             channels and a {settled_period_sample_count}-sample period — no block duration \
             is derivable from that"
        )));
    }

    negotiate_timestamp_contract(entry_points, pcm, settled_period_sample_count, pcm_name)?;

    Ok(NegotiatedCaptureStream {
        capture_stream_format: AudioCaptureStreamFormat {
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
) -> Result<AudioCaptureSampleFormat> {
    for (alsa_format, sample_format) in [
        (SND_PCM_FORMAT_FLOAT_LE, AudioCaptureSampleFormat::F32),
        (SND_PCM_FORMAT_S16_LE, AudioCaptureSampleFormat::I16),
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

/// State the timestamp contract on the stream before it runs.
///
/// This is the whole reason the arm can be trusted for A/V join: without the
/// tstamp *mode* `snd_pcm_status` reports no time at all, and without the
/// tstamp *type* it reports one on the wrong clock.
fn negotiate_timestamp_contract(
    entry_points: &AlsaLibraryEntryPoints,
    pcm: *mut c_void,
    period_sample_count: SndPcmUframes,
    pcm_name: &str,
) -> Result<()> {
    let software_parameters = AlsaAllocatedObject::allocated_by(
        entry_points,
        entry_points.snd_pcm_sw_params_malloc,
        entry_points.snd_pcm_sw_params_free,
        "software parameters",
    )?;

    // SAFETY (every call here): `pcm` is an open capture handle whose hardware
    // parameters are applied, and the parameter object is a live libasound
    // allocation.
    let read_current =
        unsafe { (entry_points.snd_pcm_sw_params_current)(pcm, software_parameters.pointer()) };
    if read_current < 0 {
        return Err(entry_points.refuse(
            &format!("reading the software parameters of '{pcm_name}'"),
            read_current,
        ));
    }

    let mode_set = unsafe {
        (entry_points.snd_pcm_sw_params_set_tstamp_mode)(
            pcm,
            software_parameters.pointer(),
            SND_PCM_TSTAMP_ENABLE,
        )
    };
    if mode_set < 0 {
        return Err(entry_points.refuse("timestamping on the capture stream", mode_set));
    }

    let type_set = unsafe {
        (entry_points.snd_pcm_sw_params_set_tstamp_type)(
            pcm,
            software_parameters.pointer(),
            SND_PCM_TSTAMP_TYPE_MONOTONIC,
        )
    };
    if type_set < 0 {
        return Err(entry_points.refuse("monotonic timestamps on the capture stream", type_set));
    }

    // Wake the reader once a whole period is readable, which is what makes
    // "status minus reported delay" name the first sample of the block about to
    // be read rather than a sample still being captured.
    let avail_min_set = unsafe {
        (entry_points.snd_pcm_sw_params_set_avail_min)(
            pcm,
            software_parameters.pointer(),
            period_sample_count,
        )
    };
    if avail_min_set < 0 {
        return Err(entry_points.refuse("a one-period wake threshold", avail_min_set));
    }

    // A capture stream starts when `snd_pcm_start` says so, never implicitly on
    // the first read, so that the monotonic bracket taken around the start is a
    // real bracket.
    let start_threshold_set = unsafe {
        (entry_points.snd_pcm_sw_params_set_start_threshold)(
            pcm,
            software_parameters.pointer(),
            SndPcmUframes::MAX,
        )
    };
    if start_threshold_set < 0 {
        return Err(entry_points.refuse("an explicit start threshold", start_threshold_set));
    }

    let applied = unsafe { (entry_points.snd_pcm_sw_params)(pcm, software_parameters.pointer()) };
    if applied < 0 {
        return Err(
            entry_points.refuse(&format!("the timestamp contract on '{pcm_name}'"), applied)
        );
    }
    Ok(())
}

/// The reader thread and the flag that stops it.
struct CaptureDeliveryThread {
    stop_requested: Arc<AtomicBool>,
    reader_thread: JoinHandle<()>,
}

/// One ALSA capture stream, negotiated and ready to run.
struct AlsaAudioCaptureStream {
    opened_pcm: Arc<OpenedAlsaCapturePcm>,
    capture_stream_format: AudioCaptureStreamFormat,
    period_sample_count: u32,
    /// What the caller named, or `default` — for error text a reader can act on.
    device_name: String,
    delivery: Option<CaptureDeliveryThread>,
}

impl AudioCaptureStream for AlsaAudioCaptureStream {
    fn stream_format(&self) -> AudioCaptureStreamFormat {
        self.capture_stream_format
    }

    fn start_delivering_to(&mut self, hand_off: CapturedAudioBlockHandOff) -> Result<()> {
        self.stop_delivering()?;

        let opened_pcm = Arc::clone(&self.opened_pcm);
        let entry_points = &opened_pcm.entry_points;
        let pcm = opened_pcm.pcm;
        let status = AlsaAllocatedObject::allocated_by(
            entry_points,
            entry_points.snd_pcm_status_malloc,
            entry_points.snd_pcm_status_free,
            "a status object",
        )?;

        // SAFETY: `pcm` is an open, fully negotiated capture handle and no other
        // thread holds it — the reader thread is spawned below, after this.
        let prepared = unsafe { (entry_points.snd_pcm_prepare)(pcm) };
        if prepared < 0 {
            return Err(entry_points.refuse(
                &format!("preparing capture on '{}'", self.device_name),
                prepared,
            ));
        }

        // The bracket the timestamp-domain check needs: nothing was captured
        // before the stream started, so the device's stamp for its first period
        // cannot legitimately precede this.
        let started_at_ns = monotonic_now_ns();
        // SAFETY: as above.
        let started = unsafe { (entry_points.snd_pcm_start)(pcm) };
        if started < 0 {
            return Err(entry_points.refuse(
                &format!("starting capture on '{}'", self.device_name),
                started,
            ));
        }

        self.refuse_a_device_that_stamps_outside_the_monotonic_domain(&status, started_at_ns)?;

        let stop_requested = Arc::new(AtomicBool::new(false));
        let reader_thread = spawn_capture_reader_thread(CaptureReaderThreadInputs {
            opened_pcm,
            status,
            capture_stream_format: self.capture_stream_format,
            period_sample_count: self.period_sample_count,
            device_name: self.device_name.clone(),
            stop_requested: Arc::clone(&stop_requested),
            hand_off,
        })?;
        self.delivery = Some(CaptureDeliveryThread {
            stop_requested,
            reader_thread,
        });
        Ok(())
    }

    fn stop_delivering(&mut self) -> Result<()> {
        let Some(delivery) = self.delivery.take() else {
            return Ok(());
        };
        delivery.stop_requested.store(true, Ordering::Release);
        // Joining is what the trait's "the hand-off is not called again once
        // this returns" means here: the reader owns the hand-off and drops it
        // as it ends, so nothing can still be inside it afterwards.
        if delivery.reader_thread.join().is_err() {
            return Err(Error::Runtime(format!(
                "the ALSA capture reader for '{}' panicked",
                self.device_name
            )));
        }
        // SAFETY: the reader thread has been joined, so this is the only thread
        // holding the handle.
        let dropped = unsafe { (self.opened_pcm.entry_points.snd_pcm_drop)(self.opened_pcm.pcm) };
        if dropped < 0 {
            return Err(self.opened_pcm.entry_points.refuse(
                &format!("stopping capture on '{}'", self.device_name),
                dropped,
            ));
        }
        Ok(())
    }
}

impl AlsaAudioCaptureStream {
    /// Read one status off the just-started stream and refuse the device if its
    /// timestamp is not on the machine monotonic clock.
    ///
    /// Setting `SND_PCM_TSTAMP_TYPE_MONOTONIC` is a request, and a driver or an
    /// ALSA plugin is free to fill `htstamp` with something else — or with
    /// nothing. Publishing such a value would corrupt every A/V join
    /// downstream in a way nothing later can detect, so the stream refuses to
    /// run rather than lying about the epoch it stamps in.
    fn refuse_a_device_that_stamps_outside_the_monotonic_domain(
        &self,
        status: &AlsaAllocatedObject,
        started_at_ns: i64,
    ) -> Result<()> {
        let entry_points = &self.opened_pcm.entry_points;
        for _ in 0..CONSECUTIVE_SILENT_WAITS_BEFORE_GIVING_UP {
            if read_status_of_a_readable_period(
                entry_points,
                self.opened_pcm.pcm,
                status,
                &self.device_name,
            )? == PeriodReadiness::NothingYet
            {
                continue;
            }
            // SAFETY: the status was just filled by `snd_pcm_status`.
            let device_stamp_ns = unsafe { read_htimestamp_ns(entry_points, status) };
            let now_ns = monotonic_now_ns();
            if !a_device_stamp_lands_in_the_monotonic_bracket(
                device_stamp_ns,
                started_at_ns,
                now_ns,
            ) {
                return Err(Error::NotSupported(format!(
                    "ALSA device '{}' stamped its first period at {device_stamp_ns} ns, \
                     outside the CLOCK_MONOTONIC bracket [{started_at_ns}, {now_ns}] the \
                     stream ran in — it ignored SND_PCM_TSTAMP_TYPE_MONOTONIC, so its \
                     stamps cannot be joined with a frame timestamp",
                    self.device_name
                )));
            }
            return Ok(());
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

/// Everything the reader thread owns, handed over in one move.
struct CaptureReaderThreadInputs {
    opened_pcm: Arc<OpenedAlsaCapturePcm>,
    status: AlsaAllocatedObject,
    capture_stream_format: AudioCaptureStreamFormat,
    period_sample_count: u32,
    device_name: String,
    stop_requested: Arc<AtomicBool>,
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
        status,
        capture_stream_format,
        period_sample_count,
        device_name,
        stop_requested,
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

    let mut block =
        vec![0u8; capture_stream_format.interleaved_byte_count_for(period_sample_count)];
    let mut consecutive_silent_waits = 0;

    while !stop_requested.load(Ordering::Acquire) {
        match read_status_of_a_readable_period(entry_points, opened_pcm.pcm, &status, &device_name)
        {
            Ok(PeriodReadiness::Readable) => consecutive_silent_waits = 0,
            Ok(PeriodReadiness::NothingYet) => {
                consecutive_silent_waits += 1;
                if consecutive_silent_waits >= CONSECUTIVE_SILENT_WAITS_BEFORE_GIVING_UP {
                    tracing::error!(
                        device = %device_name,
                        "ALSA capture reader stopping: the device delivered nothing for \
                         {} consecutive waits",
                        consecutive_silent_waits
                    );
                    return;
                }
                continue;
            }
            Err(refusal) => {
                tracing::error!(device = %device_name, %refusal, "ALSA capture reader stopping");
                return;
            }
        }

        // SAFETY: `status` was just filled, and the out-parameter is an owned
        // local.
        let device_stamp_ns = unsafe { read_htimestamp_ns(entry_points, &status) };
        // SAFETY: `status` was just filled by `snd_pcm_status`.
        let unread_sample_count =
            unsafe { (entry_points.snd_pcm_status_get_delay)(status.pointer()) };
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
                block.as_mut_ptr().cast::<c_void>(),
                SndPcmUframes::from(period_sample_count),
            )
        };
        if read < 0 {
            let error_code = c_int::try_from(read).unwrap_or(c_int::MIN);
            if !recover_from_a_read_failure(entry_points, opened_pcm.pcm, &device_name, error_code)
            {
                return;
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
            interleaved_sample_bytes: &block
                [..capture_stream_format.interleaved_byte_count_for(sample_count)],
            sample_count,
            first_sample_timestamp_ns,
        });
    }
}

/// Whether a wait produced a whole readable period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeriodReadiness {
    Readable,
    NothingYet,
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
    status: &AlsaAllocatedObject,
    device_name: &str,
) -> Result<PeriodReadiness> {
    // SAFETY: `pcm` is an open, started capture handle held by this thread
    // alone; `snd_pcm_wait` polls it and returns without touching anything else.
    let waited = unsafe { (entry_points.snd_pcm_wait)(pcm, CAPTURE_WAIT_TIMEOUT_MS) };
    if waited == 0 {
        return Ok(PeriodReadiness::NothingYet);
    }
    if waited < 0 {
        if !recover_from_a_read_failure(entry_points, pcm, device_name, waited) {
            return Err(
                entry_points.refuse(&format!("waiting for samples from '{device_name}'"), waited)
            );
        }
        return Ok(PeriodReadiness::NothingYet);
    }

    // SAFETY: `pcm` is open and `status` is a live libasound allocation.
    let filled = unsafe { (entry_points.snd_pcm_status)(pcm, status.pointer()) };
    if filled < 0 {
        return Err(entry_points.refuse(
            &format!("reading the device status of '{device_name}'"),
            filled,
        ));
    }
    Ok(PeriodReadiness::Readable)
}

/// Ask libasound to put a broken stream back together, and say whether reading
/// may continue.
///
/// An overrun leaves a gap, and the gap is left visible rather than papered
/// over: the timestamps and sample counts of the blocks either side of it say
/// exactly how much audio went missing. Nothing is interpolated and no sample
/// is invented.
fn recover_from_a_read_failure(
    entry_points: &AlsaLibraryEntryPoints,
    pcm: *mut c_void,
    device_name: &str,
    error_code: c_int,
) -> bool {
    // SAFETY: `pcm` is an open capture handle held by this thread alone.
    let recovered = unsafe { (entry_points.snd_pcm_recover)(pcm, error_code, 1) };
    if recovered < 0 {
        tracing::error!(
            device = %device_name,
            "ALSA capture could not recover from {}: {}",
            entry_points.error_text(error_code),
            entry_points.error_text(recovered)
        );
        return false;
    }
    if error_code == ALSA_OVERRUN || error_code == ALSA_SUSPENDED {
        tracing::warn!(
            device = %device_name,
            "ALSA capture recovered from {} — the audio it covers is missing, and the gap \
             is derivable from the timestamps of the blocks either side of it",
            entry_points.error_text(error_code)
        );
    }
    true
}

/// The device's own timing for its most recent hardware pointer update, in
/// nanoseconds on the clock the software parameters demanded.
///
/// # Safety
///
/// `status` must have been filled by a successful `snd_pcm_status` call.
unsafe fn read_htimestamp_ns(
    entry_points: &AlsaLibraryEntryPoints,
    status: &AlsaAllocatedObject,
) -> i64 {
    let mut device_stamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: the caller guarantees a filled status, and the out-parameter is an
    // owned local.
    unsafe { (entry_points.snd_pcm_status_get_htstamp)(status.pointer(), &mut device_stamp) };
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

/// Whether a device's own stamp is on the machine monotonic clock.
///
/// The bracket is taken around the stream's own start, so nothing the device
/// captured can legitimately fall outside it. A `CLOCK_REALTIME` stamp misses
/// by five decades and a zeroed one — the device reporting no time at all —
/// misses by however long the machine has been up.
fn a_device_stamp_lands_in_the_monotonic_bracket(
    device_stamp_ns: i64,
    started_at_ns: i64,
    read_back_at_ns: i64,
) -> bool {
    (started_at_ns..=read_back_at_ns).contains(&device_stamp_ns)
}

fn monotonic_now_ns() -> i64 {
    MediaClock::now().as_nanos() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The whole of the arm's timestamp claim: the stamp names the *first*
    /// sample of the block, so a device holding one period of unread samples
    /// puts it a period before the status it was read from.
    ///
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

    /// The guard that keeps a wrong-epoch stamp off the data plane: setting
    /// `SND_PCM_TSTAMP_TYPE_MONOTONIC` is a request, and a driver or an ALSA
    /// plugin is free to ignore it. A published wall-clock stamp would corrupt
    /// every A/V join downstream in a way nothing later can detect.
    #[test]
    fn a_stamp_from_the_wrong_clock_is_refused_and_a_monotonic_one_is_not() {
        let started_at_ns = 541_560_000_000_000;
        let read_back_at_ns = started_at_ns + PERIOD_NANOS;
        assert!(a_device_stamp_lands_in_the_monotonic_bracket(
            started_at_ns + 1,
            started_at_ns,
            read_back_at_ns
        ));
        assert!(
            !a_device_stamp_lands_in_the_monotonic_bracket(0, started_at_ns, read_back_at_ns),
            "a zeroed htstamp is a device reporting no time at all, not time zero"
        );
        assert!(
            !a_device_stamp_lands_in_the_monotonic_bracket(
                1_787_000_000_000_000_000,
                started_at_ns,
                read_back_at_ns
            ),
            "a wall-clock stamp is five decades outside any uptime bracket"
        );
        assert!(
            !a_device_stamp_lands_in_the_monotonic_bracket(
                read_back_at_ns + 1,
                started_at_ns,
                read_back_at_ns
            ),
            "a device cannot have captured a sample after the status that reported it"
        );
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
