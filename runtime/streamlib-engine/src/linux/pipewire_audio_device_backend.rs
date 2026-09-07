// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The audio backend chain's first arm: PipeWire, reached entirely at runtime.
//!
//! libpipewire is loaded and resolved once per process by
//! [`crate::linux::pipewire_runtime_library`]; this arm calls the audio half
//! of the shim through that table, and links no audio library itself.

use std::borrow::Cow;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::Arc;

use crate::core::context::{
    AudioBlockForPlaybackHandOff, AudioBlockRequestedByDevice, AudioCaptureStream,
    AudioDeviceBackend, AudioDeviceBackendArmUnavailableReason, AudioDeviceStreamRequest,
    AudioPlaybackStream, AudioSampleFormat, AudioStreamFailureReason, AudioStreamFailureRecorder,
    AudioStreamFormat, AudioStreamLivenessReport, CapturedAudioBlockFromDevice,
    CapturedAudioBlockHandOff,
};
use crate::core::{Error, Result};
use crate::linux::pipewire_runtime_library::{PipeWireLibraryEntryPoints, ShimFailureText};

mod audio_shim {
    use std::ffi::{c_char, c_int, c_void};

    /// Mirrors `struct StreamLibPipeWireNegotiatedAudioFormat`.
    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct NegotiatedAudioFormat {
        pub sample_rate: u32,
        pub channels: u32,
        pub sample_format: u32,
    }

    /// `STREAMLIB_PIPEWIRE_SAMPLE_FORMAT_F32_LE`.
    pub const SAMPLE_FORMAT_F32_LE: u32 = 0;
    /// `STREAMLIB_PIPEWIRE_SAMPLE_FORMAT_I16_LE`.
    pub const SAMPLE_FORMAT_I16_LE: u32 = 1;

    /// Mirrors `struct StreamLibPipeWireAudioStream`, which is opaque here.
    #[repr(C)]
    pub struct AudioStream {
        _opaque: [u8; 0],
    }

    /// `STREAMLIB_PIPEWIRE_STREAM_DIRECTION_CAPTURE`.
    pub const STREAM_DIRECTION_CAPTURE: c_int = 0;
    /// `STREAMLIB_PIPEWIRE_STREAM_DIRECTION_PLAYBACK`.
    pub const STREAM_DIRECTION_PLAYBACK: c_int = 1;

    pub type CapturedBlockHandOff = unsafe extern "C" fn(
        hand_off_context: *mut c_void,
        interleaved_sample_bytes: *const u8,
        interleaved_sample_byte_count: usize,
        sample_count: u32,
        first_sample_timestamp_ns: i64,
    );

    pub type PlaybackBlockHandOff = unsafe extern "C" fn(
        hand_off_context: *mut c_void,
        interleaved_sample_bytes_to_fill: *mut u8,
        interleaved_sample_byte_count: usize,
        sample_count: u32,
    );

    pub type StreamFailureHandOff =
        unsafe extern "C" fn(hand_off_context: *mut c_void, reason: *const c_char);

    unsafe extern "C" {
        pub fn streamlib_pipewire_audio_stream_open(
            entry_points: *const *mut c_void,
            direction: c_int,
            device_id_or_null: *const c_char,
            negotiated_format_out: *mut NegotiatedAudioFormat,
            failure_text: *mut c_char,
            failure_text_capacity: usize,
        ) -> *mut AudioStream;
        pub fn streamlib_pipewire_capture_stream_start_delivering(
            audio_stream: *mut AudioStream,
            hand_off: CapturedBlockHandOff,
            hand_off_context: *mut c_void,
        );
        pub fn streamlib_pipewire_playback_stream_start_requesting(
            audio_stream: *mut AudioStream,
            hand_off: PlaybackBlockHandOff,
            hand_off_context: *mut c_void,
        );
        pub fn streamlib_pipewire_audio_stream_report_failures_to(
            audio_stream: *mut AudioStream,
            hand_off: Option<StreamFailureHandOff>,
            hand_off_context: *mut c_void,
        );
        pub fn streamlib_pipewire_audio_stream_stop_handing_off(audio_stream: *mut AudioStream);
        pub fn streamlib_pipewire_audio_stream_close(audio_stream: *mut AudioStream);
    }

