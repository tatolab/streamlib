# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The recorder's block placement, checked with no device and no engine."""

import unittest

import numpy

from captured_audio_waveform_recorder import waveform_placed_by_start_sample


class WaveformPlacedByStartSample(unittest.TestCase):
    def test_a_gap_between_blocks_stays_a_gap_of_silence(self):
        first_block = numpy.full(4, 0.5)
        second_block = numpy.full(3, -0.25)

        waveform = waveform_placed_by_start_sample(
            [(1000, first_block), (1010, second_block)]
        )

        self.assertEqual(len(waveform), 13)
        numpy.testing.assert_array_equal(waveform[0:4], first_block)
        numpy.testing.assert_array_equal(waveform[4:10], numpy.zeros(6))
        numpy.testing.assert_array_equal(waveform[10:13], second_block)

    def test_adjacent_blocks_join_with_no_gap(self):
        waveform = waveform_placed_by_start_sample(
            [(0, numpy.ones(2)), (2, numpy.full(2, 2.0))]
        )

        numpy.testing.assert_array_equal(waveform, [1.0, 1.0, 2.0, 2.0])


if __name__ == "__main__":
    unittest.main()
