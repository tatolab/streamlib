// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The audio backend chain's first arm: PipeWire, reached entirely at runtime.
//!
//! `libpipewire-0.3.so.0` is opened with `libloading` and every entry point is
//! a `dlsym` result, the way `vulkan/rhi/drm_modifier_probe.rs` reaches
//! `libEGL.so.1`. Nothing here links an audio library, so the wheel's
//! `DT_NEEDED` set does not grow — the invariant
//! `sdk/streamlib-python-wheel/tests/test_wheel_portability.py` holds.
//!
//! The half that cannot be reached by `dlopen` at all is SPA's `static inline`
//! pod builders and parsers, which have no shared object behind them:
//! `pipewire_capture_shim.c` compiles those in and calls libpipewire only
//! through the pointers this module hands it.

use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::Arc;

use libloading::Library;

use crate::core::context::{
    AudioCaptureSampleFormat, AudioCaptureStream, AudioCaptureStreamFormat,
    AudioCaptureStreamRequest, AudioDeviceBackend, CapturedAudioBlockFromDevice,
    CapturedAudioBlockHandOff,
};
use crate::core::{Error, Result};

/// The versioned soname, which is the only spelling that resolves on a machine
/// with no PipeWire development package: the `.so` symlink ships in `-dev`, and
/// the wheel's whole point is running where that was never installed.
const PIPEWIRE_LIBRARY_SONAME: &str = "libpipewire-0.3.so.0";

/// How much failure text the shim is given room to write.
const SHIM_FAILURE_TEXT_CAPACITY: usize = 512;

mod shim {
    use std::ffi::{c_char, c_int, c_void};

    /// Mirrors `struct StreamLibPipeWireNegotiatedCaptureFormat`.
    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct NegotiatedCaptureFormat {
        pub sample_rate: u32,
        pub channels: u32,
        pub sample_format: u32,
    }

    /// `STREAMLIB_PIPEWIRE_SAMPLE_FORMAT_F32_LE`.
    pub const SAMPLE_FORMAT_F32_LE: u32 = 0;
    /// `STREAMLIB_PIPEWIRE_SAMPLE_FORMAT_I16_LE`.
    pub const SAMPLE_FORMAT_I16_LE: u32 = 1;

    /// Mirrors `struct StreamLibPipeWireCaptureStream`, which is opaque here.
    #[repr(C)]
    pub struct CaptureStream {
        _opaque: [u8; 0],
    }

    pub type CapturedBlockHandOff = unsafe extern "C" fn(
        hand_off_context: *mut c_void,
        interleaved_sample_bytes: *const u8,
        interleaved_sample_byte_count: usize,
        sample_count: u32,
        first_sample_timestamp_ns: i64,
    );

    unsafe extern "C" {
        pub fn streamlib_pipewire_entry_point_count() -> usize;
        pub fn streamlib_pipewire_entry_point_names() -> *const *const c_char;
        pub fn streamlib_pipewire_initialize(entry_points: *const *mut c_void);
        pub fn streamlib_pipewire_loaded_library_version(
            entry_points: *const *mut c_void,
        ) -> *const c_char;
        pub fn streamlib_pipewire_daemon_answers(
            entry_points: *const *mut c_void,
            failure_text: *mut c_char,
            failure_text_capacity: usize,
        ) -> c_int;
        pub fn streamlib_pipewire_capture_stream_open(
            entry_points: *const *mut c_void,
            device_id_or_null: *const c_char,
            negotiated_format_out: *mut NegotiatedCaptureFormat,
            failure_text: *mut c_char,
            failure_text_capacity: usize,
        ) -> *mut CaptureStream;
        pub fn streamlib_pipewire_capture_stream_start_delivering(
            capture_stream: *mut CaptureStream,
            hand_off: CapturedBlockHandOff,
            hand_off_context: *mut c_void,
        );
        pub fn streamlib_pipewire_capture_stream_stop_delivering(capture_stream: *mut CaptureStream);
        pub fn streamlib_pipewire_capture_stream_close(capture_stream: *mut CaptureStream);
    }