    /// Mirrors `struct StreamLibPipeWireStreamProperty`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    #[cfg(test)]
    pub struct StreamProperty {
        pub key: *const c_char,
        pub value: *const c_char,
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
        pub fn streamlib_pipewire_stream_properties(
            items: *mut StreamProperty,
            item_capacity: u32,
            direction: c_int,
            device_id_or_null: *const c_char,
            sink_name: *mut c_char,
            sink_name_capacity: usize,
        ) -> u32;
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
    pub fn load_and_connect() -> std::result::Result<Self, AudioDeviceBackendArmUnavailableReason> {
        let entry_points = PipeWireLibraryEntryPoints::loaded_once_per_process()
            .map_err(|reason| AudioDeviceBackendArmUnavailableReason::of(reason.to_string()))?;

        entry_points
            .daemon_answers()
            .map_err(AudioDeviceBackendArmUnavailableReason::of)?;

        tracing::debug!(
            version = %entry_points.loaded_library_version(),
            "PipeWire audio arm: a daemon answered"
        );

        Ok(Self {
            entry_points: Arc::clone(entry_points),
        })
    }
}

impl AudioDeviceBackend for PipeWireAudioDeviceBackend {
    fn backend_name(&self) -> &'static str {
        "pipewire"
    }

    fn open_capture_stream(
        &self,
        request: &AudioDeviceStreamRequest,
    ) -> Result<Box<dyn AudioCaptureStream>> {
        let (opened, capture_stream_format) =
            self.open_audio_stream(audio_shim::STREAM_DIRECTION_CAPTURE, request)?;
        let (failure_recorder, liveness_report) =
            AudioStreamFailureRecorder::recording_into_a_new_report();
        let capture_stream = PipeWireAudioCaptureStream {
            opened,
            capture_stream_format,
            installed_hand_off: None,
            failure_recorder: Box::new(failure_recorder),
            liveness_report,
        };
        // SAFETY: the stream is live, and the recorder it points at is owned by
        // the struct being returned, whose drop order frees it only after the
        // close that retires this hand-off.
        unsafe {
            install_the_shims_failure_hand_off_pointing_at(
                capture_stream.opened.audio_stream,
                &capture_stream.failure_recorder,
            );
        }
        Ok(Box::new(capture_stream))
    }

    fn open_playback_stream(
        &self,
        request: &AudioDeviceStreamRequest,
    ) -> Result<Box<dyn AudioPlaybackStream>> {
        let (opened, playback_stream_format) =
            self.open_audio_stream(audio_shim::STREAM_DIRECTION_PLAYBACK, request)?;
        let (failure_recorder, liveness_report) =
            AudioStreamFailureRecorder::recording_into_a_new_report();
        let playback_stream = PipeWireAudioPlaybackStream {
            opened,
            playback_stream_format,
            installed_hand_off: None,
            failure_recorder: Box::new(failure_recorder),
            liveness_report,
        };
        // SAFETY: as for the capture stream above.
        unsafe {
            install_the_shims_failure_hand_off_pointing_at(
                playback_stream.opened.audio_stream,
                &playback_stream.failure_recorder,
            );
        }
        Ok(Box::new(playback_stream))
    }
}

