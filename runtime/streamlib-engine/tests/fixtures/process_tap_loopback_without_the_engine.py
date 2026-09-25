#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The known signal through a private Core Audio process tap, with no StreamLib.

`verify_audio_loopback.sh --path tap-muted` plays through `SpeakerSink` and
captures through a `MicrophoneSource` opened on a private process tap's
aggregate device. This closes the same loop with only the engine taken out:
the same `PrivateCaptureDeviceTappingThisProcessesOutput`, an IOProc on the
speaker where `SpeakerSink` was, and an IOProc on the aggregate, found by its
UID, where `MicrophoneSource` was. When that run fails and this one passes,
the tap is sound and the engine is not.

The tap is of this process, not of a player such as `afplay`: the HAL has no
process object to tap until a process is its client, so a tap of another
player could only be made once that player was already sounding.

Usage:
    process_tap_loopback_without_the_engine.py <signal.wav> <captured.wav>
        <speaker-device-uid> muted|unmuted

Writes what the tap heard as a mono WAV, and one line of evidence on stdout.
Exit 0 when there is a capture to analyse, 77 when none can be made here, and
1 when the loop is broken.
"""

import ctypes
import os
import sys
import threading

import numpy

import coreaudio_process_tap
import known_audio_signal

BYTES_PER_FLOAT32_SAMPLE = 4

# Silence played after the signal, so the tap's own latency lands inside what
# is recorded rather than after the player has stopped.
TRAILING_SILENCE_SECONDS = 1.0

# How long the aggregate has to run its first IO cycle, and the player to get
# past the signal's own length, before the loop counts as broken.
FIRST_IO_CYCLE_TIMEOUT_SECONDS = 5.0
PLAYBACK_TIMEOUT_MARGIN_SECONDS = 5.0


class KnownSignalPlayer:
    """Writes a mono signal into every channel of each output buffer, once, then zeros."""

    def __init__(self, mono_samples, channels_in_each_buffer) -> None:
        # Interleaved up front for every buffer layout the device reports, so
        # the IO thread only ever copies.
        self._interleaved_by_channel_count = {
            channels: numpy.ascontiguousarray(
                numpy.repeat(numpy.asarray(mono_samples, dtype="<f4"), channels)
            )
            for channels in set(channels_in_each_buffer)
            if channels > 0
        }
        self.frames_to_play = len(mono_samples)
        self.frames_played = 0
        self.buffers_of_an_unexpected_layout = 0
        self.finished = threading.Event()

    def on_io_cycle(self, _input_buffer_list, _input_time_stamp, output_buffer_list) -> None:
        frames_this_cycle = 0
        for buffer in coreaudio_process_tap.audio_buffers_at(output_buffer_list):
            interleaved = self._interleaved_by_channel_count.get(buffer.number_of_channels)
            if interleaved is None or not buffer.data:
                self.buffers_of_an_unexpected_layout += 1
                continue
            frame_bytes = buffer.number_of_channels * BYTES_PER_FLOAT32_SAMPLE
            frames = buffer.data_byte_size // frame_bytes
            playable = max(0, min(frames, self.frames_to_play - self.frames_played))
            if playable:
                ctypes.memmove(
                    buffer.data,
                    interleaved.ctypes.data + self.frames_played * frame_bytes,
                    playable * frame_bytes,
                )
            ctypes.memset(
                buffer.data + playable * frame_bytes, 0, (frames - playable) * frame_bytes
            )
            frames_this_cycle = max(frames_this_cycle, frames)
        self.frames_played += frames_this_cycle
        if self.frames_played >= self.frames_to_play:
            self.finished.set()


class TappedAudioRecorder:
    """Every input frame a device delivers, placed by the HAL's sample time.

    Placed rather than appended, as `CapturedAudioWaveformRecorder` places
    blocks, so an IO cycle the HAL skipped stays a hole the analysis can see.
    """

    def __init__(self, channels_in_each_buffer, capacity_frames: int) -> None:
        self._channels_in_each_buffer = list(channels_in_each_buffer)
        self._recorded_by_buffer = [
            numpy.zeros(capacity_frames * channels, dtype="<f4")
            for channels in self._channels_in_each_buffer
        ]
        self._capacity_frames = capacity_frames
        self._first_sample_time = None
        self.frames_recorded_through = 0
        self.io_cycles = 0
        self.buffers_of_an_unexpected_layout = 0
        self.frames_past_capacity = 0
        self.first_io_cycle = threading.Event()

    def on_io_cycle(self, input_buffer_list, input_time_stamp, _output_buffer_list) -> None:
        self.io_cycles += 1
        self.first_io_cycle.set()
        buffers = coreaudio_process_tap.audio_buffers_at(input_buffer_list)
        if len(buffers) != len(self._channels_in_each_buffer) or not buffers:
            self.buffers_of_an_unexpected_layout += max(len(buffers), 1)
            return
        sample_time = coreaudio_process_tap.sample_time_at(input_time_stamp)
        if sample_time is None:
            at = self.frames_recorded_through
        else:
            if self._first_sample_time is None:
                self._first_sample_time = sample_time
            at = int(round(sample_time - self._first_sample_time))
        for buffer, channels, recorded in zip(
            buffers, self._channels_in_each_buffer, self._recorded_by_buffer
        ):
            if buffer.number_of_channels != channels or not buffer.data:
                self.buffers_of_an_unexpected_layout += 1
                return
            frame_bytes = channels * BYTES_PER_FLOAT32_SAMPLE
            frames = buffer.data_byte_size // frame_bytes
            if at < 0 or at + frames > self._capacity_frames:
                self.frames_past_capacity += frames
                return
            ctypes.memmove(recorded.ctypes.data + at * frame_bytes, buffer.data, frames * frame_bytes)
            self.frames_recorded_through = max(self.frames_recorded_through, at + frames)

    def mono_waveform(self):
        """Every channel of every buffer mixed down equally, as the engine's recorder does."""
        total_channels = sum(self._channels_in_each_buffer)
        summed = numpy.zeros(self.frames_recorded_through, dtype="<f8")
        if total_channels == 0:
            return summed
        for channels, recorded in zip(self._channels_in_each_buffer, self._recorded_by_buffer):
            summed += (
                recorded[: self.frames_recorded_through * channels]
                .reshape(-1, channels)
                .astype("<f8")
                .sum(axis=1)
            )
        return summed / total_channels


