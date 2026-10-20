# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The stream's own package: its nodes, their shaders, and what they share.

Importing this package must stay free of side effects — every node runs in its
own child interpreter, which imports its class by name from here.
"""
