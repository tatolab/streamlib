# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""Packets units a test publishes: one bare access unit or Opus packet each."""

ANNEX_B_ACCESS_UNIT = b"\x00\x00\x00\x01\x67\x42\xe0\x1f\x00\x00\x00\x01\x65\x88\x84"
OPUS_PACKET = b"\x78\x01\x02\x03"

#: A mono `OpusHead` (RFC 7845 §5.1): version 1, one channel, no pre-skip,
#: 48 kHz, no gain, mapping family 0.
MONO_OPUS_IDENTIFICATION_HEADER_HEX = "4f707573486561640101000080bb0000000000"