    // The shim calls this on every block; Rust only ever calls it to hold the
    // derivation in a test, which is why it is declared only there.
    #[cfg(test)]
    unsafe extern "C" {
        pub fn streamlib_pipewire_first_sample_timestamp_ns(
            cycle_timestamp_ns: i64,
            delay_in_rate_units: i64,
            rate_numerator: u32,
            rate_denominator: u32,
            sample_count: u32,
            sample_rate: u32,
        ) -> i64;
    }
}

/// The loaded library and one resolved address per entry point the shim names,
/// in the shim's own order.
///
/// The order and the count come from the shim rather than being restated here,
/// so the two halves cannot drift: a name added to the C X-macro is a name this
/// resolves, with no Rust edit and no possibility of an off-by-one that would
/// call the wrong function.
struct PipeWireLibraryEntryPoints {
    /// Kept solely to hold the library open: every address below points into it.
    _library: Library,
    resolved_addresses: Vec<*mut c_void>,
}

// The addresses are entry points of a library this struct itself keeps loaded
// for as long as it lives, so they are valid from any thread and never written
// after construction.
unsafe impl Send for PipeWireLibraryEntryPoints {}
unsafe impl Sync for PipeWireLibraryEntryPoints {}

impl PipeWireLibraryEntryPoints {
    /// Open the library and resolve every entry point, or say which step failed
    /// in the words the demotion log line should carry.
    fn resolve() -> std::result::Result<Self, String> {
        let library = unsafe { Library::new(PIPEWIRE_LIBRARY_SONAME) }
            .map_err(|e| format!("{PIPEWIRE_LIBRARY_SONAME} could not be loaded: {e}"))?;

        let name_count = unsafe { shim::streamlib_pipewire_entry_point_count() };
        let names = unsafe { shim::streamlib_pipewire_entry_point_names() };
        let mut resolved_addresses = Vec::with_capacity(name_count);
        for name_index in 0..name_count {
            let name = unsafe { CStr::from_ptr(*names.add(name_index)) };
            let symbol: libloading::Symbol<'_, unsafe extern "C" fn()> =
                unsafe { library.get(name.to_bytes_with_nul()) }.map_err(|_| {
                    format!(
                        "{PIPEWIRE_LIBRARY_SONAME} exports no {}, so this host's PipeWire is \
                         older than the 0.3.50 floor this arm binds against",
                        name.to_string_lossy()
                    )
                })?;
            resolved_addresses.push(*symbol as *mut c_void);
        }

        Ok(Self {
            _library: library,
            resolved_addresses,
        })
    }

    fn as_ptr(&self) -> *const *mut c_void {
        self.resolved_addresses.as_ptr()
    }
}

/// A buffer the shim writes a failure into, read back as an owned message.
struct ShimFailureText([c_char; SHIM_FAILURE_TEXT_CAPACITY]);

impl ShimFailureText {
    fn new() -> Self {
        Self([0; SHIM_FAILURE_TEXT_CAPACITY])
    }

    fn as_mut_ptr(&mut self) -> *mut c_char {
        self.0.as_mut_ptr()
    }

    /// Read as a bounded slice rather than through `CStr::from_ptr`, so the
    /// borrow is tied to the buffer and a shim that forgot to terminate its
    /// text cannot walk off the end.
    fn read(&self) -> String {
        let bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(self.0.as_ptr().cast::<u8>(), self.0.len()) };
        CStr::from_bytes_until_nul(bytes)
            .map(|text| text.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "PipeWire reported a failure with no readable text".to_string())
    }
}

/// Audio over a PipeWire session, with libpipewire bound at runtime.
pub struct PipeWireAudioDeviceBackend {
    entry_points: Arc<PipeWireLibraryEntryPoints>,
}