def _require_32_bit_float(stream_formats, what: str) -> None:
    if not stream_formats:
        raise coreaudio_process_tap.CoreAudioFixtureError(f"{what} reports no streams")
    for stream_format in stream_formats:
        if not stream_format.is_32_bit_float:
            raise coreaudio_process_tap.CoreAudioFixtureError(
                f"{what} is {stream_format}, and this peer copies 32-bit float only"
            )


def _raise_for_what_went_wrong_on_the_io_threads(
    player, player_io_proc, recorder, recorder_io_proc
) -> None:
    for role, io_proc in (("player", player_io_proc), ("recorder", recorder_io_proc)):
        if io_proc.exceptions_on_the_io_thread:
            raise coreaudio_process_tap.CoreAudioFixtureError(
                f"the {role}'s IOProc raised {io_proc.exceptions_on_the_io_thread} times, "
                f"first {io_proc.first_exception_on_the_io_thread}"
            )
    for role, counted in (("player", player), ("recorder", recorder)):
        if counted.buffers_of_an_unexpected_layout:
            raise coreaudio_process_tap.CoreAudioFixtureError(
                f"the {role} met {counted.buffers_of_an_unexpected_layout} buffers laid out "
                "unlike the device's stream configuration"
            )


def play_and_record_through_a_process_tap(signal, rate: int, speaker, mute_behaviour: str):
    """Plays `signal` on `speaker` and records it back off a private tap of this process.

    Returns the mono capture, its rate, and a line of evidence.
    """
    scope_output = coreaudio_process_tap.PROPERTY_SCOPE_OUTPUT
    scope_input = coreaudio_process_tap.PROPERTY_SCOPE_INPUT
    _require_32_bit_float(
        coreaudio_process_tap.stream_virtual_formats(speaker.object_id, scope_output),
        f"{speaker.uid}'s output",
    )
    played = numpy.concatenate(
        [signal, numpy.zeros(int(TRAILING_SILENCE_SECONDS * rate), dtype="<f8")]
    )
    player = KnownSignalPlayer(
        played, coreaudio_process_tap.channels_in_each_buffer(speaker.object_id, scope_output)
    )
    played_seconds = len(played) / rate
    aggregate_device_uid = f"streamlib-fixture-process-tap-{os.getpid()}"

    with coreaudio_process_tap.PrivateCaptureDeviceTappingThisProcessesOutput(
        aggregate_device_uid, mute_behaviour, speaker.uid
    ) as process_tap:
        # By UID, as `MicrophoneSource` resolves a device_id: a private
        # aggregate its creator cannot list is one the engine could never open.
        aggregate = coreaudio_process_tap.device_with_uid(aggregate_device_uid)
        if aggregate is None:
            raise coreaudio_process_tap.CoreAudioFixtureError(
                f"the private aggregate {aggregate_device_uid} is not listed to the process "
                "that made it"
            )
        if aggregate.input_channels == 0:
            raise coreaudio_process_tap.CoreAudioFixtureError(
                f"the private aggregate {aggregate_device_uid} has no input channels to "
                "read the tap through"
            )
        _require_32_bit_float(
            coreaudio_process_tap.stream_virtual_formats(aggregate.object_id, scope_input),
            f"{aggregate_device_uid}'s input",
        )
        capture_rate = int(aggregate.nominal_sample_rate)
        recorder = TappedAudioRecorder(
            coreaudio_process_tap.channels_in_each_buffer(aggregate.object_id, scope_input),
            int((played_seconds + PLAYBACK_TIMEOUT_MARGIN_SECONDS + 1.0) * capture_rate),
        )
        evidence = process_tap.evidence()

        with coreaudio_process_tap.RunningAudioDeviceIOProc(
            aggregate.object_id, recorder.on_io_cycle
        ) as recorder_io_proc:
            if not recorder.first_io_cycle.wait(FIRST_IO_CYCLE_TIMEOUT_SECONDS):
                raise coreaudio_process_tap.CoreAudioFixtureError(
                    f"the private aggregate ran no IO cycle within "
                    f"{FIRST_IO_CYCLE_TIMEOUT_SECONDS:.0f} s of starting"
                )
            with coreaudio_process_tap.RunningAudioDeviceIOProc(
                speaker.object_id, player.on_io_cycle
            ) as player_io_proc:
                playback_timeout_seconds = played_seconds + PLAYBACK_TIMEOUT_MARGIN_SECONDS
                if not player.finished.wait(playback_timeout_seconds):
                    raise coreaudio_process_tap.CoreAudioFixtureError(
                        f"the speaker's IOProc played {player.frames_played} of "
                        f"{player.frames_to_play} frames in {playback_timeout_seconds:.1f} s"
                    )

    _raise_for_what_went_wrong_on_the_io_threads(
        player, player_io_proc, recorder, recorder_io_proc
    )
    evidence += (
        f" speaker_device_uid={speaker.uid} aggregate_listed_to_its_creator=yes"
        f" frames_played={player.frames_played} recorder_io_cycles={recorder.io_cycles}"
        f" frames_recorded={recorder.frames_recorded_through}"
        f" frames_past_capacity={recorder.frames_past_capacity}"
    )
    return recorder.mono_waveform(), capture_rate, evidence