impl PipeWireAudioDeviceBackend {
    /// Open one stream in either direction and read back what PipeWire settled
    /// on.
    ///
    /// One function for both because everything here is direction-blind: the
    /// device id is validated the same way, the shim is asked the same way, and
    /// a refusal has to name the device the same way.
    fn open_audio_stream(
        &self,
        direction: c_int,
        request: &AudioDeviceStreamRequest,
    ) -> Result<(OpenedPipeWireAudioStream, AudioStreamFormat)> {
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

        let mut negotiated_format = audio_shim::NegotiatedAudioFormat::default();
        let mut failure_text = ShimFailureText::new();
        let (failure_text_ptr, failure_text_capacity) = failure_text.as_shim_out_parameters();
        // SAFETY: the device id, if any, is a live `CString` for the length of
        // this call; the format and failure out-parameters are owned locals.
        let opened = unsafe {
            audio_shim::streamlib_pipewire_audio_stream_open(
                self.entry_points.as_ptr(),
                direction,
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

        let opened = OpenedPipeWireAudioStream {
            _entry_points: Arc::clone(&self.entry_points),
            audio_stream: opened,
        };
        Ok((opened, stream_format_of(negotiated_format)?))
    }
}

/// The seam's format, read out of what PipeWire settled on.
fn stream_format_of(negotiated: audio_shim::NegotiatedAudioFormat) -> Result<AudioStreamFormat> {
    let sample_format = match negotiated.sample_format {
        audio_shim::SAMPLE_FORMAT_F32_LE => AudioSampleFormat::F32,
        audio_shim::SAMPLE_FORMAT_I16_LE => AudioSampleFormat::I16,
        other => {
            return Err(Error::Runtime(format!(
                "the PipeWire audio shim reported sample format {other}, which names no \
                 encoding an AudioBlock can carry"
            )));
        }
    };
    Ok(AudioStreamFormat {
        sample_rate: negotiated.sample_rate,
        channels: negotiated.channels,
        sample_format,
    })
}

/// A shim-owned stream and the library every address in it points into, closed
/// exactly once when this drops.
///
/// Owned by whichever direction's stream holds it, so the close, the library
/// lifetime and the `Send` claim are stated once rather than per direction.
struct OpenedPipeWireAudioStream {
    /// Held so the library outlives every address the shim still holds.
    _entry_points: Arc<PipeWireLibraryEntryPoints>,
    audio_stream: *mut audio_shim::AudioStream,
}

impl Drop for OpenedPipeWireAudioStream {
    fn drop(&mut self) {
        // Closes and joins PipeWire's loop thread, so a hand-off dropped after
        // this is provably no longer reachable from a callback.
        // SAFETY: the pointer came from the shim's own `open` and is closed
        // exactly once, here.
        unsafe {
            audio_shim::streamlib_pipewire_audio_stream_close(self.audio_stream);
        }
    }
}

// The pointer is exclusively owned by this struct — nothing else holds a copy,
// and every shim entry point it is passed to takes PipeWire's thread-loop lock
// before touching anything the loop thread also touches. `pw_thread_loop` is a
// use-from-any-thread API, so moving that ownership between threads is sound.
// `Sync` is deliberately not implemented: two `&` references could install a
// hand-off concurrently.
unsafe impl Send for OpenedPipeWireAudioStream {}

/// What the shim calls on PipeWire's thread-loop thread, with that loop's lock
/// held, when a stream it already opened enters its error state.
///
/// Must not unwind: this is a plain `extern "C"` boundary, so a panic here
/// aborts the process rather than crossing into C.
///
/// Recording is all it does. The push stops at this function and the seam stays
/// pollable, which is the point: an owner told about a death on the loop thread
/// could not act on it — the one natural reaction, stopping the stream, takes
/// the very lock this call is holding.
///
/// It allocates, which would be indefensible on the sample path and is fine
/// here: the shim keeps the first reason and calls this at most once for the
/// life of a stream.
unsafe extern "C" fn record_a_stream_failure_in_the_liveness_report(
    failure_recorder_context: *mut c_void,
    reason: *const c_char,
) {
    // SAFETY: the context is the address of the recorder *inside* the box the
    // stream installed under the loop lock — which is why the box is there,
    // since the struct holding it moves and that heap address does not. `close`
    // retires this hand-off under that same lock before the box is dropped, so
    // the recorder is live for the length of this call.
    let failure_recorder =
        unsafe { &*failure_recorder_context.cast::<AudioStreamFailureRecorder>() };
    let reason: Cow<'_, str> = if reason.is_null() {
        Cow::Borrowed("the PipeWire stream entered its error state")
    } else {
        // SAFETY: the shim hands over its own NUL-terminated failure text,
        // which stays valid until this returns.
        unsafe { CStr::from_ptr(reason) }.to_string_lossy()
    };
    failure_recorder.record_the_failure_that_ended_the_stream(AudioStreamFailureReason::of(
        format!("the PipeWire stream stopped serving its device: {reason}"),
    ));
}

/// Install the shim's failure hand-off, pointing it at a stream's own recorder.
///
/// Done at open rather than at the first delivery: a stream that dies while its
/// owner has it stopped has still died, and an owner that stops and looks has
/// to find that out.
///
/// # Safety
///
/// `audio_stream` must be live, and `failure_recorder` must outlive it — which
/// is what the drop order on both stream structs guarantees.
unsafe fn install_the_shims_failure_hand_off_pointing_at(
    audio_stream: *mut audio_shim::AudioStream,
    failure_recorder: &AudioStreamFailureRecorder,
) {
    let failure_recorder_context = (&raw const *failure_recorder).cast_mut().cast::<c_void>();
    // SAFETY: the caller's contract, and the shim takes the loop lock around
    // the install.
    unsafe {
        audio_shim::streamlib_pipewire_audio_stream_report_failures_to(
            audio_stream,
            Some(record_a_stream_failure_in_the_liveness_report),
            failure_recorder_context,
        );
    }
}

/// One PipeWire capture stream, negotiated and connected.
struct PipeWireAudioCaptureStream {
    opened: OpenedPipeWireAudioStream,
    capture_stream_format: AudioStreamFormat,
    /// The hand-off the shim's callback context points at. Owned here so it
    /// outlives every delivery and is freed only once the shim has promised no
    /// further callback.
    ///
    /// **Declared after `opened`, and it must stay that way.** Rust drops
    /// fields in declaration order, and `opened`'s drop is what closes the
    /// stream and joins PipeWire's loop thread — so this box is freed only
    /// once no callback can still hold a pointer into it. Move it above and
    /// the loop thread reads freed memory, with nothing to say so.
    installed_hand_off: Option<Box<CapturedAudioBlockHandOff>>,
    /// What the shim's failure hand-off points at, boxed for a stable address
    /// and declared after `opened` for the same reason `installed_hand_off`
    /// is.
    failure_recorder: Box<AudioStreamFailureRecorder>,
    /// The read half handed to whoever owns the stream. Not boxed: nothing in
    /// C points at it.
    liveness_report: AudioStreamLivenessReport,
}

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
    fn stream_format(&self) -> AudioStreamFormat {
        self.capture_stream_format
    }

