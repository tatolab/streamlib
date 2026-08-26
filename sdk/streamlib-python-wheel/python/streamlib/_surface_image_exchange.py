# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Surface ids in, PNG files on disk out — the `exchange` verb's two forms.

The id form is one call. The channel form is the rxjs shape: `tap` is the
observable and `exchange` is an operator applied to it, composed here at the
consumer and nowhere else. The node is never asked to read a bag — it forwards
bags verbatim, this decodes them, reads whatever field the caller says carries a
surface id, and exchanges that id on its own.

The composition runs in the CLI's own process on purpose. The measured 60 ms
round-trip was a CLI process spawning and connecting per frame, not the
operation; holding one CLI process across the whole sample is what keeps a
sampled frame inside the pool-depth window instead of outwaiting it.

A frame whose pool slot was recycled before the exchange reached it is a retry
against a newer bag, never a silent skip: the run reports every id it retried,
so a short sample can never read as a full one.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, NamedTuple, Optional

from ._control_plane_client import (
    ControlPlaneError,
    ExchangedSurfaceImage,
    SurfaceImageExchangeRefusal,
    call_tool,
    fetch_surface_image_png_bytes,
)
from ._engine import decode_tapped_channel_bag_frame_to_python_object

__all__ = [
    "DEFAULT_SURFACE_ID_BAG_FIELD_NAME",
    "TappedBagFrame",
    "SampledChannelExchangeReport",
    "exchange_one_published_surface_id_into_directory",
    "sample_channel_into_exchanged_surface_images",
]

#: The bag field the channel form reads a surface id out of unless the caller
#: names another. Declared, never guessed: the engine inspects no bag content
#: anywhere, so which field carries an id is the consumer's own knowledge.
DEFAULT_SURFACE_ID_BAG_FIELD_NAME = "surface_id"

#: How many tap rounds one channel-form run will spend before giving up. Each
#: round costs the tap tool's own bounded sample window, so this bounds the
#: whole verb; without it a channel that publishes ids whose frames always
#: recycle first would retry forever.
MAX_TAP_ROUNDS_PER_SAMPLE_RUN = 8

#: Characters kept verbatim in a written file's name. A pooled frame id is
#: `<slot>#<generation>`, and `#` is not one of them.
FILE_NAME_SAFE_CHARACTERS = frozenset(
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-"
)


class TappedBagFrame(NamedTuple):
    """One bag a tap forwarded, and whether the tool capped its preview.

    The cap travels with the bag rather than failing the round that carried it:
    whether a bag this client cannot read matters at all depends on whether the
    stride goes on to select it.
    """

    framed_bytes: bytes
    byte_len_when_preview_was_capped: "Optional[int]"


class SampledChannelExchangeReport(NamedTuple):
    """What one channel-form run exchanged, and what it had to retry.

    `stopped_early_because` carries a failure the run could not compose past —
    a refusal that is not a recycled frame, a channel that stopped answering.
    It is reported rather than raised so the frames that did land are still
    named: a PNG on disk whose path was never printed is evidence a harness
    cannot use and a human will not find.
    """

    written_image_paths: "list[Path]"
    retried_recycled_surface_ids: "list[str]"
    bags_missing_the_surface_id_field: int
    bags_examined: int
    tap_rounds: int
    stopped_early_because: "Optional[str]" = None


def _file_name_stem_for_surface_id(published_surface_id: str) -> str:
    """A surface id as a file-name stem, with everything unsafe folded to `_`."""
    stem = "".join(
        character if character in FILE_NAME_SAFE_CHARACTERS else "_"
        for character in published_surface_id
    )
    return stem or "surface"


def _write_exchanged_surface_image(
    output_directory: Path, file_name: str, exchanged: ExchangedSurfaceImage
) -> Path:
    """Write one exchanged frame into `output_directory` and return its path."""
    output_directory.mkdir(parents=True, exist_ok=True)
    image_path = output_directory / file_name
    image_path.write_bytes(exchanged.png_image_bytes)
    return image_path


def exchange_one_published_surface_id_into_directory(
    url: str, published_surface_id: str, output_directory: Path
) -> Path:
    """Exchange one id for its frame's exact pixels, written as a PNG."""
    exchanged = fetch_surface_image_png_bytes(url, published_surface_id)
    return _write_exchanged_surface_image(
        output_directory,
        f"{_file_name_stem_for_surface_id(published_surface_id)}.png",
        exchanged,
    )


