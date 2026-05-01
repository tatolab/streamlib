# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Adapter wrappers for streamlib's surface-share architecture.

Each module under this package mirrors a Rust crate of the form
``streamlib-adapter-<name>``: ``vulkan``, ``opengl`` (#512), ``skia``
(#513), ``cpu_readback`` (#514), ``cuda`` (#587 / #589). The Python
module provides type shapes and convenience wrappers customer code
uses against the subprocess-side native binding
(``streamlib-python-native``).
"""

from streamlib.adapters import cpu_readback, cuda, opengl, skia, vulkan

__all__ = ["cpu_readback", "cuda", "opengl", "skia", "vulkan"]
