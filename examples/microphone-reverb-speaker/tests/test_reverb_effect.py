# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

import unittest

from processors.reverb_effect import ReverbEffect, ReverbEffectConfig


class ReverbEffectConfigTest(unittest.TestCase):
    def test_default_config_preserves_the_existing_dials(self) -> None:
        effect = ReverbEffect(ReverbEffectConfig())

        self.assertEqual(effect.wet_level, 0.25)
        self.assertEqual(effect.dry_level, 0.7)

    def test_config_values_are_validated(self) -> None:
        with self.assertRaisesRegex(ValueError, "room_size=1.1"):
            ReverbEffect(ReverbEffectConfig(room_size=1.1))


if __name__ == "__main__":
    unittest.main()