def _tapped_bag_frames(
    url: str, channel: str, requested_bag_count: int
) -> "list[TappedBagFrame]":
    """One `tap` call's bags, as the framed bytes the channel carried.

    The tool hex-encodes a bounded prefix of each bag, and says so. That flag is
    carried per bag, not raised here: a bag the stride never selects is a bag
    this run never needed, and failing the whole round on one would also throw
    away the readable bags that came before it.
    """
    result_text = call_tool(
        url, "tap", {"channel": channel, "count": max(1, requested_bag_count)}
    )
    try:
        tap_tool_result = json.loads(result_text)
    except ValueError as decode_failure:
        raise ControlPlaneError(
            f"tap of `{channel}` returned a non-JSON result: {result_text}"
        ) from decode_failure

    bags = (
        tap_tool_result.get("bags") if isinstance(tap_tool_result, dict) else None
    )
    if not isinstance(bags, list):
        raise ControlPlaneError(
            f"tap of `{channel}` returned no `bags` array: {result_text}"
        )

    frames: "list[TappedBagFrame]" = []
    for bag in bags:
        hex_preview = bag.get("hex_preview") if isinstance(bag, dict) else None
        if not isinstance(hex_preview, str):
            raise ControlPlaneError(
                f"tap of `{channel}` returned a bag with no hex preview: {bag!r}"
            )
        try:
            framed_bytes = bytes.fromhex(hex_preview)
        except ValueError as decode_failure:
            raise ControlPlaneError(
                f"tap of `{channel}` returned a bag whose hex preview does not decode: "
                f"{decode_failure}"
            ) from decode_failure
        frames.append(
            TappedBagFrame(
                framed_bytes=framed_bytes,
                byte_len_when_preview_was_capped=(
                    bag.get("byte_len") if bag.get("hex_truncated") else None
                ),
            )
        )
    return frames


def _surface_id_in_bag(
    framed_bag_bytes: bytes, channel: str, surface_id_bag_field_name: str
) -> "Optional[str]":
    """The named field's value from one tapped bag, or `None` when it has none.

    A bag that will not decode at all stops the run rather than being counted as
    a bag without the field: the second reads as "this channel does not publish
    ids", which would be a lie about a channel this client simply could not read.
    """
    try:
        bag = decode_tapped_channel_bag_frame_to_python_object(framed_bag_bytes)
    except ValueError as decode_failure:
        raise ControlPlaneError(
            f"a bag from `{channel}` could not be decoded: {decode_failure}"
        ) from decode_failure

    if not isinstance(bag, dict):
        return None
    published_surface_id: "Any" = bag.get(surface_id_bag_field_name)
    return published_surface_id if isinstance(published_surface_id, str) else None


def sample_channel_into_exchanged_surface_images(
    url: str,
    channel: str,
    output_directory: Path,
    *,
    wanted_image_count: int,
    every_nth_bag: int,
    surface_id_bag_field_name: str,
) -> SampledChannelExchangeReport:
    """Tap `channel`, exchange the sampled bags' surface ids, write the PNGs.

    The stride counts bags this client received, and runs across the whole run
    rather than restarting per tap round — otherwise a run that needed a second
    round would exchange two adjacent frames while reporting a stride. It is
    not a stride over the channel: each round is a fresh attach, so an unknown
    number of bags flow by between rounds and are never counted.
    """
    written_image_paths: "list[Path]" = []
    retried_recycled_surface_ids: "list[str]" = []
    bags_missing_the_surface_id_field = 0
    bags_examined = 0
    tap_rounds = 0
    stopped_early_because: "Optional[str]" = None

    try:
        while (
            len(written_image_paths) < wanted_image_count
            and tap_rounds < MAX_TAP_ROUNDS_PER_SAMPLE_RUN
        ):
            tap_rounds += 1
            still_wanted = wanted_image_count - len(written_image_paths)
            for tapped_bag in _tapped_bag_frames(
                url, channel, still_wanted * every_nth_bag
            ):
                selected = bags_examined % every_nth_bag == 0
                bags_examined += 1
                if not selected:
                    continue

                if tapped_bag.byte_len_when_preview_was_capped is not None:
                    raise ControlPlaneError(
                        f"a bag the sample selected on `{channel}` is "
                        f"{tapped_bag.byte_len_when_preview_was_capped} bytes, past the "
                        f"prefix `tap` previews, so its surface id cannot be read from "
                        f"here. Exchange an id from this channel directly: "
                        f"`streamlib exchange <surface-id> --out <dir>`."
                    )

                published_surface_id = _surface_id_in_bag(
                    tapped_bag.framed_bytes, channel, surface_id_bag_field_name
                )
                if published_surface_id is None:
                    bags_missing_the_surface_id_field += 1
                    continue

                try:
                    exchanged = fetch_surface_image_png_bytes(url, published_surface_id)
                except SurfaceImageExchangeRefusal as refusal:
                    # A recycled frame is the one refusal that composes: the id
                    # was real and its slot has moved on, so the next bag is the
                    # answer. Every other refusal will answer the same forever.
                    if not refusal.names_a_recycled_frame:
                        raise
                    retried_recycled_surface_ids.append(published_surface_id)
                    continue

                written_image_paths.append(
                    _write_exchanged_surface_image(
                        output_directory,
                        f"{len(written_image_paths):04d}-"
                        f"{_file_name_stem_for_surface_id(published_surface_id)}.png",
                        exchanged,
                    )
                )
                if len(written_image_paths) == wanted_image_count:
                    break
    except (ControlPlaneError, OSError) as failure:
        stopped_early_because = str(failure)

    return SampledChannelExchangeReport(
        written_image_paths=written_image_paths,
        retried_recycled_surface_ids=retried_recycled_surface_ids,
        bags_missing_the_surface_id_field=bags_missing_the_surface_id_field,
        bags_examined=bags_examined,
        tap_rounds=tap_rounds,
        stopped_early_because=stopped_early_because,
    )
