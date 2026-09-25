# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The macOS audio fixtures' Core Audio helper, short of creating a tap.

Nothing here creates a tap or an aggregate device, because the first tap raises
the System Audio Recording prompt and so belongs to an attended run. What is
checked is everything before that call: the TCC gate that decides whether one
may be made at all, which devices get pinned, the dictionary the aggregate is
described by, and the Objective-C description the tap would be made from —
which is where a wrong `objc_msgSend` prototype would show.
"""

import contextlib
import ctypes
import io
import os
import sys
import unittest
from unittest import mock

import numpy

import coreaudio_process_tap as helper


def a_device(**overrides):
    fields = {
        "object_id": 1,
        "uid": "SomeDevice",
        "name": "Some Device",
        "transport_type": "virt",
        "input_channels": 0,
        "output_channels": 0,
        "input_data_source": "",
        "output_data_source": "",
        "nominal_sample_rate": 48_000.0,
    }
    fields.update(overrides)
    return helper.CoreAudioDevice(**fields)


BUILT_IN_SPEAKER = a_device(
    uid="BuiltInSpeakerDevice",
    transport_type="bltn",
    output_channels=2,
    output_data_source="ispk",
)
HEADPHONES_ON_THE_JACK = a_device(
    uid="BuiltInHeadphoneOutputDevice",
    transport_type="bltn",
    output_channels=2,
    output_data_source="hdpn",
)
BUILT_IN_MICROPHONE = a_device(
    uid="BuiltInMicrophoneDevice",
    transport_type="bltn",
    input_channels=1,
    input_data_source="imic",
)
CONTINUITY_IPHONE_MICROPHONE = a_device(
    uid="327566DB-83F2-4AA2-88BD-794300000003", transport_type="ccwd", input_channels=1
)
VIRTUAL_DEVICE_WITH_BOTH_DIRECTIONS = a_device(
    uid="CamoAudioDevice_UID", input_channels=2, output_channels=2
)


class DevicePinning(unittest.TestCase):
    def test_the_built_in_speaker_is_pinned_over_everything_else(self):
        devices = [VIRTUAL_DEVICE_WITH_BOTH_DIRECTIONS, HEADPHONES_ON_THE_JACK, BUILT_IN_SPEAKER]
        self.assertEqual(helper.built_in_speaker_among(devices), BUILT_IN_SPEAKER)

    def test_headphones_are_never_taken_for_the_speaker(self):
        """Headphones on the jack are built in too; only the data source tells them apart."""
        self.assertIsNone(helper.built_in_speaker_among([HEADPHONES_ON_THE_JACK]))

    def test_the_built_in_microphone_is_pinned_over_a_continuity_iphone(self):
        devices = [CONTINUITY_IPHONE_MICROPHONE, VIRTUAL_DEVICE_WITH_BOTH_DIRECTIONS, BUILT_IN_MICROPHONE]
        self.assertEqual(helper.built_in_microphone_among(devices), BUILT_IN_MICROPHONE)

    def test_a_mac_with_no_built_in_microphone_pins_none(self):
        self.assertIsNone(
            helper.built_in_microphone_among(
                [CONTINUITY_IPHONE_MICROPHONE, VIRTUAL_DEVICE_WITH_BOTH_DIRECTIONS]
            )
        )


class AggregateDeviceDescription(unittest.TestCase):
    def setUp(self):
        self.description = helper.aggregate_device_description(
            "streamlib-fixture-process-tap-1", "TAP-UUID", "BuiltInSpeakerDevice"
        )

    def test_the_aggregate_is_private_so_it_dies_with_its_process(self):
        self.assertEqual(self.description["private"], 1)
        self.assertEqual(self.description["uid"], "streamlib-fixture-process-tap-1")

    def test_the_tap_is_read_on_the_clock_of_the_device_it_plays_to(self):
        self.assertEqual(self.description["master"], "BuiltInSpeakerDevice")
        self.assertEqual(self.description["subdevices"], [{"uid": "BuiltInSpeakerDevice"}])
        self.assertEqual(self.description["taps"], [{"uid": "TAP-UUID", "drift": 1}])

    def test_the_aggregate_does_not_wait_for_the_tap_to_start(self):
        """`tapautostart` makes starting the device wait for tapped audio, and the
        graph may start the microphone before the speaker plays anything."""
        self.assertNotIn("tapautostart", self.description)


class FourCharacterCodes(unittest.TestCase):
    def test_a_code_round_trips(self):
        self.assertEqual(helper.four_char_code_text(helper.four_char_code("bltn")), "bltn")

    def test_a_status_that_spells_nothing_reads_as_its_number(self):
        self.assertEqual(helper.four_char_code_text(-50), str(-50))


EVERY_TCC_ANSWER_BUT_A_GRANT = ("denied", "not-determined", "unknown", "", "restricted")


class TapCreationGate(unittest.TestCase):
    """Fails closed: unattended, nothing but a confirmed grant makes a tap."""

    def test_a_grant_makes_a_tap_attended_or_not(self):
        for attended in (False, True):
            with self.subTest(attended=attended):
                self.assertIsNone(helper.refusal_to_create_a_tap("authorized", attended))

    def test_unattended_every_other_answer_refuses(self):
        for answer in EVERY_TCC_ANSWER_BUT_A_GRANT:
            with self.subTest(answer=answer):
                self.assertIsNotNone(helper.refusal_to_create_a_tap(answer, attended=False))

    def test_a_tcc_framework_that_will_not_load_answers_unknown_and_is_refused(self):
        with mock.patch.object(helper, "TCC_PRIVATE_FRAMEWORK", "/nonexistent/TCC"):
            answer = helper.system_audio_recording_authorization()
        self.assertEqual(answer, "unknown")
        self.assertIsNotNone(helper.refusal_to_create_a_tap(answer, attended=False))

    def test_a_tcc_that_will_not_answer_is_named_as_such(self):
        refusal = helper.refusal_to_create_a_tap("unknown", attended=False)
        self.assertIn("TCC would not say", refusal)
        self.assertIn(helper.ATTENDED_RUN_ENVIRONMENT_VARIABLE, refusal)

    def test_attended_only_a_denial_still_refuses(self):
        self.assertIsNotNone(helper.refusal_to_create_a_tap("denied", attended=True))
        for answer in ("not-determined", "unknown", "restricted"):
            with self.subTest(answer=answer):
                self.assertIsNone(helper.refusal_to_create_a_tap(answer, attended=True))


class TapCreationGateOnTheCommandLine(unittest.TestCase):
    def run_the_gate(self, answer, attended):
        environment = {helper.ATTENDED_RUN_ENVIRONMENT_VARIABLE: "1" if attended else ""}
        stdout, stderr = io.StringIO(), io.StringIO()
        with mock.patch.object(
            helper, "system_audio_recording_authorization", return_value=answer
        ), mock.patch.dict(os.environ, environment), contextlib.redirect_stdout(
            stdout
        ), contextlib.redirect_stderr(stderr):
            status = helper.main(["coreaudio_process_tap.py", "authorize-a-tap"])
        return status, stdout.getvalue().strip(), stderr.getvalue()

    def test_an_unanswerable_tcc_is_77_unattended_with_the_answer_kept(self):
        status, answer, reason = self.run_the_gate("unknown", attended=False)
        self.assertEqual((status, answer), (77, "unknown"))
        self.assertTrue(reason.startswith("SKIP: "))

    def test_a_grant_proceeds_unattended(self):
        self.assertEqual(self.run_the_gate("authorized", attended=False)[:2], (0, "authorized"))

    def test_attended_an_unanswered_prompt_proceeds(self):
        self.assertEqual(
            self.run_the_gate("not-determined", attended=True)[:2], (0, "not-determined")
        )


class ExactZerosAttribution(unittest.TestCase):
    def test_zeros_with_the_grant_held_throughout_are_red(self):
        status, reason = helper.why_a_tap_delivered_exact_zeros("authorized", "authorized")
        self.assertEqual(status, 1)
        self.assertTrue(reason.startswith("ERROR: "))

    def test_zeros_with_a_grant_given_during_the_run_are_77(self):
        for before in ("not-determined", "unknown"):
            with self.subTest(before=before):
                self.assertEqual(
                    helper.why_a_tap_delivered_exact_zeros(before, "authorized")[0], 77
                )

    def test_zeros_when_tcc_cannot_say_are_77_and_say_so(self):
        status, reason = helper.why_a_tap_delivered_exact_zeros("authorized", "unknown")
        self.assertEqual(status, 77)
        self.assertIn("would not say", reason)

    def test_zeros_with_no_grant_now_are_77(self):
        for now in ("denied", "not-determined"):
            with self.subTest(now=now):
                self.assertEqual(
                    helper.why_a_tap_delivered_exact_zeros("authorized", now)[0], 77
                )


def an_audio_buffer_list(channel_counts, frames):
    """An AudioBufferList laid out as the HAL hands one to an IOProc, over numpy memory.

    Returns the list and the arrays behind it; both must outlive every read.
    """
    arrays = [numpy.zeros(frames * channels, dtype="<f4") for channels in channel_counts]

    class AudioBufferListOfThisMany(ctypes.Structure):
        _fields_ = [
            ("number_of_buffers", ctypes.c_uint32),
            ("buffers", helper.AudioBuffer * len(channel_counts)),
        ]

    buffer_list = AudioBufferListOfThisMany()
    buffer_list.number_of_buffers = len(channel_counts)
    for index, (channels, array) in enumerate(zip(channel_counts, arrays)):
        buffer_list.buffers[index] = helper.AudioBuffer(channels, array.nbytes, array.ctypes.data)
    return buffer_list, arrays


def an_audio_time_stamp(sample_time, sample_time_is_valid=True):
    """An AudioTimeStamp's 64 bytes with only the sample time and its flag set."""
    stamp = (ctypes.c_uint8 * 64)()
    ctypes.c_double.from_buffer(stamp, 0).value = sample_time
    ctypes.c_uint32.from_buffer(stamp, helper.AUDIO_TIME_STAMP_FLAGS_OFFSET).value = (
        helper.AUDIO_TIME_STAMP_SAMPLE_TIME_VALID if sample_time_is_valid else 0
    )
    return stamp