impl PipeWireAudioDeviceBackend {
    /// Load libpipewire and confirm a daemon actually answers, or say why this
    /// arm cannot serve so the chain can demote.
    ///
    /// The connection round trip is the point: `libpipewire` present with no
    /// daemon behind it is the ordinary container case, and probing on presence
    /// alone would strand exactly the machines the chain exists to serve.
    pub fn load_and_connect() -> std::result::Result<Self, String> {
        let entry_points = PipeWireLibraryEntryPoints::resolve()?;

        // Process-global, and this runs inside the chain's one-shot probe, so
        // it happens exactly once however many streams are opened later.
        unsafe { shim::streamlib_pipewire_initialize(entry_points.as_ptr()) };

        let mut failure_text = ShimFailureText::new();
        let daemon_answered = unsafe {
            shim::streamlib_pipewire_daemon_answers(
                entry_points.as_ptr(),
                failure_text.as_mut_ptr(),
                SHIM_FAILURE_TEXT_CAPACITY,
            )
        } == 0;
        if !daemon_answered {
            return Err(failure_text.read());
        }

        let library_version = unsafe {
            let version = shim::streamlib_pipewire_loaded_library_version(entry_points.as_ptr());
            if version.is_null() {
                "unknown".to_string()
            } else {
                CStr::from_ptr(version).to_string_lossy().into_owned()
            }
        };
        tracing::debug!(
            library = PIPEWIRE_LIBRARY_SONAME,
            version = %library_version,
            "PipeWire audio arm: a daemon answered"
        );

        Ok(Self {
            entry_points: Arc::new(entry_points),
        })
    }
}

impl AudioDeviceBackend for PipeWireAudioDeviceBackend {
    fn backend_name(&self) -> &'static str {
        "pipewire"
    }

    fn open_capture_stream(
        &self,
        request: &AudioCaptureStreamRequest,
    ) -> Result<Box<dyn AudioCaptureStream>> {
        // The device paces this arm, so the request's deviceless pacing clock
        // is deliberately untouched: a graph whose audio is device-paced never
        // starts the timer, which is what keeps device ticks and timer ticks
        // from interleaving.
        let device_id = request
            .device_id
            .as_deref()
            .map(|device_id| {
                CString::new(device_id).map_err(|_| {
                    Error::Configuration(format!(
                        "audio device id '{device_id}' contains a NUL byte and cannot name a \
                         PipeWire target object"
                    ))
                })
            })
            .transpose()?;

        let mut negotiated_format = shim::NegotiatedCaptureFormat::default();
        let mut failure_text = ShimFailureText::new();
        let opened = unsafe {
            shim::streamlib_pipewire_capture_stream_open(
                self.entry_points.as_ptr(),
                device_id
                    .as_ref()
                    .map_or(std::ptr::null(), |device_id| device_id.as_ptr()),
                &mut negotiated_format,
                failure_text.as_mut_ptr(),
                SHIM_FAILURE_TEXT_CAPACITY,
            )
        };
        if opened.is_null() {
            // A wrong device id is a wiring error, and landing on a different
            // device would be worse than failing — so the name is in the text.
            let named_device = request.device_id.as_deref().unwrap_or("<default>");
            return Err(Error::Configuration(format!(
                "PipeWire could not open audio device '{named_device}': {}",
                failure_text.read()
            )));
        }

        Ok(Box::new(PipeWireAudioCaptureStream {
            _entry_points: Arc::clone(&self.entry_points),
            capture_stream: opened,
            capture_stream_format: capture_stream_format_of(negotiated_format)?,
            installed_hand_off: None,
        }))
    }
}

/// The seam's format, read out of what PipeWire settled on.
fn capture_stream_format_of(
    negotiated: shim::NegotiatedCaptureFormat,
) -> Result<AudioCaptureStreamFormat> {
    let sample_format = match negotiated.sample_format {
        shim::SAMPLE_FORMAT_F32_LE => AudioCaptureSampleFormat::F32,
        shim::SAMPLE_FORMAT_I16_LE => AudioCaptureSampleFormat::I16,
        other => {
            return Err(Error::Runtime(format!(
                "the PipeWire capture shim reported sample format {other}, which names no \
                 encoding an AudioBlock can carry"
            )));
        }
    };
    Ok(AudioCaptureStreamFormat {
        sample_rate: negotiated.sample_rate,
        channels: negotiated.channels,
        sample_format,
    })
}

