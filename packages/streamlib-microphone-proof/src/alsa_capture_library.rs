// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The handful of `libasound` entry points a capture needs, resolved at runtime.

use std::ffi::{CStr, CString, c_char, c_int, c_long, c_uint, c_ulong, c_void};
use std::sync::Arc;

use libloading::Library;

/// The versioned soname; the bare `.so` ships only in `libasound2-dev`.
const ALSA_LIBRARY_SONAME: &str = "libasound.so.2";

/// `SND_PCM_STREAM_CAPTURE`.
const SND_PCM_STREAM_CAPTURE: c_int = 1;
/// Blocking open: the capture thread bounds its own waits with `snd_pcm_wait`.
const SND_PCM_OPEN_MODE_BLOCKING: c_int = 0;
/// `SND_PCM_FORMAT_FLOAT_LE`, the `f32` the wire's default dtype names.
const SND_PCM_FORMAT_FLOAT_LE: c_int = 14;
/// `SND_PCM_ACCESS_RW_INTERLEAVED`, the layout the wire states.
const SND_PCM_ACCESS_RW_INTERLEAVED: c_int = 3;
/// Let alsa-lib resample when the device cannot run at the requested rate.
const ALLOW_SOFTWARE_RESAMPLING: c_int = 1;
/// `snd_pcm_recover` without printing to stderr.
const RECOVER_SILENTLY: c_int = 1;

/// `snd_pcm_uframes_t`.
type SndPcmUframes = c_ulong;
/// `snd_pcm_sframes_t`.
type SndPcmSframes = c_long;

/// `-EPIPE`: the device overran a capture stream.
pub(crate) const ALSA_BROKEN_PIPE: c_int = -32;

struct AlsaEntryPoints {
    snd_pcm_open: unsafe extern "C" fn(*mut *mut c_void, *const c_char, c_int, c_int) -> c_int,
    snd_pcm_set_params:
        unsafe extern "C" fn(*mut c_void, c_int, c_int, c_uint, c_uint, c_int, c_uint) -> c_int,
    snd_pcm_get_params:
        unsafe extern "C" fn(*mut c_void, *mut SndPcmUframes, *mut SndPcmUframes) -> c_int,
    snd_pcm_start: unsafe extern "C" fn(*mut c_void) -> c_int,
    snd_pcm_wait: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    snd_pcm_readi: unsafe extern "C" fn(*mut c_void, *mut c_void, SndPcmUframes) -> SndPcmSframes,
    snd_pcm_delay: unsafe extern "C" fn(*mut c_void, *mut SndPcmSframes) -> c_int,
    snd_pcm_recover: unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int,
    snd_pcm_close: unsafe extern "C" fn(*mut c_void) -> c_int,
    snd_strerror: unsafe extern "C" fn(c_int) -> *const c_char,
}

/// `libasound`, opened once, with the entry points resolved out of it.
pub(crate) struct AlsaCaptureLibrary {
    entry_points: AlsaEntryPoints,
    _library: Library,
}

/// What a capture PCM was opened at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpenedCapturePcmFormat {
    pub(crate) sample_rate: u32,
    pub(crate) channels: u32,
    pub(crate) buffer_frames: u64,
    pub(crate) period_frames: u64,
}

/// An open capture PCM; closed on drop. Used from one thread at a time.
pub(crate) struct OpenedCapturePcm {
    library: Arc<AlsaCaptureLibrary>,
    pcm: *mut c_void,
}

// SAFETY: an ALSA PCM handle may move between threads as long as only one uses it at a
// time, which `OpenedCapturePcm`'s `&mut self` methods and lack of `Sync` guarantee.
unsafe impl Send for OpenedCapturePcm {}

/// What one bounded wait for a period came back with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapturePeriodWait {
    Ready,
    TimedOut,
    Failed(c_int),
}

impl AlsaCaptureLibrary {
    pub(crate) fn load() -> Result<Self, String> {
        // SAFETY: loading libasound runs its constructors, which have no preconditions.
        let library = unsafe { Library::new(ALSA_LIBRARY_SONAME) }
            .map_err(|error| format!("could not open {ALSA_LIBRARY_SONAME}: {error}"))?;
        macro_rules! resolve {
            ($name:ident) => {
                // SAFETY: the symbol's type is its `<alsa/pcm.h>` prototype.
                *unsafe { library.get(concat!(stringify!($name), "\0").as_bytes()) }.map_err(
                    |error| {
                        format!(
                            "{ALSA_LIBRARY_SONAME} has no {}: {error}",
                            stringify!($name)
                        )
                    },
                )?
            };
        }
        let entry_points = AlsaEntryPoints {
            snd_pcm_open: resolve!(snd_pcm_open),
            snd_pcm_set_params: resolve!(snd_pcm_set_params),
            snd_pcm_get_params: resolve!(snd_pcm_get_params),
            snd_pcm_start: resolve!(snd_pcm_start),
            snd_pcm_wait: resolve!(snd_pcm_wait),
            snd_pcm_readi: resolve!(snd_pcm_readi),
            snd_pcm_delay: resolve!(snd_pcm_delay),
            snd_pcm_recover: resolve!(snd_pcm_recover),
            snd_pcm_close: resolve!(snd_pcm_close),
            snd_strerror: resolve!(snd_strerror),
        };
        Ok(Self {
            entry_points,
            _library: library,
        })
    }

    pub(crate) fn error_text(&self, error_code: c_int) -> String {
        // SAFETY: `snd_strerror` returns a static string for any code.
        let text = unsafe { (self.entry_points.snd_strerror)(error_code) };
        if text.is_null() {
            return format!("ALSA error {error_code}");
        }
        // SAFETY: non-null, NUL-terminated, static.
        let text = unsafe { CStr::from_ptr(text) }.to_string_lossy();
        format!("{text} ({error_code})")
    }

