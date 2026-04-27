# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Adapter wrappers for streamlib's surface-share architecture.

Each module under this package mirrors a Rust crate of the form
``streamlib-adapter-<name>``: ``vulkan``, ``opengl`` (#512), ``skia``
(#513), ``cpu_readback`` (#514). The Python module provides type
shapes and convenience wrappers customer code uses against the
subprocess-side native binding (``streamlib-python-native``).
"""

from streamlib.adapters import opengl, vulkan

__all__ = ["opengl", "vulkan"]
