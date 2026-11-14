# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""Rust function nodes, declared from the module's native half."""

from tatolab.stream import node

from rust_function_node_fixture import _native

ScaleAndOffset = node.rust_function(_native.ScaleAndOffset)
CountsItsOwnCalls = node.rust_function(_native.CountsItsOwnCalls)
RustGate = node.rust_function(_native.RustGate)

__all__ = ["CountsItsOwnCalls", "RustGate", "ScaleAndOffset"]