    fn liveness_report(&self) -> AudioStreamLivenessReport {
        self.liveness_report.clone()
    }

    fn start_delivering_to(&mut self, hand_off: CapturedAudioBlockHandOff) -> Result<()> {
        // Boxed so C gets a thin, stable address for what is otherwise a fat
        // pointer. The allocation does not move when the `Box` itself does.
        let hand_off = Box::new(hand_off);
        let hand_off_context = (&raw const *hand_off).cast_mut().cast::<c_void>();
        // SAFETY: the stream pointer is live, and the context stays valid
        // because the `Box` is stored on `self` on the next line.
        unsafe {
            audio_shim::streamlib_pipewire_capture_stream_start_delivering(
                self.opened.audio_stream,
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
            audio_shim::streamlib_pipewire_audio_stream_stop_handing_off(self.opened.audio_stream);
        }
        self.installed_hand_off = None;
        Ok(())
    }
}

/// One PipeWire playback stream, negotiated and connected.
struct PipeWireAudioPlaybackStream {
    opened: OpenedPipeWireAudioStream,
    playback_stream_format: AudioStreamFormat,
    /// The hand-off the shim's callback context points at. Owned here so it
    /// outlives every request and is freed only once the shim has promised no
    /// further callback.
    ///
    /// **Declared after `opened`, and it must stay that way** — the same
    /// drop-order requirement its capture sibling carries, for the same
    /// reason.
    installed_hand_off: Option<Box<AudioBlockForPlaybackHandOff>>,
    /// What the shim's failure hand-off points at, under the same drop-order
    /// requirement, beside the read half its owner is handed.
    failure_recorder: Box<AudioStreamFailureRecorder>,
    liveness_report: AudioStreamLivenessReport,
}

/// What the shim calls on PipeWire's thread-loop thread when it needs samples,
/// with that loop's lock held.
///
/// Must not unwind: this is a plain `extern "C"` boundary, so a panic here
/// aborts the process rather than crossing into C.
unsafe extern "C" fn fill_requested_block_from_hand_off(
    hand_off_context: *mut c_void,
    interleaved_sample_bytes_to_fill: *mut u8,
    interleaved_sample_byte_count: usize,
    sample_count: u32,
) {
    if interleaved_sample_bytes_to_fill.is_null() {
        return;
    }
    // SAFETY: the context is the address of the `Box<AudioBlockForPlaybackHandOff>`
    // that `start_requesting_from` installed under the loop lock, and
    // `stop_requesting` retires it under that same lock before dropping it — so
    // it is live for the length of this call.
    let hand_off = unsafe { &*hand_off_context.cast::<AudioBlockForPlaybackHandOff>() };
    // SAFETY: the shim sized this against the dequeued buffer's own mapping,
    // which stays valid until this returns, and the loop thread is the only
    // one writing it.
    let interleaved_sample_bytes_to_fill = unsafe {
        std::slice::from_raw_parts_mut(
            interleaved_sample_bytes_to_fill,
            interleaved_sample_byte_count,
        )
    };
    hand_off(AudioBlockRequestedByDevice {
        interleaved_sample_bytes_to_fill,
        sample_count,
    });
}

impl AudioPlaybackStream for PipeWireAudioPlaybackStream {
    fn stream_format(&self) -> AudioStreamFormat {
        self.playback_stream_format
    }