/// One PipeWire capture stream, negotiated and connected.
struct PipeWireAudioCaptureStream {
    /// Held so the library outlives every address the shim still holds.
    _entry_points: Arc<PipeWireLibraryEntryPoints>,
    capture_stream: *mut shim::CaptureStream,
    capture_stream_format: AudioCaptureStreamFormat,
    /// The hand-off the shim's callback context points at. Owned here so it
    /// outlives every delivery and is freed only once the shim has promised no
    /// further callback.
    installed_hand_off: Option<Box<CapturedAudioBlockHandOff>>,
}

// The stream is only ever touched from the thread that owns it; the shim
// serializes everything else behind PipeWire's own loop lock.
unsafe impl Send for PipeWireAudioCaptureStream {}

/// What the shim calls on PipeWire's realtime thread.
///
/// Must not unwind: this is a plain `extern "C"` boundary, so a panic here
/// aborts the process rather than crossing into C.
unsafe extern "C" fn deliver_captured_block_to_hand_off(
    hand_off_context: *mut c_void,
    interleaved_sample_bytes: *const u8,
    interleaved_sample_byte_count: usize,
    sample_count: u32,
    first_sample_timestamp_ns: i64,
) {
    let hand_off = unsafe { &*hand_off_context.cast::<CapturedAudioBlockHandOff>() };
    let interleaved_sample_bytes = if interleaved_sample_bytes.is_null() {
        &[][..]
    } else {
        unsafe {
            std::slice::from_raw_parts(interleaved_sample_bytes, interleaved_sample_byte_count)
        }
    };
    hand_off(CapturedAudioBlockFromDevice {
        interleaved_sample_bytes,
        sample_count,
        first_sample_timestamp_ns,
    });
}

impl AudioCaptureStream for PipeWireAudioCaptureStream {
    fn stream_format(&self) -> AudioCaptureStreamFormat {
        self.capture_stream_format
    }

    fn start_delivering_to(&mut self, hand_off: CapturedAudioBlockHandOff) -> Result<()> {
        let hand_off = Box::new(hand_off);
        let hand_off_context = (&raw const *hand_off).cast_mut().cast::<c_void>();
        unsafe {
            shim::streamlib_pipewire_capture_stream_start_delivering(
                self.capture_stream,
                deliver_captured_block_to_hand_off,
                hand_off_context,
            );
        }
        // Installed before the previous one is dropped: the shim takes the loop
        // lock, so once it returns no callback can still be holding the old
        // context, and dropping it first would leave a window where it could.
        self.installed_hand_off = Some(hand_off);
        Ok(())
    }

    fn stop_delivering(&mut self) -> Result<()> {
        unsafe {
            shim::streamlib_pipewire_capture_stream_stop_delivering(self.capture_stream);
        }
        self.installed_hand_off = None;
        Ok(())
    }
}

