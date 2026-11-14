# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The stream's own package: its nodes, their shaders, and what they share.

Importing this package must stay free of side effects — every node runs in its
own child interpreter, which imports its class by name from here.
"""
