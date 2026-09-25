# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The rig peer's player and recorder, short of a tap.

The IO-thread halves are driven here over AudioBufferLists laid out as the HAL
lays them out, so what they copy is checked without a device — including a
whole known signal carried player to recorder, seen green and seen red. The
one run on a real device plays zeros, so there is nothing to hear and nothing
to prompt for.
"""

import ctypes
import os
import sys
import tempfile
import unittest

import numpy

import coreaudio_process_tap as helper
import known_audio_signal
import process_tap_loopback_without_the_engine as peer
from test_coreaudio_process_tap import an_audio_buffer_list, an_audio_time_stamp

FRAMES_PER_IO_CYCLE = 512


class KnownSignalPlayer(unittest.TestCase):
    def test_the_signal_goes_into_every_channel_and_then_zeros(self):
        player = peer.KnownSignalPlayer(numpy.array([0.25, -0.5, 0.75]), [2])
        buffer_list, (played,) = an_audio_buffer_list([2], frames=2)
        played[:] = 9.0
        player.on_io_cycle(None, None, ctypes.addressof(buffer_list))
        self.assertEqual(played.tolist(), [0.25, 0.25, -0.5, -0.5])
        self.assertFalse(player.finished.is_set())

        played[:] = 9.0
        player.on_io_cycle(None, None, ctypes.addressof(buffer_list))
        self.assertEqual(played.tolist(), [0.75, 0.75, 0.0, 0.0])
        self.assertTrue(player.finished.is_set())
        self.assertEqual(player.frames_played, 4)

    def test_a_buffer_of_a_layout_the_device_did_not_report_is_counted_not_written(self):
        player = peer.KnownSignalPlayer(numpy.ones(8), [2])
        buffer_list, (played,) = an_audio_buffer_list([3], frames=2)
        player.on_io_cycle(None, None, ctypes.addressof(buffer_list))
        self.assertEqual(player.buffers_of_an_unexpected_layout, 1)
        self.assertFalse(played.any())


class TappedAudioRecorder(unittest.TestCase):
    def record_a_cycle(self, recorder, samples, sample_time):
        buffer_list, (delivered,) = an_audio_buffer_list([2], frames=len(samples) // 2)
        delivered[:] = samples
        stamp = an_audio_time_stamp(sample_time)
        recorder.on_io_cycle(ctypes.addressof(buffer_list), ctypes.addressof(stamp), None)

    def test_channels_are_mixed_down_equally(self):
        recorder = peer.TappedAudioRecorder([2], capacity_frames=16)
        self.record_a_cycle(recorder, [0.5, 0.25, -1.0, 0.0], sample_time=1_000.0)
        self.assertEqual(recorder.mono_waveform().tolist(), [0.375, -0.5])
        self.assertTrue(recorder.first_io_cycle.is_set())

    def test_a_cycle_the_hal_skipped_stays_a_hole(self):
        recorder = peer.TappedAudioRecorder([2], capacity_frames=16)
        self.record_a_cycle(recorder, [1.0] * 4, sample_time=1_000.0)
        self.record_a_cycle(recorder, [1.0] * 4, sample_time=1_004.0)
        self.assertEqual(recorder.mono_waveform().tolist(), [1.0, 1.0, 0.0, 0.0, 1.0, 1.0])

    def test_a_cycle_past_capacity_is_counted_not_written(self):
        recorder = peer.TappedAudioRecorder([2], capacity_frames=3)
        self.record_a_cycle(recorder, [1.0] * 4, sample_time=0.0)
        self.record_a_cycle(recorder, [1.0] * 4, sample_time=2.0)
        self.assertEqual(recorder.frames_past_capacity, 2)
        self.assertEqual(recorder.frames_recorded_through, 2)

    def test_a_cycle_with_no_input_buffers_is_counted(self):
        recorder = peer.TappedAudioRecorder([2], capacity_frames=16)
        recorder.on_io_cycle(None, None, None)
        self.assertEqual(recorder.buffers_of_an_unexpected_layout, 1)


class TheKnownSignalFromPlayerToRecorder(unittest.TestCase):
    """What the player writes, handed to the recorder as a digital tap would hand it."""

    def carry(self, skip_the_recorders_cycle_at_seconds=None):
        signal = known_audio_signal.generate_signal()
        player = peer.KnownSignalPlayer(signal, [2])
        recorder = peer.TappedAudioRecorder([2], capacity_frames=len(signal) * 2)
        skipped_cycle = (
            int(skip_the_recorders_cycle_at_seconds * known_audio_signal.SAMPLE_RATE)
            // FRAMES_PER_IO_CYCLE
            if skip_the_recorders_cycle_at_seconds is not None
            else None
        )
        cycle = 0
        while not player.finished.is_set():
            buffer_list, _ = an_audio_buffer_list([2], frames=FRAMES_PER_IO_CYCLE)
            player.on_io_cycle(None, None, ctypes.addressof(buffer_list))
            if cycle != skipped_cycle:
                stamp = an_audio_time_stamp(48_000.0 + cycle * FRAMES_PER_IO_CYCLE)
                recorder.on_io_cycle(
                    ctypes.addressof(buffer_list), ctypes.addressof(stamp), None
                )
            cycle += 1
        with tempfile.TemporaryDirectory() as directory:
            captured = os.path.join(directory, "captured.wav")
            known_audio_signal.write_wav(
                captured, recorder.mono_waveform(), known_audio_signal.SAMPLE_RATE
            )
            return known_audio_signal.analyse(
                captured, os.path.join(directory, "spectrogram.png")
            )

    def test_a_clean_carry_passes_the_digital_analysis(self):
        report = self.carry()
        self.assertEqual(report["verdict"], "PASS", report["failed"])

    def test_one_skipped_cycle_inside_the_tone_fails_it(self):
        report = self.carry(skip_the_recorders_cycle_at_seconds=0.7)
        self.assertEqual(report["verdict"], "FAIL")
        self.assertIn("silent_stretch_ms", report["failed"])


@unittest.skipUnless(sys.platform == "darwin", "Core Audio is macOS's")
class AnIOProcOnTheBuiltInSpeakers(unittest.TestCase):
    """Plays zeros only: the IOProc glue on the real HAL, silent and prompt-free."""

    def test_a_player_of_zeros_runs_to_its_end(self):
        speaker = helper.built_in_speaker_among(helper.attached_devices())
        if speaker is None:
            self.skipTest("no built-in speakers, or headphones on the jack")
        zeros = numpy.zeros(int(0.25 * speaker.nominal_sample_rate))
        player = peer.KnownSignalPlayer(
            zeros, helper.channels_in_each_buffer(speaker.object_id, helper.PROPERTY_SCOPE_OUTPUT)
        )
        with helper.RunningAudioDeviceIOProc(speaker.object_id, player.on_io_cycle) as io_proc:
            self.assertTrue(player.finished.wait(5.0))
        self.assertEqual(
            io_proc.exceptions_on_the_io_thread, 0, io_proc.first_exception_on_the_io_thread
        )
        self.assertEqual(player.buffers_of_an_unexpected_layout, 0)
        self.assertGreaterEqual(player.frames_played, player.frames_to_play)


if __name__ == "__main__":
    unittest.main()