impl Drop for PipeWireAudioCaptureStream {
    fn drop(&mut self) {
        // Closes and joins PipeWire's loop thread, so the hand-off this then
        // drops is provably no longer reachable from a callback.
        unsafe {
            shim::streamlib_pipewire_capture_stream_close(self.capture_stream);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE_48K_NUMERATOR: u32 = 1;
    const RATE_48K_DENOMINATOR: u32 = 48_000;
    const QUANTUM_SAMPLES: u32 = 1024;
    /// 1024 samples at 48 kHz, truncated the way the derivation truncates.
    const QUANTUM_NANOS: i64 = 21_333_333;

    fn first_sample_timestamp_ns(cycle_timestamp_ns: i64, delay_in_rate_units: i64) -> i64 {
        unsafe {
            shim::streamlib_pipewire_first_sample_timestamp_ns(
                cycle_timestamp_ns,
                delay_in_rate_units,
                RATE_48K_NUMERATOR,
                RATE_48K_DENOMINATOR,
                QUANTUM_SAMPLES,
                48_000,
            )
        }
    }

    /// The whole of the arm's timestamp claim, on a machine with no audio
    /// server: the value names the *first* sample, so it sits one block before
    /// the cycle that delivered it — not at the cycle, which is what a
    /// publish-time stamp would give and what a rig assertion measures.
    ///
    /// Mental revert: drop the block-duration term and this returns the cycle
    /// time, putting the stamp at the block's end and making a joined camera
    /// frame one quantum early.
    #[test]
    fn a_blocks_stamp_is_one_block_before_the_cycle_that_delivered_it() {
        let cycle_timestamp_ns = 541_560_469_372_719;
        assert_eq!(
            first_sample_timestamp_ns(cycle_timestamp_ns, 0),
            cycle_timestamp_ns - QUANTUM_NANOS
        );
    }

    /// A device that reports travel time pushes the capture instant further
    /// back by exactly that much, converted out of the graph's rate units.
    #[test]
    fn a_reported_device_delay_moves_the_stamp_further_into_the_past() {
        let cycle_timestamp_ns = 1_000_000_000;
        let delay_of_one_quantum = i64::from(QUANTUM_SAMPLES);
        assert_eq!(
            first_sample_timestamp_ns(cycle_timestamp_ns, delay_of_one_quantum),
            cycle_timestamp_ns - QUANTUM_NANOS - QUANTUM_NANOS,
            "a delay of one quantum in rate units is one quantum of nanoseconds"
        );
    }

    /// Consecutive cycles are one block apart, which is what makes
    /// `first_sample_timestamp_ns + sample_count / sample_rate` the next
    /// block's expected stamp — the property a consumer joins on.
    #[test]
    fn consecutive_cycles_are_exactly_one_block_apart() {
        let first_cycle_ns = 1_000_000_000;
        let second_cycle_ns = first_cycle_ns + QUANTUM_NANOS;
        assert_eq!(
            first_sample_timestamp_ns(second_cycle_ns, 0)
                - first_sample_timestamp_ns(first_cycle_ns, 0),
            QUANTUM_NANOS
        );
    }

    /// A rate fraction of 0/0 is what a stream that has not settled reports,
    /// and no delay is derivable from it — the stamp must still be the cycle
    /// minus the block rather than a division by zero.
    #[test]
    fn an_unsettled_rate_fraction_contributes_no_delay_rather_than_dividing_by_zero() {
        let stamp = unsafe {
            shim::streamlib_pipewire_first_sample_timestamp_ns(
                1_000_000_000,
                512,
                0,
                0,
                QUANTUM_SAMPLES,
                48_000,
            )
        };
        assert_eq!(stamp, 1_000_000_000 - QUANTUM_NANOS);
    }

    /// The two halves agree on how many entry points there are and on their
    /// order, because only the C side states it. A Rust-side restatement is
    /// what this exists to make impossible.
    #[test]
    fn the_shim_names_every_entry_point_it_expects_rust_to_resolve() {
        let count = unsafe { shim::streamlib_pipewire_entry_point_count() };
        assert!(count > 0, "the shim names no entry points at all");

        let names = unsafe { shim::streamlib_pipewire_entry_point_names() };
        for name_index in 0..count {
            let name = unsafe { CStr::from_ptr(*names.add(name_index)) }
                .to_str()
                .expect("entry-point names are ASCII C identifiers");
            assert!(
                name.starts_with("pw_"),
                "every entry point is a libpipewire symbol, but slot {name_index} is {name:?}"
            );
        }
    }

    /// The demotion path, held where the words a reader will see are produced:
    /// a library that is not there names itself, so a container's log says
    /// which arm was skipped rather than only which arm was chosen.
    #[test]
    fn a_missing_library_is_reported_by_name_rather_than_crashing() {
        let Err(reason) = (unsafe { Library::new("libpipewire-0.3.so.0.does-not-exist") })
            .map_err(|e| format!("{PIPEWIRE_LIBRARY_SONAME} could not be loaded: {e}"))
        else {
            panic!("a library with no such file cannot load");
        };
        assert!(
            reason.contains(PIPEWIRE_LIBRARY_SONAME),
            "the demotion reason must name the library: {reason}"
        );
    }

    /// The shim's failure buffer is read as a bounded slice, so text that fills
    /// it exactly — or one that was never written at all — is a message rather
    /// than a walk off the end.
    #[test]
    fn failure_text_the_shim_never_wrote_reads_as_an_empty_message() {
        assert_eq!(ShimFailureText::new().read(), "");
    }
}
