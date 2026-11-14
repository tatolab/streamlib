// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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
    AudioDeviceBackend, AudioDeviceStreamRequest, AudioPlaybackStream, AudioSampleFormat,
    AudioStreamFormat, CapturedAudioBlockFromDevice, CapturedAudioBlockHandOff,
    DeviceBackendArmUnavailableReason, DeviceStreamFailureReason, DeviceStreamFailureRecorder,
    DeviceStreamLivenessReport, RequestedAudioStreamFormat,
};
use crate::core::{Error, Result};
use crate::linux::pipewire_runtime_library::{PipeWireLibraryEntryPoints, ShimFailureText};
use crate::linux::thread_priority::{
    apply_realtime_priority_to_thread_of_this_process, thread_of_this_process_is_realtime,
};

mod audio_shim {
    use std::ffi::{c_char, c_int, c_void};

    /// Mirrors `struct StreamLibPipeWireRequestedAudioFormat`.
    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct RequestedAudioFormat {
        pub sample_rate: u32,
        pub channels: u32,
        pub sample_format: u32,
    }

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
            requested_format: *const RequestedAudioFormat,
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
    pub fn load_and_connect() -> std::result::Result<Self, DeviceBackendArmUnavailableReason> {
        let entry_points = PipeWireLibraryEntryPoints::loaded_once_per_process()
            .map_err(|reason| DeviceBackendArmUnavailableReason::of(reason.to_string()))?;

        entry_points
            .daemon_answers()
            .map_err(DeviceBackendArmUnavailableReason::of)?;

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
        let (opened, capture_stream_format) = self.open_audio_stream(
            audio_shim::STREAM_DIRECTION_CAPTURE,
            request,
            RequestedAudioStreamFormat::default(),
        )?;
        let (failure_recorder, liveness_report) =
            DeviceStreamFailureRecorder::recording_into_a_new_report();
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
        let (opened, playback_stream_format) = self.open_audio_stream(
            audio_shim::STREAM_DIRECTION_PLAYBACK,
            request,
            request.requested_format,
        )?;
        let (failure_recorder, liveness_report) =
            DeviceStreamFailureRecorder::recording_into_a_new_report();
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
    /// Open one stream in either direction toward `requested_format` and read
    /// back what PipeWire settled on. A capture stream asks for nothing but the
    /// scalar encoding, so it settles on the device's own rate and channels.
    ///
    /// One function for both because everything here is direction-blind: the
    /// device id is validated the same way, the shim is asked the same way, and
    /// a refusal has to name the device the same way.
    fn open_audio_stream(
        &self,
        direction: c_int,
        request: &AudioDeviceStreamRequest,
        requested_format: RequestedAudioStreamFormat,
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

        let requested_format = audio_shim::RequestedAudioFormat {
            sample_rate: requested_format.sample_rate.unwrap_or(0),
            channels: requested_format.channels.unwrap_or(0),
            sample_format: match requested_format.sample_format {
                Some(AudioSampleFormat::I16) => audio_shim::SAMPLE_FORMAT_I16_LE,
                Some(AudioSampleFormat::F32) | None => audio_shim::SAMPLE_FORMAT_F32_LE,
            },
        };
        let mut negotiated_format = audio_shim::NegotiatedAudioFormat::default();
        let mut failure_text = ShimFailureText::new();
        let (failure_text_ptr, failure_text_capacity) = failure_text.as_shim_out_parameters();
        // SAFETY: the device id, if any, is a live `CString` for the length of
        // this call; the requested format and the format and failure
        // out-parameters are owned locals.
        let opened = unsafe {
            audio_shim::streamlib_pipewire_audio_stream_open(
                self.entry_points.as_ptr(),
                direction,
                device_id
                    .as_ref()
                    .map_or(std::ptr::null(), |device_id| device_id.as_ptr()),
                &requested_format,
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
        ask_for_realtime_on_every_pipewire_audio_thread_of_this_process();
        Ok((opened, stream_format_of(negotiated_format)?))
    }
}

/// Kernel thread names (cut to 15 bytes) of the threads an audio stream's
/// samples cross in this process: the shim's thread loop, which runs every
/// `process` callback, and PipeWire's data loop, which feeds it.
const PIPEWIRE_AUDIO_THREAD_NAMES: [&str; 2] = ["streamlib-audio", "pw-data-loop"];

/// Ask for realtime scheduling on each PipeWire audio thread of this process
/// that is not realtime yet.
///
/// PipeWire's default client config loads no realtime module, so a client's
/// threads stay on `SCHED_OTHER` unless the client asks.
fn ask_for_realtime_on_every_pipewire_audio_thread_of_this_process() {
    let Ok(threads_of_this_process) = std::fs::read_dir("/proc/self/task") else {
        return;
    };
    for thread_entry in threads_of_this_process.flatten() {
        let Some(thread_id) = thread_entry
            .file_name()
            .to_str()
            .and_then(|thread_id| thread_id.parse::<u64>().ok())
        else {
            continue;
        };
        let Ok(thread_name) = std::fs::read_to_string(thread_entry.path().join("comm")) else {
            continue;
        };
        let thread_name = thread_name.trim_end();
        if !PIPEWIRE_AUDIO_THREAD_NAMES.contains(&thread_name)
            || thread_of_this_process_is_realtime(thread_id)
        {
            continue;
        }
        match apply_realtime_priority_to_thread_of_this_process(thread_id) {
            Ok(()) => tracing::info!(
                thread_id,
                thread_name,
                "PipeWire audio thread scheduled realtime"
            ),
            Err(refusal) => tracing::debug!(
                thread_id,
                thread_name,
                %refusal,
                "PipeWire audio thread stays best-effort: realtime scheduling was refused"
            ),
        }
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
        unsafe { &*failure_recorder_context.cast::<DeviceStreamFailureRecorder>() };
    let reason: Cow<'_, str> = if reason.is_null() {
        Cow::Borrowed("the PipeWire stream entered its error state")
    } else {
        // SAFETY: the shim hands over its own NUL-terminated failure text,
        // which stays valid until this returns.
        unsafe { CStr::from_ptr(reason) }.to_string_lossy()
    };
    failure_recorder.record_the_failure_that_ended_the_stream(DeviceStreamFailureReason::of(
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
    failure_recorder: &DeviceStreamFailureRecorder,
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
    failure_recorder: Box<DeviceStreamFailureRecorder>,
    /// The read half handed to whoever owns the stream. Not boxed: nothing in
    /// C points at it.
    liveness_report: DeviceStreamLivenessReport,
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

    fn liveness_report(&self) -> DeviceStreamLivenessReport {
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
    failure_recorder: Box<DeviceStreamFailureRecorder>,
    liveness_report: DeviceStreamLivenessReport,
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

    /// Not reported on this arm, so a caller keeps the period it assumes.
    fn device_period_in_per_channel_samples(&self) -> Option<u32> {
        None
    }

    fn liveness_report(&self) -> DeviceStreamLivenessReport {
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
mod tests;
