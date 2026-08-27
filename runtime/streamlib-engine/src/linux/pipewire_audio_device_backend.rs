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

mod capture_shim {
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
        pub fn streamlib_pipewire_capture_stream_stop_delivering(
            capture_stream: *mut CaptureStream,
        );
        pub fn streamlib_pipewire_capture_stream_close(capture_stream: *mut CaptureStream);
    }

    /// Mirrors `struct StreamLibPipeWireChunkExtent`.
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[cfg(test)]
    pub struct ChunkExtent {
        pub offset: u32,
        pub byte_count: u32,
    }

    // The shim calls these on every block; Rust only ever calls them to hold
    // the arithmetic in a test, which is why they are declared only there.
    #[cfg(test)]
    unsafe extern "C" {
        pub fn streamlib_pipewire_sink_name_length_of_monitor_device_id(
            device_id: *const c_char,
        ) -> usize;
        pub fn streamlib_pipewire_clamped_chunk_extent(
            chunk_offset: u32,
            chunk_size: u32,
            data_maxsize: u32,
        ) -> ChunkExtent;
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
    fn resolve() -> std::result::Result<Self, PipeWireArmUnavailableReason> {
        Self::resolve_from(PIPEWIRE_LIBRARY_SONAME)
    }

    /// The soname is a parameter so that both refusals — the library is not
    /// there, and the library is there but exports none of this — are reachable
    /// from a test without a PipeWire-shaped stub on disk.
    fn resolve_from(
        library_soname: &str,
    ) -> std::result::Result<Self, PipeWireArmUnavailableReason> {
        // SAFETY: `dlopen` of a soname. Loading an audio library can run its
        // initialisers, which is what the chain's probe-by-opening accepts;
        // nothing here dereferences anything until `get` succeeds.
        let library = unsafe { Library::new(library_soname) }.map_err(|e| {
            PipeWireArmUnavailableReason(format!("{library_soname} could not be loaded: {e}"))
        })?;

        // SAFETY: both return pointers into the shim's own `static const`
        // storage, so the array and every name in it are `'static` and
        // immutable.
        let name_count = unsafe { capture_shim::streamlib_pipewire_entry_point_count() };
        let names = unsafe { capture_shim::streamlib_pipewire_entry_point_names() };
        let mut resolved_addresses = Vec::with_capacity(name_count);
        for name_index in 0..name_count {
            // SAFETY: `name_index < name_count`, and the shim guarantees that
            // many NUL-terminated names at that address.
            let name = unsafe { CStr::from_ptr(*names.add(name_index)) };
            // SAFETY: `dlsym` with a NUL-terminated name; the resulting address
            // is kept alive by the `Library` this struct stores beside it.
            let symbol: libloading::Symbol<'_, unsafe extern "C" fn()> =
                unsafe { library.get(name.to_bytes_with_nul()) }.map_err(|_| {
                    PipeWireArmUnavailableReason(format!(
                        "{library_soname} exports no {}, so this host's PipeWire is older \
                         than the 0.3.50 floor this arm binds against",
                        name.to_string_lossy()
                    ))
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
///
/// Held as bytes rather than `c_char` so reading it needs no `unsafe` and does
/// not depend on `c_char`'s platform signedness; the cast happens once, at the
/// boundary that hands the pointer over.
struct ShimFailureText([u8; SHIM_FAILURE_TEXT_CAPACITY]);

impl ShimFailureText {
    fn new() -> Self {
        Self([0; SHIM_FAILURE_TEXT_CAPACITY])
    }

    /// The pointer and the capacity together, so the two cannot be passed to
    /// the shim disagreeing about the same buffer.
    fn as_shim_out_parameters(&mut self) -> (*mut c_char, usize) {
        (self.0.as_mut_ptr().cast::<c_char>(), self.0.len())
    }

    /// Read as a bounded slice rather than through `CStr::from_ptr`, so a shim
    /// that forgot to terminate its text cannot walk off the end.
    fn read(&self) -> String {
        CStr::from_bytes_until_nul(&self.0)
            .map(|text| text.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "PipeWire reported a failure with no readable text".to_string())
    }
}

/// Why the PipeWire arm cannot serve, in the words the demotion log line
/// carries.
///
/// Not a core `Error`: nothing failed that a caller must handle. The chain has
/// another arm, and this is what a reader needs in order to tell "the library
/// was absent" from "no daemon answered".
pub struct PipeWireArmUnavailableReason(String);

impl std::fmt::Display for PipeWireArmUnavailableReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
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
    pub fn load_and_connect() -> std::result::Result<Self, PipeWireArmUnavailableReason> {
        let entry_points = PipeWireLibraryEntryPoints::resolve()?;

        // Process-global, and this runs inside the chain's one-shot probe, so
        // it happens exactly once however many streams are opened later.
        // SAFETY: the table is fully resolved and outlives this call.
        unsafe { capture_shim::streamlib_pipewire_initialize(entry_points.as_ptr()) };

        let mut failure_text = ShimFailureText::new();
        let (failure_text_ptr, failure_text_capacity) = failure_text.as_shim_out_parameters();
        // SAFETY: the table is fully resolved, and the out-buffer's pointer and
        // capacity come from the buffer itself, which outlives the call.
        let daemon_answered = unsafe {
            capture_shim::streamlib_pipewire_daemon_answers(
                entry_points.as_ptr(),
                failure_text_ptr,
                failure_text_capacity,
            )
        } == 0;
        if !daemon_answered {
            return Err(PipeWireArmUnavailableReason(failure_text.read()));
        }

        // SAFETY: `pw_get_library_version` returns a pointer to libpipewire's
        // own static version string, valid for as long as the library is loaded.
        let library_version = unsafe {
            let version =
                capture_shim::streamlib_pipewire_loaded_library_version(entry_points.as_ptr());
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

        let mut negotiated_format = capture_shim::NegotiatedCaptureFormat::default();
        let mut failure_text = ShimFailureText::new();
        let (failure_text_ptr, failure_text_capacity) = failure_text.as_shim_out_parameters();
        // SAFETY: the device id, if any, is a live `CString` for the length of
        // this call; the format and failure out-parameters are owned locals.
        let opened = unsafe {
            capture_shim::streamlib_pipewire_capture_stream_open(
                self.entry_points.as_ptr(),
                device_id
                    .as_ref()
                    .map_or(std::ptr::null(), |device_id| device_id.as_ptr()),
                &mut negotiated_format,
                failure_text_ptr,
                failure_text_capacity,
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
    negotiated: capture_shim::NegotiatedCaptureFormat,
) -> Result<AudioCaptureStreamFormat> {
    let sample_format = match negotiated.sample_format {
        capture_shim::SAMPLE_FORMAT_F32_LE => AudioCaptureSampleFormat::F32,
        capture_shim::SAMPLE_FORMAT_I16_LE => AudioCaptureSampleFormat::I16,
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
    capture_stream: *mut capture_shim::CaptureStream,
    capture_stream_format: AudioCaptureStreamFormat,
    /// The hand-off the shim's callback context points at. Owned here so it
    /// outlives every delivery and is freed only once the shim has promised no
    /// further callback.
    installed_hand_off: Option<Box<CapturedAudioBlockHandOff>>,
}

// The pointer is exclusively owned by this struct — nothing else holds a copy,
// and every shim entry point it is passed to takes PipeWire's thread-loop lock
// before touching anything the loop thread also touches. `pw_thread_loop` is a
// use-from-any-thread API, so moving that ownership between threads is sound.
// `Sync` is deliberately not implemented: two `&` references could call
// `start_delivering` concurrently.
unsafe impl Send for PipeWireAudioCaptureStream {}

/// What the shim calls on PipeWire's thread-loop thread, with that loop's lock
/// held.
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
    // SAFETY: the context is the address of the `Box<CapturedAudioBlockHandOff>`
    // that `start_delivering_to` installed under the loop lock, and
    // `stop_delivering` retires it under that same lock before dropping it — so
    // it is live for the length of this call.
    let hand_off = unsafe { &*hand_off_context.cast::<CapturedAudioBlockHandOff>() };
    let interleaved_sample_bytes = if interleaved_sample_bytes.is_null() {
        &[][..]
    } else {
        // SAFETY: the shim clamps the daemon's chunk offset and size to the
        // buffer's own `maxsize` before handing this pair over, so the range
        // lies inside PipeWire's mapping, which stays valid until this returns.
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
        // Boxed so C gets a thin, stable address for what is otherwise a fat
        // pointer. The allocation does not move when the `Box` itself does.
        let hand_off = Box::new(hand_off);
        let hand_off_context = (&raw const *hand_off).cast_mut().cast::<c_void>();
        // SAFETY: the stream pointer is live, and the context stays valid
        // because the `Box` is stored on `self` on the next line.
        unsafe {
            capture_shim::streamlib_pipewire_capture_stream_start_delivering(
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
        // SAFETY: the stream pointer is live. The shim takes the loop lock, so
        // when it returns no callback can still be reading the context that the
        // next line drops.
        unsafe {
            capture_shim::streamlib_pipewire_capture_stream_stop_delivering(self.capture_stream);
        }
        self.installed_hand_off = None;
        Ok(())
    }
}

impl Drop for PipeWireAudioCaptureStream {
    fn drop(&mut self) {
        // Closes and joins PipeWire's loop thread, so the hand-off this then
        // drops is provably no longer reachable from a callback.
        // SAFETY: the pointer came from the shim's own `open` and is closed
        // exactly once, here.
        unsafe {
            capture_shim::streamlib_pipewire_capture_stream_close(self.capture_stream);
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
            capture_shim::streamlib_pipewire_first_sample_timestamp_ns(
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
            capture_shim::streamlib_pipewire_first_sample_timestamp_ns(
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

    fn monitored_sink_name_of(device_id: &str) -> Option<String> {
        let device_id = CString::new(device_id).expect("a test device id has no NUL");
        let length = unsafe {
            capture_shim::streamlib_pipewire_sink_name_length_of_monitor_device_id(
                device_id.as_ptr(),
            )
        };
        (length > 0).then(|| device_id.to_string_lossy()[..length].to_string())
    }

    /// The whole of the monitor convention: a `<sink>.monitor` device id names
    /// the sink, and the shim then asks PipeWire for that sink's monitor rather
    /// than for a source of that name.
    ///
    /// Mental revert: return 0 here and the stream targets a sink with no
    /// `stream.capture.sink`, which PipeWire answers by attaching to the
    /// session's default source — silence that looks like a working pipeline.
    #[test]
    fn a_monitor_device_id_names_the_sink_it_monitors() {
        assert_eq!(
            monitored_sink_name_of("streamlib-fixture-audio-sink.monitor").as_deref(),
            Some("streamlib-fixture-audio-sink")
        );
    }

    /// An ordinary source is captured as itself, so the convention cannot
    /// hijack a device that merely has "monitor" in its name.
    #[test]
    fn a_plain_device_id_is_not_read_as_a_monitor() {
        assert_eq!(monitored_sink_name_of("alsa_input.pci-0000_00_1f.3"), None);
        assert_eq!(monitored_sink_name_of("my.monitor.device"), None);
        assert_eq!(monitored_sink_name_of(""), None);
    }

    /// `.monitor` alone names no sink, so it stays an ordinary target and fails
    /// as the missing device it is rather than resolving to something.
    #[test]
    fn the_suffix_alone_names_no_sink() {
        assert_eq!(monitored_sink_name_of(".monitor"), None);
    }

    fn clamped_chunk_extent(offset: u32, size: u32, maxsize: u32) -> capture_shim::ChunkExtent {
        unsafe { capture_shim::streamlib_pipewire_clamped_chunk_extent(offset, size, maxsize) }
    }

    /// The ordinary case: a chunk that sits inside its buffer is passed through
    /// untouched.
    #[test]
    fn a_chunk_within_its_buffer_is_left_alone() {
        assert_eq!(
            clamped_chunk_extent(0, 8192, 65536),
            capture_shim::ChunkExtent {
                offset: 0,
                byte_count: 8192
            }
        );
    }

    /// `spa_chunk` states the offset is taken modulo `maxsize`, so a wrapped
    /// offset names a position inside the mapping rather than past its end.
    #[test]
    fn an_offset_past_the_mapping_wraps_into_it() {
        assert_eq!(clamped_chunk_extent(70000, 16, 65536).offset, 4464);
    }

    /// The failure this exists to prevent: the pair becomes a Rust slice, so a
    /// size the daemon overstates would be a read past the end of PipeWire's
    /// mapping — not a bad sample value.
    ///
    /// Mental revert: take `chunk->size` at face value and this returns 999 999
    /// bytes out of a 4 096-byte buffer.
    #[test]
    fn a_size_larger_than_the_mapping_is_clamped_to_what_is_actually_there() {
        assert_eq!(clamped_chunk_extent(0, 999_999, 4096).byte_count, 4096);
        assert_eq!(clamped_chunk_extent(1024, 999_999, 4096).byte_count, 3072);
    }

    /// A buffer that maps nothing yields nothing, rather than a modulo by zero.
    #[test]
    fn a_buffer_that_maps_nothing_yields_an_empty_extent() {
        assert_eq!(
            clamped_chunk_extent(16, 64, 0),
            capture_shim::ChunkExtent {
                offset: 0,
                byte_count: 0
            }
        );
    }

    /// The two halves agree on how many entry points there are and on their
    /// order, because only the C side states it. A Rust-side restatement is
    /// what this exists to make impossible.
    #[test]
    fn the_shim_names_every_entry_point_it_expects_rust_to_resolve() {
        let count = unsafe { capture_shim::streamlib_pipewire_entry_point_count() };
        assert!(count > 0, "the shim names no entry points at all");

        let names = unsafe { capture_shim::streamlib_pipewire_entry_point_names() };
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

    /// The demotion path, driven through the production loader rather than a
    /// copy of it: a container without libpipewire has to say which library it
    /// went looking for, or its log names only the arm that was chosen and
    /// never the one that was skipped.
    ///
    /// Mental revert: drop the soname from `resolve_from`'s not-found message
    /// and this fails, which the previous version of this test did not.
    #[test]
    fn a_missing_library_demotes_and_names_the_library_it_looked_for() {
        let missing_soname = "libpipewire-0.3.so.0.streamlib-test-no-such-library";
        let Err(reason) = PipeWireLibraryEntryPoints::resolve_from(missing_soname) else {
            panic!("a library with no such file cannot load");
        };
        assert!(
            reason.to_string().contains(missing_soname),
            "the demotion reason must name the library it could not load: {reason}"
        );
    }

    /// A library that loads but exports none of this is the older-PipeWire
    /// case, and it has to name the symbol rather than crash on a null address.
    /// libc stands in for it: it always loads, and it exports no `pw_*`.
    #[test]
    fn a_library_missing_an_entry_point_names_the_symbol_rather_than_crashing() {
        let Err(reason) = PipeWireLibraryEntryPoints::resolve_from("libc.so.6") else {
            panic!("libc exports no pw_* symbol, so the table cannot resolve against it");
        };
        let reason = reason.to_string();
        assert!(
            reason.contains("libc.so.6") && reason.contains("pw_"),
            "the reason must name both the library and the symbol it wanted: {reason}"
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
