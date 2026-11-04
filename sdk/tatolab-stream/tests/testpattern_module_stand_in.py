# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A stand-in for the `tatolab.testpattern` the scaffold imports, which a project's
`uv sync` installs and this package's suite does not.

It declares the module source's execution and port and nothing else; the module's
own suite holds the real declaration. Written beside a project, it joins the
installed `tatolab` namespace package the compile imports from.
"""

from pathlib import Path

TESTPATTERN_MODULE_PACKAGE_STAND_IN_SOURCE = (
    "from tatolab.stream import node\n"
    "\n"
    "\n"
    '@node(execution="continuous", interval_ms=33)\n'
    "class TestPatternSource:\n"
    "    __test__ = False\n"
    "\n"
    "    @node.output()\n"
    "    def video(self) -> None: ...\n"
)

TESTPATTERN_MODULE_NODE_TYPE = "tatolab.testpattern:TestPatternSource"


def write_the_testpattern_module_stand_in_into(project_directory: Path) -> None:
    """Write the `tatolab/testpattern/` package, laid out as the module's, under `project_directory`."""
    stand_in_package = project_directory / "tatolab" / "testpattern"
    if stand_in_package.exists():
        return
    stand_in_package.mkdir(parents=True)
    (stand_in_package / "__init__.py").write_text(TESTPATTERN_MODULE_PACKAGE_STAND_IN_SOURCE)
