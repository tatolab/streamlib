# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Puts the module's Python on the path, as its installed wheel would, with its native half stood
in for so no GPU is needed."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "python"))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import native_scene_stand_in  # noqa: E402

sys.modules["tatolab.rive._native"] = native_scene_stand_in  # type: ignore[assignment]
