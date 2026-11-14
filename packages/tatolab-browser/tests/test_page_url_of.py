# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""A `BrowserSource` `url` without a scheme names a page file inside the project."""

from pathlib import Path

import pytest

from tatolab.browser.browser_source import page_url_of


def test_a_url_with_a_scheme_is_opened_as_written() -> None:
    for url in ("https://example.com/page", "file:///tmp/page.html", "data:text/html,<p>hi</p>"):
        assert page_url_of(url) == url


def test_a_path_without_a_scheme_is_the_project_file_it_names(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (tmp_path / "templates").mkdir()
    page = tmp_path / "templates" / "lower_third.html"
    page.write_text("<p>lower third</p>")
    monkeypatch.chdir(tmp_path)
    assert page_url_of("templates/lower_third.html") == page.resolve().as_uri()


def test_a_path_naming_no_file_is_refused_naming_it(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    with pytest.raises(FileNotFoundError, match="missing_page.html"):
        page_url_of("missing_page.html")