    fn liveness_report(&self) -> AudioStreamLivenessReport {
        self.liveness_report.clone()
    }

    fn start_requesting_from(&mut self, hand_off: AudioBlockForPlaybackHandOff) -> Result<()> {
        // Boxed so C gets a thin, stable address for what is otherwise a fat
        // pointer. The allocation does not move when the `Box` itself does.
        let hand_off = Box::new(hand_off);
        let hand_off_context = (&raw const *hand_off).cast_mut().cast::<c_void>();
        // SAFETY: the stream pointer is live, and the context stays valid
        // because the `Box` is stored on `self` on the next line.
        unsafe {
            audio_shim::streamlib_pipewire_playback_stream_start_requesting(
                self.opened.audio_stream,
                fill_requested_block_from_hand_off,
                hand_off_context,
            );
        }
        // Installed before the previous one is dropped, for the same reason the
        // capture stream does it in this order.
        self.installed_hand_off = Some(hand_off);
        Ok(())
    }

    fn stop_requesting(&mut self) -> Result<()> {
        // SAFETY: the stream pointer is live. The shim takes the loop lock, so
        // when it returns no callback can still be reading the context that the
        // next line drops.
        unsafe {
            audio_shim::streamlib_pipewire_audio_stream_stop_handing_off(self.opened.audio_stream);
        }
        self.installed_hand_off = None;
        Ok(())
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
            audio_shim::streamlib_pipewire_first_sample_timestamp_ns(
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

    /// The shim's half of the death path, held without a daemon: the state
    /// change lands on the loop thread, and what it leaves behind is a reason
    /// the stream's owner can read from its own thread.
    ///
    /// Mental revert: drop the record and this arm is back to a stream that
    /// entered its error state, said so once in a log, and went on looking
    /// healthy to everything holding it.
    #[test]
    fn a_failure_the_shim_reports_lands_in_the_report_the_owner_holds() {
        let (failure_recorder, liveness_report) =
            AudioStreamFailureRecorder::recording_into_a_new_report();
        let reason_the_daemon_gave = CString::new("node destroyed").expect("no interior NUL");

        // SAFETY: the context is the address of a live report, and the reason
        // is a NUL-terminated string that outlives the call — exactly what the
        // shim hands over on the loop thread.
        unsafe {
            record_a_stream_failure_in_the_liveness_report(
                (&raw const failure_recorder).cast_mut().cast::<c_void>(),
                reason_the_daemon_gave.as_ptr(),
            );
        }

        let reason = liveness_report
            .failure_that_ended_the_stream()
            .expect("a stream that entered its error state has to leave its owner a reason")
            .to_string();
        assert!(
            reason.contains("node destroyed") && reason.contains("PipeWire"),
            "the reason has to carry both the daemon's own words and which arm they came \
             from: {reason}"
        );
    }

    /// `pw_stream_events::state_changed` may hand over a NULL error, and a
    /// stream that failed for reasons the daemon did not spell still has to be
    /// reported as failed — silence here would be the bug this change exists
    /// to remove.
    #[test]
    fn a_failure_the_daemon_did_not_explain_is_still_reported_as_one() {
        let (failure_recorder, liveness_report) =
            AudioStreamFailureRecorder::recording_into_a_new_report();

        // SAFETY: as above, with the NULL reason libpipewire is allowed to
        // pass.
        unsafe {
            record_a_stream_failure_in_the_liveness_report(
                (&raw const failure_recorder).cast_mut().cast::<c_void>(),
                std::ptr::null(),
            );
        }

        assert!(
            liveness_report.failure_that_ended_the_stream().is_some(),
            "a stream that failed without a message is still a stream that failed"
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
            audio_shim::streamlib_pipewire_first_sample_timestamp_ns(
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
            audio_shim::streamlib_pipewire_sink_name_length_of_monitor_device_id(device_id.as_ptr())
        };
        (length > 0).then(|| device_id.to_string_lossy()[..length].to_string())
    }

    /// `STREAMLIB_PIPEWIRE_MAX_STREAM_PROPERTIES`, restated. The shim refuses to
    /// compose anything when handed a smaller capacity than its own maximum, so
    /// a property added on the C side without updating this fails these tests
    /// loudly rather than overflowing the array.
    const MAX_STREAM_PROPERTIES: usize = 5;

    /// The key/value pairs the shim would announce a stream with, read back as
    /// owned strings.
    fn composed_stream_properties(
        direction: c_int,
        device_id: Option<&str>,
    ) -> Option<Vec<(String, String)>> {
        let device_id = device_id.map(|id| CString::new(id).expect("a test device id has no NUL"));
        let mut items = [audio_shim::StreamProperty {
            key: std::ptr::null(),
            value: std::ptr::null(),
        }; MAX_STREAM_PROPERTIES];
        let mut sink_name = [0u8; 256];
        let count = unsafe {
            audio_shim::streamlib_pipewire_stream_properties(
                items.as_mut_ptr(),
                MAX_STREAM_PROPERTIES as u32,
                direction,
                device_id
                    .as_ref()
                    .map_or(std::ptr::null(), |id| id.as_ptr()),
                sink_name.as_mut_ptr().cast::<c_char>(),
                sink_name.len(),
            )
        };
        if count == 0 {
            return None;
        }
        Some(
            items[..count as usize]
                .iter()
                .map(|item| unsafe {
                    (
                        CStr::from_ptr(item.key).to_string_lossy().into_owned(),
                        CStr::from_ptr(item.value).to_string_lossy().into_owned(),
                    )
                })
                .collect(),
        )
    }

    /// The commit's actual behaviour, not just its string parsing: a
    /// `<sink>.monitor` id has to reach PipeWire as the bare sink name *plus*
    /// `stream.capture.sink`, because a sink named without that property is
    /// answered with the session's default source.
    ///
    /// Mental revert: drop the `stream.capture.sink` item and this reddens —
    /// which nothing did before, so the fix was held only by a manual rig run.
    #[test]
    fn a_monitor_device_id_asks_pipewire_for_the_sinks_monitor() {
        let properties = composed_stream_properties(
            audio_shim::STREAM_DIRECTION_CAPTURE,
            Some("streamlib-fixture-audio-sink.monitor"),
        )
        .expect("a monitor id composes properties");
        assert!(
            properties.contains(&(
                "target.object".to_string(),
                "streamlib-fixture-audio-sink".to_string()
            )),
            "the target is the sink itself, not the `.monitor` spelling: {properties:?}"
        );
        assert!(
            properties.contains(&("stream.capture.sink".to_string(), "true".to_string())),
            "without this the stream attaches to the default source: {properties:?}"
        );
    }

    /// A plain device id is targeted as itself and gains no capture-sink flag,
    /// which would otherwise look for a monitor a source does not have.
    #[test]
    fn a_plain_device_id_is_targeted_as_an_ordinary_source() {
        let properties = composed_stream_properties(
            audio_shim::STREAM_DIRECTION_CAPTURE,
            Some("alsa_input.pci-0000_00_1f.3"),
        )
        .expect("a plain id composes properties");
        assert!(properties.contains(&(
            "target.object".to_string(),
            "alsa_input.pci-0000_00_1f.3".to_string()
        )));
        assert!(
            !properties
                .iter()
                .any(|(key, _)| key == "stream.capture.sink"),
            "a source has no monitor to capture: {properties:?}"
        );
    }

    /// No device named: the session routes the stream to its own default, so
    /// nothing targets anything.
    #[test]
    fn no_device_id_names_no_target_at_all() {
        let properties = composed_stream_properties(audio_shim::STREAM_DIRECTION_CAPTURE, None)
            .expect("the default composes");
        assert!(!properties.iter().any(|(key, _)| key == "target.object"));
        assert!(
            !properties
                .iter()
                .any(|(key, _)| key == "stream.capture.sink")
        );
    }

    /// A monitor id whose sink name will not fit composes nothing, which the
    /// caller turns into a named refusal. Falling through to a plain target
    /// would be the very bug the convention exists to remove.
    #[test]
    fn an_over_long_monitor_device_id_composes_nothing_rather_than_a_plain_target() {
        let over_long = format!("{}.monitor", "s".repeat(400));
        assert!(
            composed_stream_properties(audio_shim::STREAM_DIRECTION_CAPTURE, Some(&over_long))
                .is_none()
        );
    }

    /// The direction the monitor convention does *not* apply to: a speaker
    /// pointed at `<sink>.monitor` wants that sink, and asking for its monitor
    /// would target a capture endpoint nothing can be played into.
    ///
    /// Mental revert: share one property composition across both directions and
    /// a playback stream announces itself as `Capture` with
    /// `stream.capture.sink` set — which is how audio ends up going nowhere
    /// while every log line says the stream connected.
    #[test]
    fn a_playback_stream_targets_the_sink_itself_and_announces_itself_as_playback() {
        let properties = composed_stream_properties(
            audio_shim::STREAM_DIRECTION_PLAYBACK,
            Some("streamlib-fixture-audio-sink"),
        )
        .expect("a playback target composes properties");
        assert!(
            properties.contains(&("media.category".to_string(), "Playback".to_string())),
            "a playback stream announces the direction it runs in: {properties:?}"
        );
        assert!(
            properties.contains(&(
                "target.object".to_string(),
                "streamlib-fixture-audio-sink".to_string()
            )),
            "the target is the sink that was named: {properties:?}"
        );
        assert!(
            !properties
                .iter()
                .any(|(key, _)| key == "stream.capture.sink"),
            "nothing is captured on a playback stream: {properties:?}"
        );
    }

    /// A `.monitor` suffix is a capture spelling, and a speaker handed one must
    /// not quietly become a capture stream — it targets what it was given and
    /// PipeWire refuses the link if that names nothing playable.
    #[test]
    fn a_playback_stream_never_takes_the_monitor_path() {
        let properties = composed_stream_properties(
            audio_shim::STREAM_DIRECTION_PLAYBACK,
            Some("streamlib-fixture-audio-sink.monitor"),
        )
        .expect("a playback target composes properties");
        assert!(
            properties.contains(&(
                "target.object".to_string(),
                "streamlib-fixture-audio-sink.monitor".to_string()
            )),
            "the id is passed through rather than having a suffix stripped: {properties:?}"
        );
        assert!(
            !properties
                .iter()
                .any(|(key, _)| key == "stream.capture.sink"),
            "nothing is captured on a playback stream: {properties:?}"
        );
    }

    /// The capture side keeps saying so, which is what makes the two-value
    /// property a real distinction rather than a constant.
    #[test]
    fn a_capture_stream_announces_itself_as_capture() {
        let properties = composed_stream_properties(audio_shim::STREAM_DIRECTION_CAPTURE, None)
            .expect("the default composes");
        assert!(
            properties.contains(&("media.category".to_string(), "Capture".to_string())),
            "{properties:?}"
        );
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

    fn clamped_chunk_extent(offset: u32, size: u32, maxsize: u32) -> audio_shim::ChunkExtent {
        unsafe { audio_shim::streamlib_pipewire_clamped_chunk_extent(offset, size, maxsize) }
    }

    /// The ordinary case: a chunk that sits inside its buffer is passed through
    /// untouched.
    #[test]
    fn a_chunk_within_its_buffer_is_left_alone() {
        assert_eq!(
            clamped_chunk_extent(0, 8192, 65536),
            audio_shim::ChunkExtent {
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
            audio_shim::ChunkExtent {
                offset: 0,
                byte_count: 0
            }
        );
    }
}