    /// Open `pcm_name` for interleaved `f32` capture at `sample_rate` and `channels`, with
    /// a buffer of about `buffer_latency_us`.
    pub(crate) fn open_capture_pcm(
        self: &Arc<Self>,
        pcm_name: &str,
        sample_rate: u32,
        channels: u32,
        buffer_latency_us: u32,
    ) -> Result<(OpenedCapturePcm, OpenedCapturePcmFormat), String> {
        let pcm_name_c = CString::new(pcm_name)
            .map_err(|_| format!("PCM name {pcm_name:?} contains a NUL byte"))?;
        let mut pcm: *mut c_void = std::ptr::null_mut();
        // SAFETY: `pcm` is a valid out-slot and the name is NUL-terminated.
        let opened = unsafe {
            (self.entry_points.snd_pcm_open)(
                &mut pcm,
                pcm_name_c.as_ptr(),
                SND_PCM_STREAM_CAPTURE,
                SND_PCM_OPEN_MODE_BLOCKING,
            )
        };
        if opened < 0 {
            return Err(format!(
                "snd_pcm_open({pcm_name:?}) refused: {}",
                self.error_text(opened)
            ));
        }
        let opened_pcm = OpenedCapturePcm {
            library: Arc::clone(self),
            pcm,
        };
        // SAFETY: `pcm` was just opened.
        let configured = unsafe {
            (self.entry_points.snd_pcm_set_params)(
                pcm,
                SND_PCM_FORMAT_FLOAT_LE,
                SND_PCM_ACCESS_RW_INTERLEAVED,
                channels,
                sample_rate,
                ALLOW_SOFTWARE_RESAMPLING,
                buffer_latency_us,
            )
        };
        if configured < 0 {
            return Err(format!(
                "snd_pcm_set_params({pcm_name:?}, f32, {channels} ch, {sample_rate} Hz) refused: {}",
                self.error_text(configured)
            ));
        }
        let mut buffer_frames: SndPcmUframes = 0;
        let mut period_frames: SndPcmUframes = 0;
        // SAFETY: `pcm` is configured and both out-slots are valid.
        let read_back = unsafe {
            (self.entry_points.snd_pcm_get_params)(pcm, &mut buffer_frames, &mut period_frames)
        };
        if read_back < 0 {
            return Err(format!(
                "snd_pcm_get_params({pcm_name:?}) refused: {}",
                self.error_text(read_back)
            ));
        }
        Ok((
            opened_pcm,
            OpenedCapturePcmFormat {
                sample_rate,
                channels,
                buffer_frames,
                period_frames,
            },
        ))
    }
}

impl OpenedCapturePcm {
    pub(crate) fn start(&mut self) -> Result<(), c_int> {
        // SAFETY: the PCM is open and configured.
        let started = unsafe { (self.library.entry_points.snd_pcm_start)(self.pcm) };
        if started < 0 { Err(started) } else { Ok(()) }
    }

    pub(crate) fn wait_for_a_period(&mut self, timeout_ms: i32) -> CapturePeriodWait {
        // SAFETY: the PCM is open.
        match unsafe { (self.library.entry_points.snd_pcm_wait)(self.pcm, timeout_ms) } {
            0 => CapturePeriodWait::TimedOut,
            ready if ready > 0 => CapturePeriodWait::Ready,
            failed => CapturePeriodWait::Failed(failed),
        }
    }

    /// Read up to `frames` interleaved frames into `destination`; frames read, or the
    /// ALSA error.
    pub(crate) fn read_interleaved(
        &mut self,
        destination: &mut [u8],
        frames: u32,
        bytes_per_frame: usize,
    ) -> Result<u32, c_int> {
        assert!(destination.len() >= frames as usize * bytes_per_frame);
        // SAFETY: `destination` holds room for `frames` frames, checked above.
        let read = unsafe {
            (self.library.entry_points.snd_pcm_readi)(
                self.pcm,
                destination.as_mut_ptr().cast::<c_void>(),
                SndPcmUframes::from(frames),
            )
        };
        if read < 0 {
            return Err(c_int::try_from(read).unwrap_or(c_int::MIN));
        }
        Ok(u32::try_from(read).unwrap_or(0))
    }

    /// Frames captured by the device and not yet read, or zero when it will not say.
    pub(crate) fn frames_waiting_to_be_read(&mut self) -> i64 {
        let mut delay: SndPcmSframes = 0;
        // SAFETY: the PCM is open and `delay` is a valid out-slot.
        let answered = unsafe { (self.library.entry_points.snd_pcm_delay)(self.pcm, &mut delay) };
        if answered < 0 { 0 } else { delay.max(0) }
    }

    /// Put an overrun or suspended stream back together; the error if it cannot be.
    pub(crate) fn recover(&mut self, error_code: c_int) -> Result<(), c_int> {
        // SAFETY: the PCM is open.
        let recovered = unsafe {
            (self.library.entry_points.snd_pcm_recover)(self.pcm, error_code, RECOVER_SILENTLY)
        };
        if recovered < 0 {
            Err(recovered)
        } else {
            Ok(())
        }
    }

    pub(crate) fn library(&self) -> &AlsaCaptureLibrary {
        &self.library
    }
}

impl Drop for OpenedCapturePcm {
    fn drop(&mut self) {
        // SAFETY: the PCM is open, and nothing uses it after this.
        unsafe { (self.library.entry_points.snd_pcm_close)(self.pcm) };
    }
}
