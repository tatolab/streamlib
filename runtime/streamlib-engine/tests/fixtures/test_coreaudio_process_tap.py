# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The macOS audio fixtures' Core Audio helper, short of creating a tap.

Nothing here creates a tap or an aggregate device, because the first tap raises
the System Audio Recording prompt and so belongs to an attended run. What is
checked is everything before that call: which devices get pinned, the
dictionary the aggregate is described by, and the Objective-C description the
tap would be made from — which is where a wrong `objc_msgSend` prototype would
show.
"""

import os
import sys
import unittest

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


if __name__ == "__main__":
    unittest.main()