def main(argv) -> int:
    if len(argv) != 5 or argv[4] not in coreaudio_process_tap.TAP_MUTE_BEHAVIOURS:
        print(
            "Usage: process_tap_loopback_without_the_engine.py <signal.wav> <captured.wav> "
            "<speaker-device-uid> muted|unmuted",
            file=sys.stderr,
        )
        return 2
    signal_path, captured_path, speaker_device_uid, mute_behaviour = argv[1:5]

    if mute_behaviour == "unmuted" and not coreaudio_process_tap.this_run_is_attended():
        print(
            "SKIP: an unmuted tap plays out loud, so it runs attended only — set "
            f"{coreaudio_process_tap.ATTENDED_RUN_ENVIRONMENT_VARIABLE}=1 with someone listening",
            file=sys.stderr,
        )
        return 77
    refusal = coreaudio_process_tap.refusal_to_create_a_tap(
        coreaudio_process_tap.system_audio_recording_authorization(),
        coreaudio_process_tap.this_run_is_attended(),
    )
    if refusal is not None:
        print(f"SKIP: {refusal}", file=sys.stderr)
        return 77

    speaker = coreaudio_process_tap.device_with_uid(speaker_device_uid)
    if speaker is None or speaker.output_channels == 0:
        print(f"ERROR: no output device has the UID {speaker_device_uid}", file=sys.stderr)
        return 1
    signal, rate = known_audio_signal.read_wav(signal_path)
    if int(speaker.nominal_sample_rate) != rate:
        print(
            f"SKIP: {speaker.uid} runs at {speaker.nominal_sample_rate:.0f} Hz and the signal "
            f"is {rate} Hz; this peer plays it unresampled, so set the speakers to {rate} Hz "
            "in Audio MIDI Setup",
            file=sys.stderr,
        )
        return 77

    try:
        captured, capture_rate, evidence = play_and_record_through_a_process_tap(
            signal, rate, speaker, mute_behaviour
        )
    except coreaudio_process_tap.CoreAudioFixtureError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1
    print(evidence)
    known_audio_signal.write_wav(captured_path, captured, capture_rate)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
