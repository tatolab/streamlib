# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only
# streamlib:lint-logging:allow-file — stdout carries the one JSON document the runtime
# parses and stderr what the generator prints; neither is a log event.

"""The entry the runtime runs in a project's own interpreter to run one graph generator.

The runtime starts it as `<venv python> -I -m tatolab.stream._graph_generator_run_entry
GENERATOR [--name NAME] [--settings JSON]` with the project directory as the working
directory. `GENERATOR` is `<module>:<function>`, imported with the project directory first
on `sys.path`. Whatever the project's code does, stdout carries exactly one JSON object and
the exit code is 0. The object is one of:

- `{"generator", "placed"}` — the fragment node the generator's run is stored as: its
  `name`, its generator as `type`, its promoted settings as `config` and its `fragment`;
- `{"generator", "failed_because"}` — the module did not import, `GENERATOR` names no
  `@graph_generator`, or the generator refused or raised, with its traceback.

`-I` keeps every `PYTHON*` variable and the user's site-packages out, so the venv alone
decides what imports.
"""

from __future__ import annotations

import argparse
import importlib
import json
import os
import sys
import traceback
from collections.abc import Sequence
from pathlib import Path
from typing import Any, Optional

from . import _stream_graph_builder
from ._stream_graph_builder import is_graph_generator, place_graph_generator

__all__ = ["main"]

GENERATOR_DOCUMENT_KEY = "generator"
PLACED_FRAGMENT_NODE_DOCUMENT_KEY = "placed"
GENERATOR_FAILED_BECAUSE_DOCUMENT_KEY = "failed_because"


def build_argument_parser() -> argparse.ArgumentParser:
    """The entry's arguments: the generator, the fragment's name and its promoted settings."""
    parser = argparse.ArgumentParser(
        prog="python -I -m tatolab.stream._graph_generator_run_entry",
        description=(
            "Run one @graph_generator function of the project in the working directory as a "
            "fragment, writing the fragment node it is stored as, or why there is none, to "
            "stdout as one JSON object."
        ),
    )
    parser.add_argument(
        "generator",
        metavar="GENERATOR",
        help="The generator by import path: `<module>:<function>`.",
    )
    parser.add_argument(
        "--name",
        dest="fragment_name",
        metavar="NAME",
        help="The fragment's name; the generator's own name when omitted.",
    )
    parser.add_argument(
        "--settings",
        dest="promoted_settings_json",
        metavar="JSON",
        default="{}",
        help="The generator's promoted settings, as one JSON object.",
    )
    return parser


def _generator_failure_described(generator_failure: BaseException) -> str:
    """A failure as the project's own traceback, the frames of this entry and the builder
    dropped from its head."""
    launcher_files = {__file__, _stream_graph_builder.__file__, importlib.__file__}
    failure_traceback = generator_failure.__traceback__
    while (
        failure_traceback is not None
        and failure_traceback.tb_frame.f_code.co_filename in launcher_files
    ):
        failure_traceback = failure_traceback.tb_next
    return "".join(
        traceback.format_exception(type(generator_failure), generator_failure, failure_traceback)
    ).rstrip("\n")


def _the_generator_named(generator: str) -> Any:
    module_name, separator, function_name = generator.partition(":")
    if not (module_name and separator and function_name):
        raise ValueError(
            f"a graph generator is named by its import path, `<module>:<function>`; got "
            f"`{generator}`"
        )
    module = importlib.import_module(module_name)
    generator_function = getattr(module, function_name, None)
    if not is_graph_generator(generator_function):
        defined = sorted(
            f"`{module_name}:{name}`"
            for name, value in vars(module).items()
            if is_graph_generator(value) and getattr(value, "__module__", None) == module_name
        )
        raise ValueError(
            f"`{generator}` is not a @graph_generator function. Module `{module_name}` "
            f"defines: {', '.join(defined) if defined else 'no graph generator'}"
        )
    return generator_function


def run_the_named_generator(
    generator: str, fragment_name: Optional[str], promoted_settings_json: str
) -> dict[str, Any]:
    """The document of one run of `generator`: the fragment node it placed, or why it placed
    none."""
    try:
        promoted_settings = json.loads(promoted_settings_json)
        if not isinstance(promoted_settings, dict):
            raise ValueError(
                f"a generator's promoted settings are one JSON object; got "
                f"{promoted_settings_json}"
            )
        placed = place_graph_generator(
            _the_generator_named(generator),
            name=fragment_name,
            promoted_settings=promoted_settings,
        )
    except Exception as generator_failure:  # noqa: BLE001 — reported as the project's own
        return {
            GENERATOR_DOCUMENT_KEY: generator,
            GENERATOR_FAILED_BECAUSE_DOCUMENT_KEY: _generator_failure_described(
                generator_failure
            ),
        }
    return {GENERATOR_DOCUMENT_KEY: generator, PLACED_FRAGMENT_NODE_DOCUMENT_KEY: placed}


def _carry_every_later_stdout_write_to_stderr() -> int:
    """Point fd 1 and `sys.stdout` at stderr for the rest of the process, returning a
    descriptor on the real stdout, the only way the document reaches it."""
    sys.stdout.flush()
    document_stdout_descriptor = os.dup(sys.stdout.fileno())
    os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
    sys.stdout = sys.stderr
    return document_stdout_descriptor


def main(argv: Optional[Sequence[str]] = None) -> int:
    """Run the requested generator and write its document to stdout; return the exit code."""
    arguments = build_argument_parser().parse_args(argv)
    sys.path.insert(0, str(Path.cwd()))
    document_stdout_descriptor = _carry_every_later_stdout_write_to_stderr()
    try:
        document = run_the_named_generator(
            arguments.generator,
            arguments.fragment_name,
            arguments.promoted_settings_json,
        )
    except BaseException:
        os.close(document_stdout_descriptor)
        raise
    with os.fdopen(document_stdout_descriptor, "wb") as document_stdout:
        document_stdout.write((json.dumps(document, allow_nan=False) + "\n").encode("utf-8"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
