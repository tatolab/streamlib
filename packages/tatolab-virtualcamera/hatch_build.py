# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The sdist carries the file each symlinked include points at, never the link.

`LICENSE` here links to the repository's own. Archived as a link it dangles
once the sdist leaves the repository, and a wheel built from that sdist ships
no licence.
"""

from __future__ import annotations

import os
from typing import Any

from hatchling.builders.hooks.plugin.interface import BuildHookInterface


class SdistIncludesResolvedFromTheirSymlinksBuildHook(BuildHookInterface):
    """Point every force-included symlink at the file it links to."""

    def initialize(self, version: str, build_data: dict[str, Any]) -> None:
        force_included_distribution_path_by_source_path: dict[str, str] = build_data[
            "force_include"
        ]
        for source_path, distribution_path in list(
            force_included_distribution_path_by_source_path.items()
        ):
            if os.path.islink(source_path):
                del force_included_distribution_path_by_source_path[source_path]
                force_included_distribution_path_by_source_path[
                    os.path.realpath(source_path)
                ] = distribution_path
