// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The ALSA playback stream and the writer thread that waits for a period of room and asks for one.

use super::*;

/// The writer thread and the flag that stops it.
struct PlaybackWriterThread {
    stop_requested: Arc<AtomicBool>,
    writer_thread: JoinHandle<()>,
}

/// One ALSA playback stream, negotiated and ready to run.
pub(super) struct AlsaAudioPlaybackStream {
    opened_pcm: Arc<OpenedAlsaPcm>,
    playback_stream_format: AudioStreamFormat,
    period_sample_count: u32,
    /// What the caller named, or `default` — for error text a reader can act on.
    device_name: String,
    /// The write and read halves, minted with the stream for the reason its
    /// capture sibling states.
    failure_recorder: DeviceStreamFailureRecorder,
    liveness_report: DeviceStreamLivenessReport,
    playback: Option<PlaybackWriterThread>,
}

impl AudioPlaybackStream for AlsaAudioPlaybackStream {
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

/// Everything the writer thread owns, handed over in one move.
struct PlaybackWriterThreadInputs {
    opened_pcm: Arc<OpenedAlsaPcm>,
    playback_stream_format: AudioStreamFormat,
    period_sample_count: u32,
    device_name: String,
    stop_requested: Arc<AtomicBool>,
    failure_recorder: DeviceStreamFailureRecorder,
    hand_off: AudioBlockForPlaybackHandOff,
}

fn spawn_playback_writer_thread(inputs: PlaybackWriterThreadInputs) -> Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(format!("streamlib-alsa-playback-{}", inputs.device_name))
        .spawn(
            crate::core::logging::carrying_this_threads_project_log_route_entry(move || {
                run_playback_writer_thread(inputs)
            }),
        )
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

    // Realtime comes only from asking for it, through rtkit or `CAP_SYS_NICE`.
    // Failure is logged and the thread continues on `SCHED_OTHER`, which is
    // what a container without rtkit gets.
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
                    break exit_for_a_device_that_delivered_nothing(
                        stop_requested.load(Ordering::Acquire),
                        consecutive_silent_waits,
                    );
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

impl AlsaAudioDeviceBackend {
    /// Open and negotiate one playback stream toward `requested_format`, before
    /// any sample is written.
    pub(super) fn open_alsa_playback_stream(
        &self,
        pcm_name: &str,
        requested_format: RequestedAudioStreamFormat,
    ) -> Result<AlsaAudioPlaybackStream> {
        let opened_pcm =
            OpenedAlsaPcm::open(&self.entry_points, pcm_name, AlsaStreamDirection::Playback)?;
        let negotiated = negotiate_hardware_parameters(
            &self.entry_points,
            opened_pcm.pcm,
            pcm_name,
            AlsaStreamDirection::Playback,
            requested_format,
        )?;
        negotiate_playback_start_contract(
            &self.entry_points,
            opened_pcm.pcm,
            SndPcmUframes::from(negotiated.period_sample_count),
            pcm_name,
        )?;

        let (failure_recorder, liveness_report) =
            DeviceStreamFailureRecorder::recording_into_a_new_report();
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
