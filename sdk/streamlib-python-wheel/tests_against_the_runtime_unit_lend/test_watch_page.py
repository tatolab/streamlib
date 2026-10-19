# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The runtime unit carries the preview page beside its lend.

`tatolabd` serves `lib/tatolab/watch/` under its loopback HTTP's `/watch`
route, answering the entry file at every address below it, so the entry file
loads its bundle by an absolute path under that route.
"""

import json
import re
from pathlib import Path

from test_wheel_portability import runtime_unit_lend_directory

WATCH_PAGE_ROUTE_PREFIX = "/watch/"


def watch_page_directory() -> Path:
    return runtime_unit_lend_directory().parent / "watch"


def test_the_unit_stages_the_preview_page_beside_its_lend():
    assert (watch_page_directory() / "index.html").is_file(), watch_page_directory()


def test_every_script_the_entry_file_loads_is_staged_under_the_watch_route():
    entry_file = (watch_page_directory() / "index.html").read_text()
    script_sources = re.findall(r'<script[^>]*\ssrc="([^"]+)"', entry_file)

    assert script_sources, "the entry file loads no script"
    for script_source in script_sources:
        assert script_source.startswith(WATCH_PAGE_ROUTE_PREFIX), script_source
        staged_script = watch_page_directory() / script_source.removeprefix(WATCH_PAGE_ROUTE_PREFIX)
        assert staged_script.is_file() and staged_script.stat().st_size > 0, staged_script


def test_the_opus_decoder_polyfill_is_staged_as_its_own_files_the_import_map_names():
    entry_file = (watch_page_directory() / "index.html").read_text()
    import_map_script = re.search(r'<script type="importmap">(.*?)</script>', entry_file, re.DOTALL)
    assert import_map_script is not None, "the entry file has no import map"
    import_map = json.loads(import_map_script.group(1))
    polyfill_entry_module = import_map["imports"]["@libav.js/variant-opus-af"]

    assert polyfill_entry_module.startswith(WATCH_PAGE_ROUTE_PREFIX), polyfill_entry_module
    assert (watch_page_directory() / polyfill_entry_module.removeprefix(WATCH_PAGE_ROUTE_PREFIX)).is_file()
    for suffix in (".wasm.mjs", ".wasm.wasm"):
        assert len(list(watch_page_directory().glob(f"libav-*{suffix}"))) == 1, suffix