class AudioBufferListReading(unittest.TestCase):
    def test_an_audio_buffer_is_the_sixteen_bytes_core_audio_lays_out(self):
        self.assertEqual(ctypes.sizeof(helper.AudioBuffer), 16)
        self.assertEqual(helper.AudioBuffer.data.offset, 8)

    def test_every_buffer_is_read_in_place(self):
        buffer_list, arrays = an_audio_buffer_list([2, 1], frames=4)
        buffers = helper.audio_buffers_at(ctypes.addressof(buffer_list))
        self.assertEqual([buffer.number_of_channels for buffer in buffers], [2, 1])
        self.assertEqual([buffer.data_byte_size for buffer in buffers], [32, 16])
        self.assertEqual(buffers[1].data, arrays[1].ctypes.data)

    def test_a_null_list_holds_no_buffers(self):
        self.assertEqual(len(helper.audio_buffers_at(None)), 0)

    def test_a_sample_time_is_read_only_when_flagged_valid(self):
        valid = an_audio_time_stamp(4096.0)
        not_valid = an_audio_time_stamp(4096.0, sample_time_is_valid=False)
        self.assertEqual(helper.sample_time_at(ctypes.addressof(valid)), 4096.0)
        self.assertIsNone(helper.sample_time_at(ctypes.addressof(not_valid)))
        self.assertIsNone(helper.sample_time_at(None))


