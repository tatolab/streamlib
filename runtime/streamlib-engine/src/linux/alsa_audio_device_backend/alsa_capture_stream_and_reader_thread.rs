// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The ALSA capture stream and the reader thread that waits for a period, reads the device's timing and hands the block off.

use super::*;

/// The reader thread and the flag that stops it.
struct CaptureDeliveryThread {
    stop_requested: Arc<AtomicBool>,
    reader_thread: JoinHandle<()>,
}

/// One ALSA capture stream, negotiated and ready to run.
pub(super) struct AlsaAudioCaptureStream {
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
    failure_recorder: DeviceStreamFailureRecorder,
    /// The read half, cloned to whoever owns the stream.
    liveness_report: DeviceStreamLivenessReport,
    delivery: Option<CaptureDeliveryThread>,
}

impl AudioCaptureStream for AlsaAudioCaptureStream {
    fn stream_format(&self) -> AudioStreamFormat {
        self.capture_stream_format
    }

    fn liveness_report(&self) -> DeviceStreamLivenessReport {
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
    pub(super) fn prove_the_device_can_be_timed(&mut self) -> Result<()> {
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
pub(super) fn prepare_and_start_capture(
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
    failure_recorder: DeviceStreamFailureRecorder,
    hand_off: CapturedAudioBlockHandOff,
}

fn spawn_capture_reader_thread(inputs: CaptureReaderThreadInputs) -> Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(format!("streamlib-alsa-capture-{}", inputs.device_name))
        .spawn(
            crate::core::logging::carrying_this_threads_project_log_route_entry(move || {
                run_capture_reader_thread(inputs)
            }),
        )
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

    // Realtime comes only from asking for it, through rtkit or `CAP_SYS_NICE`.
    // Failure is logged and the thread continues on `SCHED_OTHER`, which is
    // what a container without rtkit gets.
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
                    break exit_for_a_device_that_delivered_nothing(
                        stop_requested.load(Ordering::Acquire),
                        consecutive_silent_waits,
                    );
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

impl AlsaAudioDeviceBackend {
    /// Open and negotiate one capture stream, before any delivery starts.
    pub(super) fn open_alsa_capture_stream(
        &self,
        pcm_name: &str,
    ) -> Result<AlsaAudioCaptureStream> {
        let opened_pcm =
            OpenedAlsaPcm::open(&self.entry_points, pcm_name, AlsaStreamDirection::Capture)?;
        let negotiated = negotiate_hardware_parameters(
            &self.entry_points,
            opened_pcm.pcm,
            pcm_name,
            AlsaStreamDirection::Capture,
            RequestedAudioStreamFormat::default(),
        )?;
        negotiate_capture_timestamp_contract(
            &self.entry_points,
            opened_pcm.pcm,
            SndPcmUframes::from(negotiated.period_sample_count),
            pcm_name,
        )?;

        let (failure_recorder, liveness_report) =
            DeviceStreamFailureRecorder::recording_into_a_new_report();
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
}
