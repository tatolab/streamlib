# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A bundle holding a Texture, as a class node's `InputPin` reads it and its `OutputPin` writes it.

Such a bundle travels as one Bundle frame: its small members in the frame's one Records row, and
each Texture by the pool slot it lives in. The native read hands back each member by name, a
small member its stored JSON form and a Texture as it was read, and the pin builds the
frozen dataclass the bundle is written as from them. A write splits the dataclass the node gives
back into its small members' stored forms and its Textures, each a `Texture` the node read or a
texture it acquired from a pool.
"""

from __future__ import annotations

import dataclasses
from typing import Any

from tatolab.runtime._function_node_stored_values import the_stored_value_of, the_value_a_body_takes

__all__ = [
    "holds_a_texture",
    "the_bundle_a_frame_read_builds",
    "the_small_members_and_textures_of",
]

#: The member type a bundle carries by reference, beside its small members.
_TEXTURE_MEMBER_TYPE = "texture"


def holds_a_texture(pin_type: "dict[str, Any]") -> bool:
    """Whether `pin_type` is a bundle holding a Texture, which travels as one Bundle frame."""
    return pin_type["type"] == "bundle" and any(
        _is_a_texture(member) for member in pin_type["members"]
    )


def the_bundle_a_frame_read_builds(
    pin_type: "dict[str, Any]", members_read: "dict[str, Any]", what_it_is: str, bundle_class: type
) -> Any:
    """`bundle_class` built from `members_read`, each member of the bundle `pin_type` by name as
    the native read hands it back: a small member in its Python form, a Texture as it is."""
    values: "dict[str, Any]" = {}
    for member in pin_type["members"]:
        held = members_read[member["name"]]
        values[member["name"]] = (
            held
            if _is_a_texture(member)
            else the_value_a_body_takes(
                member["member_type"], held, f"{what_it_is}'s member `{member['name']}`"
            )
        )
    return bundle_class(**values)


def the_small_members_and_textures_of(
    pin_type: "dict[str, Any]", value: Any, what_it_is: str
) -> "tuple[dict[str, Any], dict[str, Any]]":
    """`value`, the dataclass a node gives for the bundle `pin_type`, as its small members'
    stored JSON forms by name and its Textures by name.

    Refused naming what `value` is when it is no dataclass of exactly the bundle's members, and
    naming the member when a Texture member is neither a `Texture` the node read nor a texture it
    holds.
    """
    member_names = [member["name"] for member in pin_type["members"]]
    if not dataclasses.is_dataclass(value) or isinstance(value, type):
        raise TypeError(
            f"{what_it_is} is a bundle of {', '.join(f'`{name}`' for name in member_names)}, "
            f"and was given {type(value).__name__}"
        )
    field_names = {field.name for field in dataclasses.fields(value)}
    if field_names != set(member_names):
        raise ValueError(
            f"{what_it_is} is a bundle of {', '.join(f'`{name}`' for name in member_names)}, "
            f"and was given a `{type(value).__name__}` of "
            f"{', '.join(f'`{name}`' for name in sorted(field_names))}"
        )
    small_members: "dict[str, Any]" = {}
    textures: "dict[str, Any]" = {}
    for member in pin_type["members"]:
        held = getattr(value, member["name"])
        what_the_member_is = f"{what_it_is}'s member `{member['name']}`"
        if not _is_a_texture(member):
            small_members[member["name"]] = the_stored_value_of(
                member["member_type"], held, what_the_member_is
            )
        elif getattr(held, "surface_id", None) is None:
            raise TypeError(
                f"{what_the_member_is} is a Texture: a `Texture` this node read or a texture "
                f"it acquired from a pool, and was given {type(held).__name__}"
            )
        else:
            textures[member["name"]] = held
    return small_members, textures


def _is_a_texture(member: "dict[str, Any]") -> bool:
    return member["member_type"]["type"] == _TEXTURE_MEMBER_TYPE