class StreamFormat(unittest.TestCase):
    def test_only_32_bit_float_linear_pcm_is_copyable(self):
        float32 = helper.AudioStreamFormat(48_000.0, "lpcm", 0b1001, 32, 2)
        self.assertTrue(float32.is_32_bit_float)
        self.assertFalse(float32._replace(bits_per_channel=16).is_32_bit_float)
        self.assertFalse(float32._replace(format_flags=0b1100).is_32_bit_float)
        self.assertFalse(float32._replace(format_id="aac ").is_32_bit_float)


class MuteBehaviour(unittest.TestCase):
    def test_an_unknown_mute_behaviour_is_refused_before_core_audio_is_touched(self):
        with self.assertRaises(ValueError):
            helper.PrivateCaptureDeviceTappingThisProcessesOutput(
                "streamlib-fixture-process-tap-1", "quiet", "BuiltInSpeakerDevice"
            )


@unittest.skipUnless(sys.platform == "darwin", "Core Audio is macOS's")
class CoreAudioShortOfATap(unittest.TestCase):
    def test_this_process_has_a_process_object_to_tap(self):
        self.assertNotEqual(helper.process_object_of(os.getpid()), helper.AUDIO_OBJECT_UNKNOWN)

    def test_the_aggregate_description_reaches_core_foundation_whole(self):
        description = helper.aggregate_device_description(
            "streamlib-fixture-process-tap-1", "TAP-UUID", "BuiltInSpeakerDevice"
        )
        with helper._CoreFoundationObjectsToRelease() as core_foundation_objects:
            rendered = core_foundation_objects.description_of(
                core_foundation_objects.value_from(description)
            )
        for key in ("private", "master", "subdevices", "taps", "drift", "TAP-UUID"):
            self.assertIn(key, rendered)

    def test_a_tap_description_is_private_and_muted_as_asked(self):
        """Only the Objective-C object: it is never handed to the HAL."""
        this_process = helper.process_object_of(os.getpid())
        for mute_behaviour in ("muted", "unmuted"):
            with self.subTest(mute_behaviour=mute_behaviour):
                with helper._PrivateProcessTapDescription(
                    this_process, mute_behaviour, "streamlib fixture unit test"
                ) as tap_description:
                    self.assertTrue(tap_description.is_private())
                    self.assertEqual(tap_description.mute_behaviour(), mute_behaviour)
                    self.assertRegex(tap_description.tap_uid(), r"^[0-9A-F-]{36}$")

    def test_the_system_audio_recording_preflight_answers_without_prompting(self):
        self.assertIn(
            helper.system_audio_recording_authorization(),
            {"authorized", "denied", "not-determined", "unknown"},
        )

    def test_the_built_in_speakers_hand_an_io_proc_32_bit_float(self):
        speaker = helper.built_in_speaker_among(helper.attached_devices())
        if speaker is None:
            self.skipTest("no built-in speakers, or headphones on the jack")
        formats = helper.stream_virtual_formats(speaker.object_id, helper.PROPERTY_SCOPE_OUTPUT)
        self.assertTrue(formats)
        self.assertTrue(all(stream_format.is_32_bit_float for stream_format in formats))
        self.assertEqual(
            sum(helper.channels_in_each_buffer(speaker.object_id, helper.PROPERTY_SCOPE_OUTPUT)),
            speaker.output_channels,
        )


if __name__ == "__main__":
    unittest.main()
