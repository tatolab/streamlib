# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`ProjectAssetUuid`: a config key whose string is a project asset's UUID, written on its annotation."""

from __future__ import annotations

import typing
from typing import Any, Literal, Optional

__all__ = ["ProjectAssetMedia", "ProjectAssetUuid"]

#: A member of the asset's `media_facts` that `list_assets` serves, naming what its bytes hold.
ProjectAssetMedia = Literal["image", "video", "audio", "lut", "scene"]

_EVERY_PROJECT_ASSET_MEDIA: "tuple[str, ...]" = typing.get_args(ProjectAssetMedia)

#: The schema keyword a config key holding a project asset's UUID carries. Its value is an
#: object; its `media` key, when present, lists the `media_facts` members of which the asset
#: must carry at least one, and when absent the key takes any asset of the project.
PROJECT_ASSET_UUID_SCHEMA_KEYWORD = "x-tatolab-project-asset"


class ProjectAssetUuid:
    """A config key holding a project asset's UUID: `Annotated[str, ProjectAssetUuid(media=("image",))]`.

    The derived schema carries `x-tatolab-project-asset`, so a client offers the project's
    assets to pick from rather than a text box. `media` narrows them to the assets whose bytes
    hold one of those media; left out, every asset of the project is offered.
    """

    __slots__ = ("_media",)

    def __init__(self, *, media: "Optional[tuple[ProjectAssetMedia, ...]]" = None) -> None:
        self._media = _refuse_media_the_runtime_does_not_read(media)

    @property
    def media(self) -> "Optional[tuple[str, ...]]":
        """The media an asset must hold, or `None` when any asset of the project is taken."""
        return self._media

    def json_schema_keywords(self) -> "dict[str, Any]":
        """The `x-tatolab-project-asset` keyword this marker writes into a key's schema."""
        keyword: "dict[str, Any]" = {}
        if self._media is not None:
            keyword["media"] = list(self._media)
        return {PROJECT_ASSET_UUID_SCHEMA_KEYWORD: keyword}

    def __eq__(self, other: object) -> bool:
        return isinstance(other, ProjectAssetUuid) and other._media == self._media

    def __hash__(self) -> int:
        return hash((ProjectAssetUuid, self._media))

    def __repr__(self) -> str:
        return f"ProjectAssetUuid(media={self._media!r})"


def _refuse_media_the_runtime_does_not_read(media: object) -> "Optional[tuple[str, ...]]":
    if media is None:
        return None
    if isinstance(media, str) or not isinstance(media, (tuple, list)):
        raise TypeError(
            f"ProjectAssetUuid(...) takes `media` as a tuple of media names, got {media!r}; "
            f"write `media=(\"image\",)` for one."
        )
    if not media:
        raise ValueError(
            "ProjectAssetUuid(media=()) takes no asset at all; leave `media` out to take any "
            "asset of the project."
        )
    unknown = [entry for entry in media if entry not in _EVERY_PROJECT_ASSET_MEDIA]
    if unknown:
        raise ValueError(
            f"ProjectAssetUuid(...) names {unknown!r}, which the runtime reads no media facts "
            f"for; `media` takes {', '.join(_EVERY_PROJECT_ASSET_MEDIA)}."
        )
    return tuple(dict.fromkeys(media))


def project_asset_uuid_an_annotation_carries(
    annotation: Any, marked: str
) -> Optional[ProjectAssetUuid]:
    """The `ProjectAssetUuid` an `Annotated[...]` carries, or `None`.

    `marked` names what the annotation types, for the refusal of two markers.
    """
    if typing.get_origin(annotation) is not typing.Annotated:
        return None
    markers = [
        entry for entry in typing.get_args(annotation)[1:] if isinstance(entry, ProjectAssetUuid)
    ]
    if len(markers) > 1:
        raise TypeError(f"{marked} carries {len(markers)} `ProjectAssetUuid`s; it takes one.")
    return markers[0] if markers else None
